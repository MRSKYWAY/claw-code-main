use std::ffi::OsStr;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::config::{RuntimeFeatureConfig, RuntimeHookConfig};

const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(10);
const MIN_HOOK_TIMEOUT_MS: u64 = 100;
const MAX_HOOK_TIMEOUT_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
}

impl HookEvent {
    fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDecision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRunResult {
    denied: bool,
    messages: Vec<String>,
}

impl HookRunResult {
    #[must_use]
    pub fn allow(messages: Vec<String>) -> Self {
        Self { denied: false, messages }
    }

    #[must_use]
    pub fn denied(messages: Vec<String>) -> Self {
        Self { denied: true, messages }
    }

    #[must_use]
    pub fn decision(&self) -> HookDecision {
        if self.denied { HookDecision::Deny } else { HookDecision::Allow }
    }

    #[must_use]
    pub fn is_denied(&self) -> bool {
        matches!(self.decision(), HookDecision::Deny)
    }

    #[must_use]
    pub fn messages(&self) -> &[String] {
        &self.messages
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookRunner {
    config: RuntimeHookConfig,
}

#[derive(Debug, Clone, Copy)]
struct HookCommandRequest<'a> {
    event: HookEvent,
    tool_name: &'a str,
    tool_input: &'a str,
    tool_output: Option<&'a str>,
    is_error: bool,
    payload: &'a str,
}

impl HookRunner {
    #[must_use]
    pub fn new(config: RuntimeHookConfig) -> Self { Self { config } }

    #[must_use]
    pub fn from_feature_config(feature_config: &RuntimeFeatureConfig) -> Self {
        Self::new(feature_config.hooks().clone())
    }

    #[must_use]
    pub fn run_pre_tool_use(&self, tool_name: &str, tool_input: &str) -> HookRunResult {
        Self::run_commands(HookEvent::PreToolUse, self.config.pre_tool_use(), tool_name, tool_input, None, false)
    }

    #[must_use]
    pub fn run_post_tool_use(&self, tool_name: &str, tool_input: &str, tool_output: &str, is_error: bool) -> HookRunResult {
        Self::run_commands(HookEvent::PostToolUse, self.config.post_tool_use(), tool_name, tool_input, Some(tool_output), is_error)
    }

    fn run_commands(event: HookEvent, commands: &[String], tool_name: &str, tool_input: &str, tool_output: Option<&str>, is_error: bool) -> HookRunResult {
        if commands.is_empty() { return HookRunResult::allow(Vec::new()); }
        let payload = json!({
            "hook_event_name": event.as_str(),
            "tool_name": tool_name,
            "tool_input": parse_tool_input(tool_input),
            "tool_input_json": tool_input,
            "tool_output": tool_output,
            "tool_result_is_error": is_error,
        }).to_string();
        let mut messages = Vec::new();
        for command in commands {
            match Self::run_command(command, HookCommandRequest { event, tool_name, tool_input, tool_output, is_error, payload: &payload }) {
                HookCommandOutcome::Allow { message } => {
                    if let Some(message) = message { messages.push(message); }
                }
                HookCommandOutcome::Deny { message } => {
                    let message = message.unwrap_or_else(|| format!("{} hook denied tool `{tool_name}`", event.as_str()));
                    messages.push(message);
                    return HookRunResult::denied(messages);
                }
                HookCommandOutcome::Warn { message } => messages.push(message),
            }
        }
        HookRunResult::allow(messages)
    }

