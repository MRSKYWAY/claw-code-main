use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use runtime::{ContentBlock, ConversationMessage, MessageRole, Session as RuntimeSession};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{RunActivity, RunRecord};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptRequest {
    pub prompt: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptResponse {
    pub message: ConversationMessage,
    pub run: RunRecord,
}

pub async fn run_prompt(
    State(state): State<super::AppState>,
    Path(id): Path<super::SessionId>,
    Json(payload): Json<PromptRequest>,
) -> super::ApiResult<Json<PromptResponse>> {
    let prompt = payload.prompt.trim();
    let model = payload.model.trim();
    if prompt.is_empty() {
        return Err(internal_error("prompt must not be empty"));
    }
    if model.is_empty() {
        return Err(internal_error("model must not be empty"));
    }

    let user_message = ConversationMessage::user_text(prompt);
    let (broadcaster, conversation) = {
        let mut sessions = state.sessions.write().await;
        let session = sessions
            .get_mut(&id)
            .ok_or_else(|| super::not_found(format!("session `{id}` not found")))?;
        let conversation = session.conversation.clone();
        session.conversation.messages.push(user_message.clone());
        (session.events.clone(), conversation)
    };
    state
        .persist()
        .await
        .map_err(|error| internal_error(format!("could not save web sessions: {error}")))?;
    let _ = broadcaster.send(super::SessionEvent::Message {
        session_id: id.clone(),
        message: user_message,
    });

    let prompt = prompt.to_string();
    let model = model.to_string();
    let command_prompt = prompt.clone();
    let command_model = model.clone();
    let result = tokio::task::spawn_blocking(move || {
        execute_claw(&command_model, &command_prompt, &conversation)
    })
    .await
    .map_err(|error| internal_error(format!("prompt task failed: {error}")))??;
    let assistant_message = ConversationMessage::assistant(vec![ContentBlock::Text {
        text: result.message,
    }]);
    let run = RunRecord {
        prompt,
        model,
        completed_at: super::unix_timestamp_millis(),
        activities: result.activities,
    };
    {
        let mut sessions = state.sessions.write().await;
        let session = sessions
            .get_mut(&id)
            .ok_or_else(|| super::not_found(format!("session `{id}` not found")))?;
        session
            .conversation
            .messages
            .push(assistant_message.clone());
        session.runs.push(run.clone());
    }
    state
        .persist()
        .await
        .map_err(|error| internal_error(format!("could not save web sessions: {error}")))?;
    let _ = broadcaster.send(super::SessionEvent::Message {
        session_id: id,
        message: assistant_message.clone(),
    });

    Ok(Json(PromptResponse {
        message: assistant_message,
        run,
    }))
}

struct ClawRunResult {
    message: String,
    activities: Vec<RunActivity>,
}

const DEFAULT_WEB_RUN_TIMEOUT: Duration = Duration::from_secs(180);
const AUTO_MODEL: &str = "claw-auto";
const AUTO_MAX_TOTAL_TIME: Duration = Duration::from_secs(135);
const AUTO_SCOUT_MAX_TIME: Duration = Duration::from_secs(30);
const SCOUT_TOOLS: &str = "read_file,glob_search,grep_search";

fn execute_claw(
    model: &str,
    prompt: &str,
    conversation: &RuntimeSession,
) -> Result<ClawRunResult, super::ApiError> {
    let command_prompt = prompt_with_history(prompt, conversation);
    if model == AUTO_MODEL {
        return execute_auto(&command_prompt);
    }

    execute_claw_process(model, &command_prompt, None, 8, web_run_timeout(), "Claw")
}

fn execute_auto(prompt: &str) -> Result<ClawRunResult, super::ApiError> {
    let total_budget = web_run_timeout().min(AUTO_MAX_TOTAL_TIME);
    let scout_timeout = (total_budget / 3).min(AUTO_SCOUT_MAX_TIME);
    let executor_timeout = total_budget.saturating_sub(scout_timeout);
    let scout_model = auto_scout_model();
    let scout_prompt = format!(
        "You are the scout in a two-stage coding task. Inspect only enough of the workspace to hand off the task below. Use at most four tool calls and only the supplied read/search tools. Do not use a shell, web search, agents, edits, tests, or broad recursive investigation. Do not retry failures. Return exactly: relevant files, existing behavior, and the smallest next step.\n\nTask:\n{prompt}"
    );
    let scout = execute_claw_process(
        scout_model,
        &scout_prompt,
        Some(SCOUT_TOOLS),
        4,
        scout_timeout,
        "Claw scout",
    );
    let (scout_report, mut activities) = match scout {
        Ok(result) => (
            truncate(&result.message, 12_000),
            prefix_activities("Scout", result.activities),
        ),
        Err((_, error)) => (
            format!(
                "Scout unavailable; continue without it. Reason: {}",
                error.0.error
            ),
            vec![RunActivity {
                kind: "scout".to_string(),
                label: "Scout unavailable".to_string(),
                detail: truncate(&error.0.error, 500),
                is_error: true,
            }],
        ),
    };
    let executor_prompt = format!(
        "You are the executor in a two-stage coding task. Use the scout report as a narrow map, then complete the user request. Work only on files identified by the scout unless a required dependency forces one additional lookup. Use at most eight tool calls. Do not redo broad repository discovery, do not use web search, do not launch agents, and never retry a failed tool. If the scout is incomplete, make one targeted glob/grep lookup, then proceed. Finish with a direct answer or a concise summary of the change.\n\n<Scout report>\n{scout_report}\n</Scout report>\n\n<User task>\n{prompt}\n</User task>"
    );
    let executor = execute_claw_process(
        auto_executor_model(prompt),
        &executor_prompt,
        Some(executor_tools()),
        8,
        executor_timeout,
        "Claw executor",
    )?;
    activities.extend(prefix_activities("Executor", executor.activities));
    Ok(ClawRunResult {
        message: executor.message,
        activities,
    })
}

fn auto_scout_model() -> &'static str {
    if has_env_key("NVIDIA_API_KEY") {
        "nvidia-fast"
    } else {
        "gemini-flash"
    }
}

