use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use runtime::{ContentBlock, ConversationMessage, MessageRole, Session as RuntimeSession};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

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
    let command_session_id = id.clone();
    let command_broadcaster = broadcaster.clone();
    let result = tokio::task::spawn_blocking(move || {
        execute_claw(
            &command_model,
            &command_prompt,
            &conversation,
            &command_session_id,
            &command_broadcaster,
        )
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

const DEFAULT_WEB_RUN_TIMEOUT: Duration = Duration::from_secs(900);
const AUTO_MODEL: &str = "claw-auto";
const AUTO_MAX_TOTAL_TIME: Duration = Duration::from_secs(900);
const AUTO_SCOUT_MAX_TIME: Duration = Duration::from_secs(900);
const LIVE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const SCOUT_TOOLS: &str = "read_file,glob_search,grep_search";

type SessionBroadcaster = broadcast::Sender<super::SessionEvent>;

enum ChildOutput {
    Stdout(String),
    Stderr(String),
}

fn execute_claw(
    model: &str,
    prompt: &str,
    conversation: &RuntimeSession,
    session_id: &str,
    broadcaster: &SessionBroadcaster,
) -> Result<ClawRunResult, super::ApiError> {
    let command_prompt = prompt_with_history(prompt, conversation);
    if model == AUTO_MODEL {
        return execute_auto(&command_prompt, session_id, broadcaster);
    }

    execute_claw_process(
        model,
        &command_prompt,
        None,
        64,
        web_run_timeout(),
        "Claw",
        session_id,
        broadcaster,
    )
}

fn execute_auto(
    prompt: &str,
    session_id: &str,
    broadcaster: &SessionBroadcaster,
) -> Result<ClawRunResult, super::ApiError> {
    let total_budget = web_run_timeout().min(AUTO_MAX_TOTAL_TIME);
    let scout_timeout = (total_budget / 3).min(AUTO_SCOUT_MAX_TIME);
    let executor_timeout = total_budget.saturating_sub(scout_timeout);
    let scout_model = auto_scout_model()?;
    let scout_prompt = format!(
        "You are the scout in a two-stage coding task. Inspect only enough of the workspace to hand off the task below. Use at most 64 tool calls and only the supplied read/search tools. Do not use a shell, web search, agents, edits, tests, or broad recursive investigation. Do not retry failures. Return exactly: relevant files, existing behavior, and the smallest next step.\n\nTask:\n{prompt}"
    );
    let scout = execute_claw_process(
        scout_model,
        &scout_prompt,
        Some(SCOUT_TOOLS),
        64,
        scout_timeout,
        "Claw scout",
        session_id,
        broadcaster,
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
        "You are the executor in a two-stage coding task. Use the scout report as a narrow map, then complete the user request. Work only on files identified by the scout unless a required dependency forces one additional lookup. Use at most 64 tool calls. Do not redo broad repository discovery, do not use web search, do not launch agents, and never retry a failed tool. If the scout is incomplete, make one targeted glob/grep lookup, then proceed. Finish with a direct answer or a concise summary of the change.\n\n<Scout report>\n{scout_report}\n</Scout report>\n\n<User task>\n{prompt}\n</User task>"
    );
    let executor = execute_claw_process(
        auto_executor_model(prompt),
        &executor_prompt,
        Some(executor_tools()),
        64,
        executor_timeout,
        "Claw executor",
        session_id,
        broadcaster,
    )?;
    activities.extend(prefix_activities("Executor", executor.activities));
    Ok(ClawRunResult {
        message: executor.message,
        activities,
    })
}

fn auto_scout_model() -> Result<&'static str, super::ApiError> {
    if has_env_key("NVIDIA_API_KEY") {
        Ok("nvidia-fast")
    } else {
        Err(internal_error("Claw Auto requires NVIDIA_API_KEY"))
    }
}

fn auto_executor_model(prompt: &str) -> &'static str {
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
    session_id: &str,
    broadcaster: &SessionBroadcaster,
) -> Result<ClawRunResult, super::ApiError> {
    let binary = std::env::var("CLAW_BIN").unwrap_or_else(|_| "claw".to_string());
    let existing_sessions = snapshot_managed_sessions();
    let prompt_file = write_prompt_file(prompt)?;
    let mut command = Command::new(&binary);
    command
        .args(["--model", model, "--print", "--prompt-file"])
        .arg(&prompt_file)
        .env("CLAW_MAX_TOOL_ITERATIONS", max_tool_iterations.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(allowed_tools) = allowed_tools {
        command.args(["--allowed-tools", allowed_tools]);
    }

    let mut child = command.spawn().map_err(|error| {
        internal_error(format!(
            "could not start `{binary}`: {error}. Install Claw or set CLAW_BIN to its executable path"
        ))
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| internal_error("Claw stdout pipe was not available"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| internal_error("Claw stderr pipe was not available"))?;
    let (sender, receiver) = mpsc::channel();
    let stdout_sender = sender.clone();
    let stdout_thread = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if let Ok(line) = line {
                let _ = stdout_sender.send(ChildOutput::Stdout(line));
            } else {
                break;
            }
        }
    });
    let stderr_thread = thread::spawn(move || {
        let mut output = String::new();
        let mut reader = BufReader::new(stderr);
        if std::io::Read::read_to_string(&mut reader, &mut output).is_ok() {
            if !output.trim().is_empty() {
                let _ = sender.send(ChildOutput::Stderr(output));
            }
        }
    });

    publish_activity(
        broadcaster,
        session_id,
        RunActivity {
            kind: "process".to_string(),
            label: format!("{stage} · started"),
            detail: format!("model {model} · live work log enabled"),
            is_error: false,
        },
    );

    let started = Instant::now();
    let mut last_heartbeat = Instant::now();
    let mut live_tool: Option<String> = None;
    let mut stderr_output = String::new();
    loop {
        loop {
            match receiver.try_recv() {
                Ok(ChildOutput::Stdout(line)) => {
                    publish_child_stdout_line(
                        broadcaster,
                        session_id,
                        stage,
                        &line,
                        &mut live_tool,
                    );
                }
                Ok(ChildOutput::Stderr(output)) => stderr_output.push_str(&output),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                stdout_thread.join().ok();
                stderr_thread.join().ok();
                loop {
                    match receiver.try_recv() {
                        Ok(ChildOutput::Stdout(line)) => publish_child_stdout_line(
                            broadcaster,
                            session_id,
                            stage,
                            &line,
                            &mut live_tool,
                        ),
                        Ok(ChildOutput::Stderr(output)) => stderr_output.push_str(&output),
                        Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                    }
                }
                let _ = std::fs::remove_file(&prompt_file);
                if !status.success() {
                    let detail = if stderr_output.trim().is_empty() {
                        format!("Claw exited with status {status}")
                    } else {
                        stderr_output.trim().to_string()
                    };
                    publish_activity(
                        broadcaster,
                        session_id,
                        RunActivity {
                            kind: "process".to_string(),
                            label: format!("{stage} · failed"),
                            detail: truncate(&detail, 500),
                            is_error: true,
                        },
                    );
                    return Err(internal_error(detail));
                }

                let session_path = latest_new_session(&existing_sessions);
                let result = session_path
                    .and_then(|path| RuntimeSession::load_from_path(path).ok())
                    .map(claw_run_result_from_session);
                let Some(result) = result else {
                    let detail = if stderr_output.trim().is_empty() {
                        "Claw completed but no persisted session result was found".to_string()
                    } else {
                        stderr_output.trim().to_string()
                    };
                    publish_activity(
                        broadcaster,
                        session_id,
                        RunActivity {
                            kind: "process".to_string(),
                            label: format!("{stage} · failed"),
                            detail: truncate(&detail, 500),
                            is_error: true,
                        },
                    );
                    return Err(internal_error(detail));
                };

                publish_activity(
                    broadcaster,
                    session_id,
                    RunActivity {
                        kind: "process".to_string(),
                        label: format!("{stage} · completed"),
                        detail: format!("{}s elapsed", started.elapsed().as_secs()),
                        is_error: false,
                    },
                );
                return Ok(result);
            }
            Ok(None) if started.elapsed() < timeout => {
                if last_heartbeat.elapsed() >= LIVE_HEARTBEAT_INTERVAL {
                    publish_activity(
                        broadcaster,
                        session_id,
                        RunActivity {
                            kind: "heartbeat".to_string(),
                            label: format!("{stage} · running"),
                            detail: format!(
                                "{}s elapsed · waiting for the next tool event",
                                started.elapsed().as_secs()
                            ),
                            is_error: false,
                        },
                    );
                    last_heartbeat = Instant::now();
                }
                thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                stdout_thread.join().ok();
                stderr_thread.join().ok();
                let _ = std::fs::remove_file(&prompt_file);
                publish_activity(
                    broadcaster,
                    session_id,
                    RunActivity {
                        kind: "process".to_string(),
                        label: format!("{stage} · timeout"),
                        detail: format!(
                            "stopped after {} seconds while still working",
                            timeout.as_secs()
                        ),
                        is_error: true,
                    },
                );
                return Err(internal_error(format!(
                    "{stage} did not finish within {} seconds. It was stopped before it could keep consuming tools.",
                    timeout.as_secs()
                )));
            }
            Err(error) => {
                let _ = std::fs::remove_file(&prompt_file);
                publish_activity(
                    broadcaster,
                    session_id,
                    RunActivity {
                        kind: "process".to_string(),
                        label: format!("{stage} · monitor error"),
                        detail: error.to_string(),
                        is_error: true,
                    },
                );
                return Err(internal_error(format!("could not monitor Claw: {error}")));
            }
        }
    }
}

