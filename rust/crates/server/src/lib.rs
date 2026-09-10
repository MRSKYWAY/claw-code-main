mod web_runtime;

use std::collections::HashMap;
use std::convert::Infallible;
use std::fs;
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_stream::stream;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use runtime::{ConversationMessage, Session as RuntimeSession};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

pub type SessionId = String;
pub type SessionStore = Arc<RwLock<HashMap<SessionId, Session>>>;

const BROADCAST_CAPACITY: usize = 64;

#[derive(Clone)]
pub struct AppState {
    sessions: SessionStore,
    next_session_id: Arc<AtomicU64>,
    storage_path: Arc<PathBuf>,
}

impl AppState {
    #[must_use]
    pub fn new() -> Self {
        Self::with_storage_path(default_storage_path())
    }

    fn with_storage_path(storage_path: PathBuf) -> Self {
        let persisted = load_store(&storage_path).unwrap_or_else(|error| {
            eprintln!(
                "Claw web could not load {}: {error}",
                storage_path.display()
            );
            PersistedStore::default()
        });
        let next_session_id = persisted.next_session_id.max(1);
        let sessions = persisted
            .sessions
            .into_iter()
            .map(|session| {
                let id = session.id.clone();
                (id, Session::from_persisted(session))
            })
            .collect();

        Self {
            sessions: Arc::new(RwLock::new(sessions)),
            next_session_id: Arc::new(AtomicU64::new(next_session_id)),
            storage_path: Arc::new(storage_path),
        }
    }

    fn allocate_session_id(&self) -> SessionId {
        let id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        format!("session-{id}")
    }

    async fn persist(&self) -> Result<(), String> {
        let sessions = self.sessions.read().await;
        let store = PersistedStore {
            version: 1,
            next_session_id: self.next_session_id.load(Ordering::Relaxed),
            sessions: sessions.values().map(Session::to_persisted).collect(),
        };
        let encoded = serde_json::to_vec_pretty(&store).map_err(|error| error.to_string())?;
        let parent = self
            .storage_path
            .parent()
            .ok_or_else(|| "web session store has no parent directory".to_string())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let temporary = self.storage_path.with_extension("json.tmp");
        fs::write(&temporary, encoded).map_err(|error| error.to_string())?;
        fs::rename(&temporary, &*self.storage_path).map_err(|error| error.to_string())
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct Session {
    pub id: SessionId,
    pub created_at: u64,
    pub conversation: RuntimeSession,
    pub runs: Vec<RunRecord>,
    events: broadcast::Sender<SessionEvent>,
}

impl Session {
    fn new(id: SessionId) -> Self {
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            id,
            created_at: unix_timestamp_millis(),
            conversation: RuntimeSession::new(),
            runs: Vec::new(),
            events,
        }
    }

    fn from_persisted(session: PersistedSession) -> Self {
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            id: session.id,
            created_at: session.created_at,
            conversation: session.conversation,
            runs: session.runs,
            events,
        }
    }

