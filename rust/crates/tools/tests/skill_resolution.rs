use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn temp_path(name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    env::temp_dir().join(format!("claw-skill-{unique}-{name}"))
}

#[test]
fn skill_resolves_from_codex_home_and_preserves_args() {
    let _guard = env_lock().lock().expect("env lock");
    let root = temp_path("codex-home");
    let skill_dir = root.join("skills").join("demo");
    fs::create_dir_all(&skill_dir).expect("create skill directory");
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: Demo skill\n---\n\nDo the demo workflow.\n",
    )
    .expect("write skill");

    let previous = env::var_os("CODEX_HOME");
    env::set_var("CODEX_HOME", &root);

    let result = tools::execute_tool(
        "Skill",
        &serde_json::json!({"skill": "demo", "args": "first second"}),
    )
    .expect("configured skill should resolve");
    let output: Value = serde_json::from_str(&result).expect("valid JSON");

    assert_eq!(output["skill"], "demo");
    assert_eq!(output["args"], "first second");
    assert_eq!(output["description"], "Demo skill");
    assert!(output["prompt"].as_str().expect("prompt").contains("demo workflow"));
    assert!(output["path"].as_str().expect("path").ends_with("demo/SKILL.md"));

    match previous {
        Some(value) => env::set_var("CODEX_HOME", value),
        None => env::remove_var("CODEX_HOME"),
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn skill_name_rejects_path_traversal_and_separator_forms() {
    let _guard = env_lock().lock().expect("env lock");

    for skill in ["../secret", "nested/secret", r"nested\\secret", ".", ".."] {
        let error = tools::execute_tool("Skill", &serde_json::json!({"skill": skill}))
            .expect_err("path-like skill names must be rejected");
        assert!(
            error.contains("path separators") || error.contains("unknown skill"),
            "unexpected error for {skill:?}: {error}"
        );
    }
}

#[test]
fn skill_lookup_is_case_insensitive() {
    let _guard = env_lock().lock().expect("env lock");
    let root = temp_path("case");
    let skill_dir = root.join("skills").join("MySkill");
    fs::create_dir_all(&skill_dir).expect("create skill directory");
    fs::write(skill_dir.join("SKILL.md"), "description: Case skill\n\ncontent")
        .expect("write skill");

    let previous = env::var_os("CODEX_HOME");
    env::set_var("CODEX_HOME", &root);

    let result = tools::execute_tool("Skill", &serde_json::json!({"skill": "myskill"}))
        .expect("case-insensitive skill lookup should resolve");
    let output: Value = serde_json::from_str(&result).expect("valid JSON");
    assert_eq!(output["description"], "Case skill");

    match previous {
        Some(value) => env::set_var("CODEX_HOME", value),
        None => env::remove_var("CODEX_HOME"),
    }
    let _ = fs::remove_dir_all(root);
}
