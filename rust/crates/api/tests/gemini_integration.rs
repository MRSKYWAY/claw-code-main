use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::Arc;
use std::sync::{Mutex as StdMutex, OnceLock};

use api::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    GeminiClient, InputContentBlock, InputMessage, MessageRequest, OutputContentBlock,
    ProviderClient, StreamEvent, ToolChoice, ToolDefinition, ToolResultContentBlock,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[tokio::test]
async fn send_message_uses_gemini_endpoint_and_maps_tools_history_and_system_prompt() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let body = concat!(
        "{",
        "\"responseId\":\"gemini_resp_test\",",
        "\"modelVersion\":\"gemini-3.7-flash\",",
        "\"candidates\":[{",
        "\"content\":{\"role\":\"model\",\"parts\":[",
        "{\"text\":\"Hello from Gemini\"},",
        "{\"functionCall\":{\"name\":\"weather\",\"args\":{\"city\":\"Paris\"}}}",
        "]},",
        "\"finishReason\":\"STOP\"",
        "}],",
        "\"usageMetadata\":{\"promptTokenCount\":11,\"candidatesTokenCount\":5}",
        "}"
    );
    let server = spawn_server(
        state.clone(),
        vec![http_response("200 OK", "application/json", body)],
    )
    .await;

    let client = GeminiClient::new("gemini-test-key").with_base_url(server.base_url());
    let response = client
        .send_message(&sample_request(false))
        .await
        .expect("request should succeed");

    assert_eq!(response.model, "gemini-3.7-flash");
    assert_eq!(response.total_tokens(), 16);
    assert_eq!(
        response.content,
        vec![
            OutputContentBlock::Text {
                text: "Hello from Gemini".to_string(),
            },
            OutputContentBlock::ToolUse {
                id: "tool_call_2".to_string(),
                name: "weather".to_string(),
                input: json!({ "city": "Paris" }),
            },
        ]
    );

    let captured = state.lock().await;
    let request = captured.first().expect("server should capture request");
    assert_eq!(request.path, "/models/gemini-3.7-flash:generateContent");
    assert_eq!(
        request.headers.get("x-goog-api-key").map(String::as_str),
        Some("gemini-test-key")
    );
    let body: serde_json::Value = serde_json::from_str(&request.body).expect("json body");
    assert_eq!(
        body["systemInstruction"]["parts"][0]["text"],
        json!("Use tools when needed")
    );
    assert_eq!(
        body["tools"][0]["functionDeclarations"][0]["name"],
        json!("weather")
    );
    assert_eq!(
        body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["type"],
        json!("object")
    );
    assert_eq!(
        body["toolConfig"]["functionCallingConfig"]["mode"],
        json!("ANY")
    );
    assert_eq!(
        body["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"][0],
        json!("weather")
    );
    assert_eq!(body["contents"][1]["role"], json!("model"));
    assert_eq!(
        body["contents"][1]["parts"][0]["functionCall"]["name"],
        json!("weather")
    );
    assert_eq!(body["contents"][2]["role"], json!("user"));
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["name"],
        json!("weather")
    );
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["response"]["temp_c"],
        json!(20)
    );
}

#[tokio::test]
async fn stream_message_synthesizes_text_and_tool_events() {
    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let body = concat!(
        "{",
        "\"responseId\":\"gemini_resp_stream\",",
        "\"modelVersion\":\"gemini-3.7-flash\",",
        "\"candidates\":[{",
        "\"content\":{\"role\":\"model\",\"parts\":[",
        "{\"text\":\"Hello\"},",
        "{\"functionCall\":{\"name\":\"weather\",\"args\":{\"city\":\"Paris\"}}}",
        "]},",
        "\"finishReason\":\"STOP\"",
        "}],",
        "\"usageMetadata\":{\"promptTokenCount\":9,\"candidatesTokenCount\":4}",
        "}"
    );
    let server = spawn_server(
        state.clone(),
        vec![http_response_with_headers(
            "200 OK",
            "application/json",
            body,
            &[("x-request-id", "req_gemini_stream")],
        )],
    )
    .await;

    let client = GeminiClient::new("gemini-test-key").with_base_url(server.base_url());
    let mut stream = client
        .stream_message(&sample_request(true))
        .await
        .expect("stream should start");

    assert_eq!(stream.request_id(), Some("req_gemini_stream"));

    let mut events = Vec::new();
    while let Some(event) = stream.next_event().await.expect("event should parse") {
        events.push(event);
    }

    assert!(matches!(events[0], StreamEvent::MessageStart(_)));
    assert!(matches!(
        events[1],
        StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            index: 0,
            content_block: OutputContentBlock::Text { .. },
        })
    ));
    assert!(matches!(
        events[2],
        StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            index: 0,
            delta: ContentBlockDelta::TextDelta { .. },
        })
    ));
    assert!(matches!(
        events[3],
        StreamEvent::ContentBlockStop(ContentBlockStopEvent { index: 0 })
    ));
    assert!(matches!(
        events[4],
        StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            index: 1,
            content_block: OutputContentBlock::ToolUse { .. },
        })
    ));
    assert!(matches!(
        events[5],
        StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            index: 1,
            delta: ContentBlockDelta::InputJsonDelta { .. },
        })
    ));
    assert!(matches!(
        events[6],
        StreamEvent::ContentBlockStop(ContentBlockStopEvent { index: 1 })
    ));
    assert!(matches!(events[7], StreamEvent::MessageDelta(_)));
    assert!(matches!(events[8], StreamEvent::MessageStop(_)));
}