fn publish_child_stdout_line(
    broadcaster: &SessionBroadcaster,
    session_id: &str,
    stage: &str,
    line: &str,
    live_tool: &mut Option<String>,
) {
    let clean = strip_ansi(line).trim().to_string();
    if clean.is_empty() {
        return;
    }
    if let Some(name) = clean
        .strip_prefix("╭─ ")
        .and_then(|value| value.strip_suffix(" ─╮"))
    {
        *live_tool = Some(name.trim().to_string());
        return;
    }
    if let (Some(name), Some(detail)) = (live_tool.as_ref(), clean.strip_prefix("│ ")) {
        publish_activity(
            broadcaster,
            session_id,
            RunActivity {
                kind: "tool_call".to_string(),
                label: format!("{stage} · {name}"),
                detail: truncate(detail.trim(), 500),
                is_error: false,
            },
        );
        *live_tool = None;
        return;
    }
    if clean.starts_with('✓') || clean.starts_with('✗') {
        let is_error = clean.starts_with('✗');
        let detail = clean
            .trim_start_matches(['✓', '✗'])
            .trim()
            .to_string();
        publish_activity(
            broadcaster,
            session_id,
            RunActivity {
                kind: "tool_result".to_string(),
                label: format!("{stage} · {detail}"),
                detail: "tool result received".to_string(),
                is_error,
            },
        );
    }
}