fn auto_executor_model(prompt: &str) -> &'static str {
    if has_env_key("NVIDIA_API_KEY") {
        let normalized = prompt.to_ascii_lowercase();
        if [
            "architecture",
            "architect",
            "design a plan",
            "implementation plan",
        ]
        .iter()
        .any(|term| normalized.contains(term))
        {
            "nvidia-plan"
        } else {
            "nvidia-agent"
        }
    } else {
        "gemini-pro"
    }
}

fn has_env_key(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn executor_tools() -> &'static str {
    if cfg!(windows) {
        "read_file,write_file,edit_file,glob_search,grep_search,PowerShell,TodoWrite"
    } else {
        "read_file,write_file,edit_file,glob_search,grep_search,bash,TodoWrite"
    }
}

fn prefix_activities(prefix: &str, activities: Vec<RunActivity>) -> Vec<RunActivity> {
    activities
        .into_iter()
        .map(|mut activity| {
            activity.label = format!("{prefix} · {}", activity.label);
            activity
        })
        .collect()
}

fn execute_claw_process(
    model: &str,
    prompt: &str,
    allowed_tools: Option<&str>,
    max_tool_iterations: usize,
    timeout: Duration,
    stage: &str,
) -> Result<ClawRunResult, super::ApiError> {
    let binary = std::env::var("CLAW_BIN").unwrap_or_else(|_| "claw".to_string());
    let prompt_file = write_prompt_file(prompt)?;
    let mut command = Command::new(&binary);
    command
        .args(["--model", model, "--output-format", "json", "--prompt-file"])
        .arg(&prompt_file)
        .env("CLAW_MAX_TOOL_ITERATIONS", max_tool_iterations.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(allowed_tools) = allowed_tools {
        command.args(["--allowed-tools", allowed_tools]);
    }
    let mut child = command
        .spawn()
        .map_err(|error| {
            internal_error(format!(
                "could not start `{binary}`: {error}. Install Claw or set CLAW_BIN to its executable path"
            ))
        })?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(200)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&prompt_file);
                return Err(internal_error(format!(
                    "{stage} did not finish within {} seconds. It was stopped before it could keep consuming tools.",
                    timeout.as_secs()
                )));
            }
            Err(error) => {
                let _ = std::fs::remove_file(&prompt_file);
                return Err(internal_error(format!("could not monitor Claw: {error}")));
            }
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|error| internal_error(format!("could not collect Claw output: {error}")));
    let _ = std::fs::remove_file(&prompt_file);
    let output = output?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(internal_error(if stderr.is_empty() {
            stdout
        } else {
            stderr
        }));
    }

    let response: CliPromptResponse = serde_json::from_str(&stdout).map_err(|error| {
        internal_error(format!(
            "Claw returned invalid structured output: {error}. Ensure CLAW_BIN points to the current Claw executable"
        ))
    })?;
    Ok(ClawRunResult {
        message: response.message.trim().to_string(),
        activities: response.into_activities(),
    })
}

fn web_run_timeout() -> Duration {
    web_run_timeout_from_env(std::env::var("CLAW_WEB_RUN_TIMEOUT_SECS").ok().as_deref())
}

fn web_run_timeout_from_env(value: Option<&str>) -> Duration {
    value
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (1..=900).contains(seconds))
        .map_or(DEFAULT_WEB_RUN_TIMEOUT, Duration::from_secs)
}

fn write_prompt_file(prompt: &str) -> Result<std::path::PathBuf, super::ApiError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| internal_error(format!("clock error: {error}")))?
        .as_nanos();
    let path = std::env::temp_dir().join(format!("claw-web-{nonce}.prompt"));
    std::fs::write(&path, prompt)
        .map_err(|error| internal_error(format!("could not write web prompt file: {error}")))?;
    Ok(path)
}

