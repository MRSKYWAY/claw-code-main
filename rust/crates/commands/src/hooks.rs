use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use runtime::ConfigLoader;
use serde_json::{Map, Value};

const SETTINGS_FILE: &str = "settings.local.json";
const HOOKS_KEY: &str = "hooks";
const PRE_EVENT: &str = "PreToolUse";
const POST_EVENT: &str = "PostToolUse";

pub fn handle_hooks_slash_command(
    action: Option<&str>,
    event: Option<&str>,
    command: Option<&str>,
    cwd: &Path,
) -> io::Result<String> {
    match action.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("list") => render_hook_list(cwd),
        Some("add") => mutate_hook(cwd, event, command, true),
        Some("remove") => mutate_hook(cwd, event, command, false),
        Some(other) => Ok(format!(
            "Unknown /hooks action '{other}'. Use /hooks, /hooks add <PreToolUse|PostToolUse> <command>, or /hooks remove <PreToolUse|PostToolUse> <command>."
        )),
    }
}

fn render_hook_list(cwd: &Path) -> io::Result<String> {
    let loader = ConfigLoader::default_for(cwd);
    let config = loader
        .load()
        .map_err(|error| io::Error::other(error.to_string()))?;
    let hooks = config.hooks();
    let mut lines = vec!["Hooks".to_string()];
    render_hook_lines(&mut lines, PRE_EVENT, hooks.pre_tool_use());
    render_hook_lines(&mut lines, POST_EVENT, hooks.post_tool_use());
    Ok(lines.join("\n"))
}

fn render_hook_lines(lines: &mut Vec<String>, label: &str, commands: &[String]) {
    if commands.is_empty() {
        lines.push(format!("  {label:<16} none"));
        return;
    }
    lines.push(format!("  {label}:"));
    for command in commands {
        lines.push(format!("    {command}"));
    }
}

fn mutate_hook(
    cwd: &Path,
    event: Option<&str>,
    command: Option<&str>,
    add: bool,
) -> io::Result<String> {
    let event = normalize_event(event.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "hook event is required: use PreToolUse or PostToolUse",
        )
    })?)?;
    let command = command
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "hook command is required"))?;

    let path = local_settings_path(cwd);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut root = read_local_settings(&path)?;
    let hooks = ensure_object(&mut root, HOOKS_KEY, &path)?;
    let commands = ensure_string_array(hooks, event, &path)?;

    let changed = if add {
        if commands.iter().any(|value| value.as_str() == Some(command)) {
            false
        } else {
            commands.push(Value::String(command.to_string()));
            true
        }
    } else {
        let before = commands.len();
        commands.retain(|value| value.as_str() != Some(command));
        before != commands.len()
    };

    if add && !changed {
        return Ok(format!("Hooks\n  Result           already configured\n  Event            {event}\n  Command          {command}"));
    }
    if !add && !changed {
        return Ok(format!("Hooks\n  Result           not found in local settings\n  Event            {event}\n  Command          {command}"));
    }

    write_settings(&path, &root)?;
    let verb = if add { "added" } else { "removed" };
    Ok(format!(
        "Hooks\n  Result           {verb}\n  Event            {event}\n  Command          {command}\n  Settings         {}",
        path.display()
    ))
}

fn normalize_event(event: &str) -> io::Result<&'static str> {
    match event.trim().to_ascii_lowercase().as_str() {
        "pre" | "pretooluse" => Ok(PRE_EVENT),
        "post" | "posttooluse" => Ok(POST_EVENT),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported hook event '{other}': use PreToolUse or PostToolUse"),
        )),
    }
}

fn local_settings_path(cwd: &Path) -> PathBuf {
    cwd.join(".claw").join(SETTINGS_FILE)
}

fn read_local_settings(path: &Path) -> io::Result<Value> {
    if !path.is_file() {
        return Ok(Value::Object(Map::new()));
    }
    let text = fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(&text).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: invalid JSON: {error}", path.display()),
        )
    })
}

fn ensure_object<'a>(
    root: &'a mut Value,
    key: &str,
    path: &Path,
) -> io::Result<&'a mut Map<String, Value>> {
    if !root.is_object() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: root must be a JSON object", path.display()),
        ));
    }
    let object = root.as_object_mut().expect("root object checked");
    let value = object
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !value.is_object() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {key} must be an object", path.display()),
        ));
    }
    Ok(value.as_object_mut().expect("object checked"))
}

fn ensure_string_array<'a>(
    hooks: &'a mut Map<String, Value>,
    event: &str,
    path: &Path,
) -> io::Result<&'a mut Vec<Value>> {
    let value = hooks
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !value.is_array() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: hooks.{event} must be an array", path.display()),
        ));
    }
    let array = value.as_array_mut().expect("array checked");
    if array.iter().any(|value| !value.is_string()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{}: hooks.{event} must contain only strings",
                path.display()
            ),
        ));
    }
    Ok(array)
}

fn write_settings(path: &Path, root: &Value) -> io::Result<()> {
    let text = serde_json::to_string_pretty(root).map_err(io::Error::other)?;
    fs::write(path, format!("{text}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_WORKSPACE_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_workspace() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let counter = TEMP_WORKSPACE_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "claw-hooks-{}-{}-{}",
            std::process::id(),
            nonce,
            counter
        ))
    }

    #[test]
    fn add_remove_persists_and_preserves_other_local_settings() {
        let cwd = temp_workspace();
        fs::create_dir_all(cwd.join(".claw")).expect("workspace");
        let path = local_settings_path(&cwd);
        fs::write(
            &path,
            r#"{
  "model": "test-model",
  "hooks": {
    "PreToolUse": ["echo existing"]
  }
}
"#,
        )
        .expect("seed settings");

        handle_hooks_slash_command(Some("add"), Some("PreToolUse"), Some("echo added"), &cwd)
            .expect("add hook");
        let after_add: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read settings")).expect("json");
        assert_eq!(after_add["model"], "test-model");
        assert_eq!(
            after_add["hooks"]["PreToolUse"],
            serde_json::json!(["echo existing", "echo added"])
        );

        handle_hooks_slash_command(Some("remove"), Some("pre"), Some("echo existing"), &cwd)
            .expect("remove hook");
        let after_remove: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read settings")).expect("json");
        assert_eq!(after_remove["model"], "test-model");
        assert_eq!(
            after_remove["hooks"]["PreToolUse"],
            serde_json::json!(["echo added"])
        );

        fs::remove_dir_all(cwd).expect("cleanup");
    }

    #[test]
    fn add_does_not_duplicate_commands() {
        let cwd = temp_workspace();
        handle_hooks_slash_command(Some("add"), Some("post"), Some("echo hi"), &cwd)
            .expect("first add");
        let report =
            handle_hooks_slash_command(Some("add"), Some("PostToolUse"), Some("echo hi"), &cwd)
                .expect("second add");
        assert!(report.contains("already configured"));
        let value: Value =
            serde_json::from_str(&fs::read_to_string(local_settings_path(&cwd)).expect("settings"))
                .expect("json");
        assert_eq!(
            value["hooks"]["PostToolUse"],
            serde_json::json!(["echo hi"])
        );
        fs::remove_dir_all(cwd).expect("cleanup");
    }
}