#[tokio::test]
async fn provider_client_dispatches_gemini_requests_from_env() {
    let _lock = env_lock();
    let _api_key = ScopedEnvVar::set("GEMINI_API_KEY", "gemini-test-key");

    let state = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let server = spawn_server(
        state.clone(),
        vec![http_response(
            "200 OK",
            "application/json",
            "{\"responseId\":\"gemini_provider\",\"modelVersion\":\"gemini-3.7-flash\",\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Through provider client\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":9,\"candidatesTokenCount\":4}}",
        )],
    )
    .await;
    let _base_url = ScopedEnvVar::set("GEMINI_BASE_URL", server.base_url());

    let client =
        ProviderClient::from_model("gemini-flash").expect("Gemini provider should be constructed");
    assert!(matches!(client, ProviderClient::Gemini(_)));

    let response = client
        .send_message(&sample_request(false))
        .await
        .expect("provider-dispatched request should succeed");

    assert_eq!(response.total_tokens(), 13);

    let captured = state.lock().await;
    let request = captured.first().expect("captured request");
    assert_eq!(request.path, "/models/gemini-3.7-flash:generateContent");
    assert_eq!(
        request.headers.get("x-goog-api-key").map(String::as_str),
        Some("gemini-test-key")
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedRequest {
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

struct TestServer {
    base_url: String,
    join_handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
    fn base_url(&self) -> String {
        self.base_url.clone()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.join_handle.abort();
    }
}

async fn spawn_server(
    state: Arc<Mutex<Vec<CapturedRequest>>>,
    responses: Vec<String>,
) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let address = listener.local_addr().expect("listener addr");
    let join_handle = tokio::spawn(async move {
        for response in responses {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buffer = Vec::new();
            let mut header_end = None;
            loop {
                let mut chunk = [0_u8; 1024];
                let read = socket.read(&mut chunk).await.expect("read request");
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(position) = find_header_end(&buffer) {
                    header_end = Some(position);
                    break;
                }
            }

            let header_end = header_end.expect("headers should exist");
            let (header_bytes, remaining) = buffer.split_at(header_end);
            let header_text = String::from_utf8(header_bytes.to_vec()).expect("utf8 headers");
            let mut lines = header_text.split("\r\n");
            let request_line = lines.next().expect("request line");
            let path = request_line
                .split_whitespace()
                .nth(1)
                .expect("path")
                .to_string();
            let mut headers = HashMap::new();
            let mut content_length = 0_usize;
            for line in lines {
                if line.is_empty() {
                    continue;
                }
                let (name, value) = line.split_once(':').expect("header");
                let value = value.trim().to_string();
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.parse().expect("content length");
                }
                headers.insert(name.to_ascii_lowercase(), value);
            }

            let mut body = remaining[4..].to_vec();
            while body.len() < content_length {
                let mut chunk = vec![0_u8; content_length - body.len()];
                let read = socket.read(&mut chunk).await.expect("read body");
                if read == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..read]);
            }

            state.lock().await.push(CapturedRequest {
                path,
                headers,
                body: String::from_utf8(body).expect("utf8 body"),
            });

            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }
    });

    TestServer {
        base_url: format!("http://{address}"),
        join_handle,
    }
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn http_response(status: &str, content_type: &str, body: &str) -> String {
    http_response_with_headers(status, content_type, body, &[])
}

fn http_response_with_headers(
    status: &str,
    content_type: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> String {
    let mut extra_headers = String::new();
    for (name, value) in headers {
        use std::fmt::Write as _;
        write!(&mut extra_headers, "{name}: {value}\r\n").expect("header write");
    }
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\n{extra_headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn sample_request(stream: bool) -> MessageRequest {
    MessageRequest {
        model: "gemini-3.7-flash".to_string(),
        max_tokens: 64,
        messages: vec![
            InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: "What is the weather in Paris?".to_string(),
                }],
            },
            InputMessage {
                role: "assistant".to_string(),
                content: vec![InputContentBlock::ToolUse {
                    id: "tool_1".to_string(),
                    name: "weather".to_string(),
                    input: json!({ "city": "Paris" }),
                }],
            },
            InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::ToolResult {
                    tool_use_id: "tool_1".to_string(),
                    content: vec![ToolResultContentBlock::Json {
                        value: json!({ "temp_c": 20 }),
                    }],
                    is_error: false,
                }],
            },
        ],
        system: Some("Use tools when needed".to_string()),
        tools: Some(vec![ToolDefinition {
            name: "weather".to_string(),
            description: Some("Fetches weather".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }),
        }]),
        tool_choice: Some(ToolChoice::Tool {
            name: "weather".to_string(),
        }),
        stream,
    }
}

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| StdMutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct ScopedEnvVar {
    key: &'static str,
    previous: Option<OsString>,
}

impl ScopedEnvVar {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}