fn prompt_with_history(prompt: &str, conversation: &RuntimeSession) -> String {
    const MAX_HISTORY_CHARS: usize = 80_000;
    let workspace = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let mut history = String::new();
    for message in &conversation.messages {
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        for block in &message.blocks {
            let text = match block {
                ContentBlock::Text { text } => text.as_str(),
                ContentBlock::ToolUse { name, input, .. } => {
                    history.push_str(&format!("{role} tool call {name}: {input}\n"));
                    continue;
                }
                ContentBlock::ToolResult {
                    tool_name, output, ..
                } => {
                    history.push_str(&format!("tool result {tool_name}: {output}\n"));
                    continue;
                }
            };
            history.push_str(role);
            history.push_str(": ");
            history.push_str(text);
            history.push('\n');
        }
    }
    let start = history.len().saturating_sub(MAX_HISTORY_CHARS);
    let history = &history[start..];
    format!(
        "Continue this persisted Claw conversation. Do not repeat the transcript.\n\nWorkspace root: {workspace}\nTreat it as the root for all relative paths; do not guess nested project directories. Start repository discovery with glob_search, grep_search, and read_file. Use only tools that are exposed to you. Do not retry an unavailable tool or repeat an equivalent search after it has failed. For genuinely independent work, you may delegate up to two focused Agent tasks (Explorer for discovery, Architect for design, Coder for changes, Reviewer for review); do not delegate simple directory discovery.\n\n<conversation>\n{history}</conversation>\n\nCurrent request:\n{prompt}"
    )
}

#[derive(Debug, Deserialize)]
struct CliPromptResponse {
    #[serde(default)]
    message: String,
    #[serde(default)]
    tool_uses: Vec<CliToolUse>,
    #[serde(default)]
    tool_results: Vec<CliToolResult>,
}

impl CliPromptResponse {
    fn into_activities(self) -> Vec<RunActivity> {
        let mut activities = self
            .tool_uses
            .into_iter()
            .map(|tool| RunActivity {
                kind: "tool_call".to_string(),
                label: tool.name,
                detail: summarize_input(&tool.input),
                is_error: false,
            })
            .collect::<Vec<_>>();
        activities.extend(self.tool_results.into_iter().map(|tool| RunActivity {
            kind: "tool_result".to_string(),
            label: tool.tool_name,
            detail: truncate(&tool.output, 500),
            is_error: tool.is_error,
        }));
        activities
    }
}

#[derive(Debug, Deserialize)]
struct CliToolUse {
    name: String,
    #[serde(default)]
    input: Value,
}

#[derive(Debug, Deserialize)]
struct CliToolResult {
    tool_name: String,
    #[serde(default)]
    output: String,
    #[serde(default)]
    is_error: bool,
}

fn summarize_input(value: &Value) -> String {
    let raw = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_string);
    let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::String(raw.clone()));
    let detail = parsed
        .get("command")
        .or_else(|| parsed.get("path"))
        .or_else(|| parsed.get("pattern"))
        .or_else(|| parsed.get("query"))
        .and_then(Value::as_str)
        .unwrap_or(&raw);
    truncate(detail, 500)
}

fn truncate(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn internal_error(message: impl Into<String>) -> super::ApiError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(super::ErrorResponse {
            error: message.into(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        prompt_with_history, web_run_timeout_from_env, CliPromptResponse, DEFAULT_WEB_RUN_TIMEOUT,
    };
    use runtime::{ConversationMessage, Session};

    #[test]
    fn separates_tool_activity_from_the_final_message() {
        let response: CliPromptResponse = serde_json::from_str(
            r#"{
                "message":"Repository has two crates.",
                "tool_uses":[{"name":"bash","input":"{\"command\":\"rg --files\"}"}],
                "tool_results":[{"tool_name":"bash","output":"Cargo.toml","is_error":false}]
            }"#,
        )
        .expect("fixture should parse");

        assert_eq!(response.message, "Repository has two crates.");
        let activities = response.into_activities();
        assert_eq!(activities.len(), 2);
        assert_eq!(activities[0].label, "bash");
        assert_eq!(activities[0].detail, "rg --files");
        assert_eq!(activities[1].detail, "Cargo.toml");
    }

    #[test]
    fn includes_persisted_messages_in_follow_up_prompt() {
        let mut session = Session::new();
        session
            .messages
            .push(ConversationMessage::user_text("Remember Rust"));
        let prompt = prompt_with_history("What did I ask?", &session);
        assert!(prompt.contains("user: Remember Rust"));
        assert!(prompt.ends_with("Current request:\nWhat did I ask?"));
    }

    #[test]
    fn uses_a_safe_default_timeout_when_not_configured() {
        assert_eq!(web_run_timeout_from_env(None), DEFAULT_WEB_RUN_TIMEOUT);
        assert_eq!(
            web_run_timeout_from_env(Some("45")),
            std::time::Duration::from_secs(45)
        );
        assert_eq!(web_run_timeout_from_env(Some("0")), DEFAULT_WEB_RUN_TIMEOUT);
    }
}