fn publish_activity(
    broadcaster: &SessionBroadcaster,
    session_id: &str,
    activity: RunActivity,
) {
    let _ = broadcaster.send(super::SessionEvent::Activity {
        session_id: session_id.to_string(),
        activity,
    });
}

fn strip_ansi(value: &str) -> String {
    let mut clean = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.next() == Some('[') {
                for code in chars.by_ref() {
                    if code.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        clean.push(ch);
    }
    clean
}

fn snapshot_managed_sessions() -> HashSet<PathBuf> {
    let directory = std::env::current_dir()
        .ok()
        .map(|cwd| cwd.join(".claw").join("sessions"));
    directory
        .and_then(|path| std::fs::read_dir(path).ok())
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"))
        .collect()
}

fn latest_new_session(existing: &HashSet<PathBuf>) -> Option<PathBuf> {
    snapshot_managed_sessions()
        .into_iter()
        .filter(|path| !existing.contains(path))
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn claw_run_result_from_session(session: RuntimeSession) -> ClawRunResult {
    let message = session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::Assistant)
        .map(|message| {
            message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
        .trim()
        .to_string();

    let activities = session
        .messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { name, input, .. } => Some(RunActivity {
                kind: "tool_call".to_string(),
                label: name.clone(),
                detail: summarize_input(&Value::String(input.clone())),
                is_error: false,
            }),
            ContentBlock::ToolResult {
                tool_name,
                output,
                is_error,
                ..
            } => Some(RunActivity {
                kind: "tool_result".to_string(),
                label: tool_name.clone(),
                detail: truncate(output, 500),
                is_error: *is_error,
            }),
            _ => None,
        })
        .collect();

    ClawRunResult { message, activities }
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
        auto_executor_model, prompt_with_history, web_run_timeout_from_env, DEFAULT_WEB_RUN_TIMEOUT,
    };
    use runtime::{ConversationMessage, Session};

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

    #[test]
    fn auto_uses_only_nvidia_models() {
        assert_eq!(auto_executor_model("implement this feature"), "nvidia-agent");
        assert_eq!(auto_executor_model("design an architecture"), "nvidia-plan");
    }
}
