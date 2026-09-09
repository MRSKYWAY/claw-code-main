use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(crate) fn resolve(skill: &str) -> Result<PathBuf, String> {
    let requested = skill.trim();
    if requested.is_empty() {
        return Err(String::from("skill must not be empty"));
    }
    if requested.contains('/') || requested.contains('\\') || requested == "." || requested == ".." {
        return Err(String::from("skill name must not contain path separators"));
    }

    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut roots = Vec::new();
    for ancestor in cwd.ancestors() {
        roots.push(ancestor.join(".claw").join("skills"));
        roots.push(ancestor.join(".codex").join("skills"));
        roots.push(ancestor.join(".claw").join("commands"));
        roots.push(ancestor.join(".codex").join("commands"));
    }
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        let home = PathBuf::from(codex_home);
        roots.push(home.join("skills"));
        roots.push(home.join("commands"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join(".agents").join("skills"));
        roots.push(home.join(".config").join("opencode").join("skills"));
        roots.push(home.join(".claw").join("skills"));
        roots.push(home.join(".codex").join("skills"));
        roots.push(home.join(".claw").join("commands"));
        roots.push(home.join(".codex").join("commands"));
    }
    if let Ok(extra) = std::env::var("CLAW_SKILL_PATHS") {
        roots.extend(std::env::split_paths(&extra));
    }

    let mut seen = BTreeSet::new();
    for root in roots {
        if !seen.insert(root.clone()) {
            continue;
        }
        if let Some(path) = find_in_root(&root, requested) {
            return Ok(path);
        }
    }

    Err(format!("unknown skill: {requested}"))
}

fn find_in_root(root: &Path, requested: &str) -> Option<PathBuf> {
    let direct = root.join(requested);
    if direct.is_file() {
        return Some(direct);
    }
    let direct_skill = direct.join("SKILL.md");
    if direct_skill.is_file() {
        return Some(direct_skill);
    }
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().eq_ignore_ascii_case(requested) {
            continue;
        }
        let path = entry.path();
        if path.is_file() {
            return Some(path);
        }
        let skill = path.join("SKILL.md");
        if skill.is_file() {
            return Some(skill);
        }
    }
    None
}