    fn to_persisted(&self) -> PersistedSession {
        PersistedSession {
            id: self.id.clone(),
            created_at: self.created_at,
            conversation: self.conversation.clone(),
            runs: self.runs.clone(),
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunActivity {
    pub kind: String,
    pub label: String,
    pub detail: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRecord {
    pub prompt: String,
    pub model: String,
    pub completed_at: u64,
    pub activities: Vec<RunActivity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSession {
    id: SessionId,
    created_at: u64,
    conversation: RuntimeSession,
    #[serde(default)]
    runs: Vec<RunRecord>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedStore {
    #[serde(default)]
    version: u32,
    #[serde(default = "default_next_session_id")]
    next_session_id: u64,
    #[serde(default)]
    sessions: Vec<PersistedSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SessionEvent {
    Snapshot {
        session_id: SessionId,
        session: RuntimeSession,
    },
    Message {
        session_id: SessionId,
        message: ConversationMessage,
    },
}

impl SessionEvent {
    fn event_name(&self) -> &'static str {
        match self {
            Self::Snapshot { .. } => "snapshot",
            Self::Message { .. } => "message",
        }
    }

    fn to_sse_event(&self) -> Result<Event, serde_json::Error> {
        Ok(Event::default()
            .event(self.event_name())
            .data(serde_json::to_string(self)?))
    }
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

type ApiError = (StatusCode, Json<ErrorResponse>);
type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateSessionResponse {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: SessionId,
    pub created_at: u64,
    pub message_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListSessionsResponse {
    pub sessions: Vec<SessionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionDetailsResponse {
    pub id: SessionId,
    pub created_at: u64,
    pub session: RuntimeSession,
    pub runs: Vec<RunRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendMessageRequest {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSummary {
    #[serde(rename = "agentId")]
    pub agent_id: String,
    pub name: String,
    pub description: String,
    #[serde(rename = "subagentType")]
    pub subagent_type: Option<String>,
    pub model: Option<String>,
    pub status: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "completedAt")]
    pub completed_at: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListAgentsResponse {
    pub agents: Vec<AgentSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSummary {
    pub alias: String,
    pub model: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListModelsResponse {
    pub models: Vec<ModelSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStatusResponse {
    pub status: String,
    pub session_count: usize,
    pub message_count: usize,
    pub model_count: usize,
    pub agent_count: usize,
}

#[must_use]
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(web_ui))
        .route("/sessions", post(create_session).get(list_sessions))
        .route("/models", get(list_models))
        .route("/agents", get(list_agents))
        .route("/status", get(runtime_status))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/events", get(stream_session_events))
        .route("/sessions/{id}/message", post(send_message))
        .route("/sessions/{id}/prompt", post(web_runtime::run_prompt))
        .with_state(state)
}

async fn web_ui() -> Html<&'static str> {
    Html(include_str!("web.html"))
}

async fn list_models() -> Json<ListModelsResponse> {
    let mut models = vec![ModelSummary {
        alias: "claw-auto".to_string(),
        model: "scout + executor".to_string(),
        label: "Claw Auto · Scout + Agent".to_string(),
    }];
    models.extend(
        api::MODEL_CATALOG
            .iter()
            .filter(|entry| !entry.alias.starts_with("claude-"))
            .map(|entry| ModelSummary {
                alias: entry.alias.to_string(),
                model: entry.model.to_string(),
                label: entry.label.to_string(),
            }),
    );
    Json(ListModelsResponse { models })
}

async fn list_agents() -> Json<ListAgentsResponse> {
    let mut agents = agent_store_dirs()
        .into_iter()
        .filter_map(|directory| fs::read_dir(directory).ok())
        .flat_map(|entries| entries.flatten())
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|extension| extension.to_str()) == Some("json"))
                .then(|| fs::read(path).ok())
                .flatten()
                .and_then(|contents| serde_json::from_slice::<AgentSummary>(&contents).ok())
        })
        .collect::<Vec<_>>();
    agents.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    agents.dedup_by(|left, right| left.agent_id == right.agent_id);
    Json(ListAgentsResponse { agents })
}

async fn runtime_status(State(state): State<AppState>) -> Json<RuntimeStatusResponse> {
    let sessions = state.sessions.read().await;
    let session_count = sessions.len();
    let message_count = sessions
        .values()
        .map(|session| session.conversation.messages.len())
        .sum();
    drop(sessions);

    let model_count = list_models().await.0.models.len();
    let agent_count = list_agents().await.0.agents.len();

    Json(RuntimeStatusResponse {
        status: "ok".to_string(),
        session_count,
        message_count,
        model_count,
        agent_count,
    })
}

fn agent_store_dirs() -> Vec<PathBuf> {
    if let Some(path) = std::env::var_os("CLAW_AGENT_STORE") {
        return vec![PathBuf::from(path)];
    }
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    let mut candidates = cwd
        .ancestors()
        .map(|directory| directory.join(".claw-agents"))
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    candidates
}

async fn create_session(
    State(state): State<AppState>,
) -> ApiResult<(StatusCode, Json<CreateSessionResponse>)> {
    let session_id = state.allocate_session_id();
    let session = Session::new(session_id.clone());

    state
        .sessions
        .write()
        .await
        .insert(session_id.clone(), session);
    state
        .persist()
        .await
        .map_err(|error| internal_error(format!("could not save web sessions: {error}")))?;

    Ok((
        StatusCode::CREATED,
        Json(CreateSessionResponse { session_id }),
    ))
}

async fn list_sessions(State(state): State<AppState>) -> Json<ListSessionsResponse> {
    let sessions = state.sessions.read().await;
    let mut summaries = sessions
        .values()
        .map(|session| SessionSummary {
            id: session.id.clone(),
            created_at: session.created_at,
            message_count: session.conversation.messages.len(),
        })
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| left.id.cmp(&right.id));

    Json(ListSessionsResponse {
        sessions: summaries,
    })
}

async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<SessionId>,
) -> ApiResult<Json<SessionDetailsResponse>> {
    let sessions = state.sessions.read().await;
    let session = sessions
        .get(&id)
        .ok_or_else(|| not_found(format!("session `{id}` not found")))?;

    Ok(Json(SessionDetailsResponse {
        id: session.id.clone(),
        created_at: session.created_at,
        session: session.conversation.clone(),
        runs: session.runs.clone(),
    }))
}

async fn send_message(
    State(state): State<AppState>,
    Path(id): Path<SessionId>,
    Json(payload): Json<SendMessageRequest>,
) -> ApiResult<StatusCode> {
    let message = ConversationMessage::user_text(payload.message);
    let broadcaster = {
        let mut sessions = state.sessions.write().await;
        let session = sessions
            .get_mut(&id)
            .ok_or_else(|| not_found(format!("session `{id}` not found")))?;
        session.conversation.messages.push(message.clone());
        session.events.clone()
    };

    let _ = broadcaster.send(SessionEvent::Message {
        session_id: id,
        message,
    });
    state
        .persist()
        .await
        .map_err(|error| internal_error(format!("could not save web sessions: {error}")))?;

    Ok(StatusCode::NO_CONTENT)
}

async fn stream_session_events(
    State(state): State<AppState>,
    Path(id): Path<SessionId>,
) -> ApiResult<impl IntoResponse> {
    let (snapshot, mut receiver) = {
        let sessions = state.sessions.read().await;
        let session = sessions
            .get(&id)
            .ok_or_else(|| not_found(format!("session `{id}` not found")))?;
        (
            SessionEvent::Snapshot {
                session_id: session.id.clone(),
                session: session.conversation.clone(),
            },
            session.subscribe(),
        )
    };

    let stream = stream! {
        if let Ok(event) = snapshot.to_sse_event() {
            yield Ok::<Event, Infallible>(event);
        }

        loop {
            match receiver.recv().await {
                Ok(event) => {
                    if let Ok(sse_event) = event.to_sse_event() {
                        yield Ok::<Event, Infallible>(sse_event);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

fn unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_millis() as u64
}

fn not_found(message: String) -> ApiError {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse { error: message }),
    )
}

fn internal_error(message: impl Into<String>) -> ApiError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
}

fn default_next_session_id() -> u64 {
    1
}

fn default_storage_path() -> PathBuf {
    if let Some(path) = std::env::var_os("CLAW_WEB_STORE") {
        return PathBuf::from(path);
    }
    let config_root = std::env::var_os("CLAW_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|value| PathBuf::from(value).join(".claw")))
        .or_else(|| std::env::var_os("USERPROFILE").map(|value| PathBuf::from(value).join(".claw")))
        .unwrap_or_else(|| PathBuf::from(".claw"));
    config_root.join("web-sessions.json")
}

fn load_store(path: &FsPath) -> Result<PersistedStore, String> {
    if !path.exists() {
        return Ok(PersistedStore::default());
    }
    let contents = fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&contents).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        app, AppState, CreateSessionResponse, ListAgentsResponse, ListModelsResponse,
        ListSessionsResponse, RuntimeStatusResponse, Session, SessionDetailsResponse,
    };
    use reqwest::Client;
    use std::fs;
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;
    use tokio::time::timeout;

    static NEXT_TEST_STORE: AtomicU64 = AtomicU64::new(1);

    struct TestServer {
        address: SocketAddr,
        handle: JoinHandle<()>,
        store_path: PathBuf,
    }

    impl TestServer {
        async fn spawn() -> Self {
            let store_path = test_store_path();
            let server_store_path = store_path.clone();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("test listener should bind");
            let address = listener
                .local_addr()
                .expect("listener should report local address");
            let handle = tokio::spawn(async move {
                axum::serve(
                    listener,
                    app(AppState::with_storage_path(server_store_path)),
                )
                .await
                .expect("server should run");
            });

            Self {
                address,
                handle,
                store_path,
            }
        }

        fn url(&self, path: &str) -> String {
            format!("http://{}{}", self.address, path)
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.handle.abort();
            let _ = fs::remove_file(&self.store_path);
        }
    }

    fn test_store_path() -> PathBuf {
        let sequence = NEXT_TEST_STORE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "claw-web-test-{}-{sequence}.json",
            std::process::id()
        ))
    }

    async fn create_session(client: &Client, server: &TestServer) -> CreateSessionResponse {
        client
            .post(server.url("/sessions"))
            .send()
            .await
            .expect("create request should succeed")
            .error_for_status()
            .expect("create request should return success")
            .json::<CreateSessionResponse>()
            .await
            .expect("create response should parse")
    }

    async fn next_sse_frame(response: &mut reqwest::Response, buffer: &mut String) -> String {
        loop {
            if let Some(index) = buffer.find("\n\n") {
                let frame = buffer[..index].to_string();
                let remainder = buffer[index + 2..].to_string();
                *buffer = remainder;
                return frame;
            }

            let next_chunk = timeout(Duration::from_secs(5), response.chunk())
                .await
                .expect("SSE stream should yield within timeout")
                .expect("SSE stream should remain readable")
                .expect("SSE stream should stay open");
            buffer.push_str(&String::from_utf8_lossy(&next_chunk));
        }
    }

    #[tokio::test]
    async fn creates_and_lists_sessions() {
        let server = TestServer::spawn().await;
        let client = Client::new();

        // given
        let created = create_session(&client, &server).await;

        // when
        let sessions = client
            .get(server.url("/sessions"))
            .send()
            .await
            .expect("list request should succeed")
            .error_for_status()
            .expect("list request should return success")
            .json::<ListSessionsResponse>()
            .await
            .expect("list response should parse");
        let details = client
            .get(server.url(&format!("/sessions/{}", created.session_id)))
            .send()
            .await
            .expect("details request should succeed")
            .error_for_status()
            .expect("details request should return success")
            .json::<SessionDetailsResponse>()
            .await
            .expect("details response should parse");

        // then
        assert_eq!(created.session_id, "session-1");
        assert_eq!(sessions.sessions.len(), 1);
        assert_eq!(sessions.sessions[0].id, created.session_id);
        assert_eq!(sessions.sessions[0].message_count, 0);
        assert_eq!(details.id, "session-1");
        assert!(details.session.messages.is_empty());
    }

    #[tokio::test]
    async fn serves_the_local_web_ui() {
        let server = TestServer::spawn().await;

        let page = Client::new()
            .get(server.url("/"))
            .send()
            .await
            .expect("web UI request should succeed")
            .error_for_status()
            .expect("web UI request should return success")
            .text()
            .await
            .expect("web UI response should be readable");

        assert!(page.contains("claw code"));
        assert!(page.contains("/sessions/"));
    }

    #[tokio::test]
    async fn serves_the_shared_model_catalog() {
        let server = TestServer::spawn().await;
        let models = Client::new()
            .get(server.url("/models"))
            .send()
            .await
            .expect("models request should succeed")
            .error_for_status()
            .expect("models request should return success")
            .json::<ListModelsResponse>()
            .await
            .expect("models response should parse");

        assert!(models
            .models
            .iter()
            .any(|model| { model.alias == "nvidia-agent" && model.model == "z-ai/glm-5.2" }));
        assert!(models
            .models
            .iter()
            .any(|model| { model.alias == "gemini-flash" && model.model == "gemini-3.7-flash" }));
    }

    #[tokio::test]
    async fn serves_runtime_status_from_live_state() {
        let server = TestServer::spawn().await;
        let client = Client::new();
        let _created = create_session(&client, &server).await;
        let agents = client
            .get(server.url("/agents"))
            .send()
            .await
            .expect("agents request should succeed")
            .error_for_status()
            .expect("agents request should return success")
            .json::<ListAgentsResponse>()
            .await
            .expect("agents response should parse");

        let status = client
            .get(server.url("/status"))
            .send()
            .await
            .expect("status request should succeed")
            .error_for_status()
            .expect("status request should return success")
            .json::<RuntimeStatusResponse>()
            .await
            .expect("status response should parse");

        assert_eq!(status.status, "ok");
        assert_eq!(status.session_count, 1);
        assert_eq!(status.message_count, 0);
        assert!(status.model_count > 0);
        assert_eq!(status.agent_count, agents.agents.len());
    }

    #[tokio::test]
    async fn reloads_sessions_from_the_local_store() {
        let store_path = test_store_path();
        let state = AppState::with_storage_path(store_path.clone());
        let session_id = state.allocate_session_id();
        let mut session = Session::new(session_id.clone());
        session
            .conversation
            .messages
            .push(runtime::ConversationMessage::user_text("persist this"));
        state.sessions.write().await.insert(session_id, session);
        state.persist().await.expect("store should save");

        let restored = AppState::with_storage_path(store_path.clone());
        let sessions = restored.sessions.read().await;
        let restored_session = sessions.get("session-1").expect("session should reload");
        assert_eq!(restored_session.conversation.messages.len(), 1);
        drop(sessions);
        let _ = fs::remove_file(store_path);
    }

    #[tokio::test]
    async fn streams_message_events_and_persists_message_flow() {
        let server = TestServer::spawn().await;
        let client = Client::new();

        // given
        let created = create_session(&client, &server).await;
        let mut response = client
            .get(server.url(&format!("/sessions/{}/events", created.session_id)))
            .send()
            .await
            .expect("events request should succeed")
            .error_for_status()
            .expect("events request should return success");
        let mut buffer = String::new();
        let snapshot_frame = next_sse_frame(&mut response, &mut buffer).await;

        // when
        let send_status = client
            .post(server.url(&format!("/sessions/{}/message", created.session_id)))
            .json(&super::SendMessageRequest {
                message: "hello from test".to_string(),
            })
            .send()
            .await
            .expect("message request should succeed")
            .status();
        let message_frame = next_sse_frame(&mut response, &mut buffer).await;
        let details = client
            .get(server.url(&format!("/sessions/{}", created.session_id)))
            .send()
            .await
            .expect("details request should succeed")
            .error_for_status()
            .expect("details request should return success")
            .json::<SessionDetailsResponse>()
            .await
            .expect("details response should parse");

        // then
        assert_eq!(send_status, reqwest::StatusCode::NO_CONTENT);
        assert!(snapshot_frame.contains("event: snapshot"));
        assert!(snapshot_frame.contains("\"session_id\":\"session-1\""));
        assert!(message_frame.contains("event: message"));
        assert!(message_frame.contains("hello from test"));
        assert_eq!(details.session.messages.len(), 1);
        assert_eq!(
            details.session.messages[0],
            runtime::ConversationMessage::user_text("hello from test")
        );
    }
}
