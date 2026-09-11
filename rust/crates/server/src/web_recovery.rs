use std::collections::HashSet;
use std::path::PathBuf;

use runtime::{ContentBlock, MessageRole, Session as RuntimeSession};

use crate::RunActivity;

pub(crate) const FAILURE_RECOVERY_TIMEOUT_SECS: u64 = 90;

pub(crate) fn recovery_model(model: &str) -> &str {
    if model == "claw-auto" {
        "nvidia-agent"
    } else {
        model
    }
}

pub(crate) fn new_session_paths(existing: &HashSet<PathBuf>) -> Vec<PathBuf> {
    let mut sessions = std::env::current_dir()
        .ok()
        .map(|cwd| cwd.join(".claw").join("sessions"))
        .and_then(|directory| std::fs::read_dir(directory).ok())
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"))
        .filter(|path| !existing.contains(path))
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect::<Vec<_>>();
    sessions.sort_by_key(|(modified, _)| *modified);
    sessions.into_iter().map(|(_, path)| path).collect()
}

pub(crate) fn execution_context(existing: &HashSet<PathBuf>) -> (String, Vec<RunActivity>) {
    let mut context = String::new();
    let mut activities = Vec::new();
    for path in new_session_paths(existing) {
        let Ok(session) = RuntimeSession::load_from_path(&path) else {
            continue;
        };
        context.push_str(&format!("\n<execution-session path=\"{}\">\n", path.display()));
        for message in &session.messages {
            match message.role {
                MessageRole::Assistant => {
                    let text = message
                        .blocks
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    if !text.trim().is_empty() {
                        context.push_str("assistant: ");
                        context.push_str(&truncate(&text, 4000));
                        context.push('\n');
                    }
                }
                MessageRole::Tool => {
                    for block in &message.blocks {
                        match block {
                            ContentBlock::ToolUse { name, input, .. } => {
                                context.push_str(&format!("tool call {name}: {}\n", truncate(input, 2000)));
                                activities.push(RunActivity {
                                    kind: "tool_call".to_string(),
                                    label: name.clone(),
                                    detail: truncate(input, 500),
                                    is_error: false,
                                });
                            }
                            ContentBlock::ToolResult {
                                tool_name,
                                output,
                                is_error,
                                ..
                            } => {
                                context.push_str(&format!(
                                    "tool result {tool_name}: {}\n",
                                    truncate(output, 3000)
                                ));
                                activities.push(RunActivity {
                                    kind: "tool_result".to_string(),
                                    label: tool_name.clone(),
                                    detail: truncate(output, 500),
                                    is_error: *is_error,
                                });
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        context.push_str("</execution-session>\n");
    }
    (truncate(&context, 24_000), activities)
}

pub(crate) fn failure_recovery_prompt(
    task_prompt: &str,
    failure: &str,
    conversation: &RuntimeSession,
    execution_context: &str,
) -> String {
    let history = conversation
        .messages
        .iter()
        .map(|message| {
            let role = match message.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };
            let text = message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            if text.trim().is_empty() {
                None
            } else {
                Some(format!("{role}: {}", truncate(&text, 4000)))
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "FAILURE RECOVERY PASS. The execution run ended before it could provide a final response. Do not use tools or make further workspace changes. Based only on the completed work and evidence below, produce the best possible final answer to the user's request. Be factual and explicit: distinguish what was completed, what remains unfinished, what validation actually happened, and the concrete blocker or failure. Do not claim work that was not verified. Use these headings exactly: Completed, Remaining, Validation, Blockers.\n\n<user-task>\n{task_prompt}\n</user-task>\n\n<failure>\n{failure}\n</failure>\n\n<persisted-conversation>\n{history}\n</persisted-conversation>\n\n<execution-evidence>\n{execution_context}\n</execution-evidence>"
    )
}

pub(crate) fn deterministic_fallback(failure: &str) -> String {
    format!(
        "Completed: the run stopped after consuming the available execution budget or encountering a runtime failure, and the completed execution state was preserved.\nRemaining: the exact remaining work could not be confirmed because the recovery pass did not return a final synthesis.\nValidation: completed tool results and the recorded failure state remain available in the session.\nBlockers: {failure}"
    )
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

#[cfg(test)]
mod tests {
    use super::{deterministic_fallback, failure_recovery_prompt, recovery_model};
    use runtime::{ConversationMessage, Session};

    #[test]
    fn auto_uses_nvidia_agent_for_recovery() {
        assert_eq!(recovery_model("claw-auto"), "nvidia-agent");
        assert_eq!(recovery_model("nvidia-fast"), "nvidia-fast");
    }

    #[test]
    fn recovery_prompt_contains_failure_and_required_sections() {
        let mut session = Session::new();
        session.messages.push(ConversationMessage::user_text("Do the task"));
        let prompt = failure_recovery_prompt(
            "Do the task",
            "timed out after 900 seconds",
            &session,
            "cargo run was still executing",
        );
        assert!(prompt.contains("timed out after 900 seconds"));
        assert!(prompt.contains("Completed, Remaining, Validation, Blockers"));
        assert!(prompt.contains("cargo run was still executing"));
    }

    #[test]
    fn fallback_reports_the_blocker_instead_of_failing() {
        let message = deterministic_fallback("provider recovery stream failed");
        assert!(message.contains("Blockers: provider recovery stream failed"));
    }
}