    fn run_command(command: &str, request: HookCommandRequest<'_>) -> HookCommandOutcome {
        let mut child = shell_command(command);
        child.stdin(std::process::Stdio::piped());
        child.stdout(std::process::Stdio::piped());
        child.stderr(std::process::Stdio::piped());
        child.env("HOOK_EVENT", request.event.as_str());
        child.env("HOOK_TOOL_NAME", request.tool_name);
        child.env("HOOK_TOOL_INPUT", request.tool_input);
        child.env("HOOK_TOOL_IS_ERROR", if request.is_error { "1" } else { "0" });
        if let Some(tool_output) = request.tool_output { child.env("HOOK_TOOL_OUTPUT", tool_output); }
        match child.output_with_stdin_timeout(request.payload.as_bytes(), hook_timeout()) {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let message = (!stdout.is_empty()).then_some(stdout);
                match output.status.code() {
                    Some(0) => HookCommandOutcome::Allow { message },
                    Some(2) => HookCommandOutcome::Deny { message },
                    Some(code) => HookCommandOutcome::Warn { message: format_hook_warning(command, code, message.as_deref(), stderr.as_str()) },
                    None => HookCommandOutcome::Warn { message: format!("{} hook `{command}` terminated by signal while handling `{}`", request.event.as_str(), request.tool_name) },
                }
            }
            Err(error) => HookCommandOutcome::Warn { message: format!("{} hook `{command}` failed for `{}`: {error}", request.event.as_str(), request.tool_name) },
        }
    }
}

enum HookCommandOutcome {
    Allow { message: Option<String> },
    Deny { message: Option<String> },
    Warn { message: String },
}

fn parse_tool_input(tool_input: &str) -> serde_json::Value {
    serde_json::from_str(tool_input).unwrap_or_else(|_| json!({ "raw": tool_input }))
}

fn format_hook_warning(command: &str, code: i32, stdout: Option<&str>, stderr: &str) -> String {
    let mut message = format!("Hook `{command}` exited with status {code}; allowing tool execution to continue");
    if let Some(stdout) = stdout.filter(|stdout| !stdout.is_empty()) { message.push_str(": "); message.push_str(stdout); }
    else if !stderr.is_empty() { message.push_str(": "); message.push_str(stderr); }
    message
}

fn hook_timeout() -> Duration {
    std::env::var("CLAW_HOOK_TIMEOUT_MS").ok().and_then(|value| value.parse::<u64>().ok()).map(|value| value.clamp(MIN_HOOK_TIMEOUT_MS, MAX_HOOK_TIMEOUT_MS)).map(Duration::from_millis).unwrap_or(DEFAULT_HOOK_TIMEOUT)
}

fn shell_command(command: &str) -> CommandWithStdin {
    #[cfg(windows)]
    let mut command_builder = { let mut command_builder = Command::new("cmd"); command_builder.arg("/C").arg(command); CommandWithStdin::new(command_builder) };
    #[cfg(not(windows))]
    let command_builder = { let mut command_builder = Command::new("sh"); command_builder.arg("-lc").arg(command); CommandWithStdin::new(command_builder) };
    command_builder
}

struct CommandWithStdin { command: Command }

impl CommandWithStdin {
    fn new(command: Command) -> Self { Self { command } }
    fn stdin(&mut self, cfg: std::process::Stdio) -> &mut Self { self.command.stdin(cfg); self }
    fn stdout(&mut self, cfg: std::process::Stdio) -> &mut Self { self.command.stdout(cfg); self }
    fn stderr(&mut self, cfg: std::process::Stdio) -> &mut Self { self.command.stderr(cfg); self }
    fn env<K, V>(&mut self, key: K, value: V) -> &mut Self where K: AsRef<OsStr>, V: AsRef<OsStr> { self.command.env(key, value); self }
    fn output_with_stdin_timeout(&mut self, stdin: &[u8], timeout: Duration) -> std::io::Result<std::process::Output> {
        let mut child = self.command.spawn()?;
        if let Some(mut child_stdin) = child.stdin.take() { use std::io::Write; child_stdin.write_all(stdin)?; }
        let started = Instant::now();
        loop {
            if child.try_wait()?.is_some() { return child.wait_with_output(); }
            if started.elapsed() >= timeout { let _ = child.kill(); let _ = child.wait(); return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, format!("timed out after {} ms", timeout.as_millis()))); }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{hook_timeout, HookDecision, HookRunResult, HookRunner, MAX_HOOK_TIMEOUT_MS, MIN_HOOK_TIMEOUT_MS};
    use crate::config::{RuntimeFeatureConfig, RuntimeHookConfig};
    use std::time::Duration;

    #[test]
    fn allows_exit_code_zero_and_captures_stdout() {
        let runner = HookRunner::new(RuntimeHookConfig::new(vec![shell_snippet("printf 'pre ok'")], Vec::new()));
        let result = runner.run_pre_tool_use("Read", r#"{"path":"README.md"}"#);
        assert_eq!(result, HookRunResult::allow(vec!["pre ok".to_string()]));
        assert_eq!(result.decision(), HookDecision::Allow);
    }

    #[test]
    fn denies_exit_code_two() {
        let runner = HookRunner::new(RuntimeHookConfig::new(vec![shell_snippet("printf 'blocked by hook'; exit 2")], Vec::new()));
        let result = runner.run_pre_tool_use("Bash", r#"{"command":"pwd"}"#);
        assert!(result.is_denied());
        assert_eq!(result.decision(), HookDecision::Deny);
        assert_eq!(result.messages(), &["blocked by hook".to_string()]);
    }

    #[test]
    fn warns_for_other_non_zero_statuses() {
        let runner = HookRunner::from_feature_config(&RuntimeFeatureConfig::default().with_hooks(RuntimeHookConfig::new(vec![shell_snippet("printf 'warning hook'; exit 1")], Vec::new())));
        let result = runner.run_pre_tool_use("Edit", r#"{"file":"src/lib.rs"}"#);
        assert!(!result.is_denied());
        assert!(result.messages().iter().any(|message| message.contains("allowing tool execution to continue")));
    }

    #[cfg(not(windows))]
    #[test]
    fn times_out_long_running_hooks() {
        std::env::set_var("CLAW_HOOK_TIMEOUT_MS", "100");
        let runner = HookRunner::new(RuntimeHookConfig::new(vec![shell_snippet("sleep 1")], Vec::new()));
        let result = runner.run_pre_tool_use("Read", r#"{"path":"README.md"}"#);
        std::env::remove_var("CLAW_HOOK_TIMEOUT_MS");
        assert!(!result.is_denied());
        assert!(result.messages().iter().any(|message| message.contains("timed out after 100 ms")));
    }

    #[test]
    fn timeout_configuration_has_safe_bounds() {
        std::env::set_var("CLAW_HOOK_TIMEOUT_MS", "1");
        assert_eq!(hook_timeout(), Duration::from_millis(MIN_HOOK_TIMEOUT_MS));
        std::env::set_var("CLAW_HOOK_TIMEOUT_MS", "999999");
        assert_eq!(hook_timeout(), Duration::from_millis(MAX_HOOK_TIMEOUT_MS));
        std::env::remove_var("CLAW_HOOK_TIMEOUT_MS");
        assert_eq!(hook_timeout(), Duration::from_secs(10));
    }

    #[cfg(windows)]
    fn shell_snippet(script: &str) -> String { script.replace('\'', "\"") }
    #[cfg(not(windows))]
    fn shell_snippet(script: &str) -> String { script.to_string() }
}
