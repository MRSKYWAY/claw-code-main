use std::path::{Path, PathBuf};

use crate::session::Session;

const EXCLUSION_MARKERS: &[&str] = &[
    "do not search files in ", "don't search files in ", "dont search files in ",
    "do not search in ", "don't search in ", "dont search in ",
    "do not inspect ", "don't inspect ", "dont inspect ",
    "do not access ", "don't access ", "dont access ",
    "do not read ", "don't read ", "dont read ",
    "do not touch ", "don't touch ", "dont touch ",
];

pub fn apply_user_scope_constraints(session: &mut Session, user_input: &str) -> Vec<String> {
    let workspace = std::env::current_dir().ok();
    let lower = user_input.to_ascii_lowercase();
    let mut added = Vec::new();

    for marker in EXCLUSION_MARKERS {
        let mut search_from = 0;
        while let Some(relative) = lower[search_from..].find(marker) {
            let start = search_from + relative + marker.len();
            let tail = &user_input[start..];
            if let Some(target) = extract_target(tail) {
                let normalized = normalize_scope_path(&target, workspace.as_deref());
                if session.add_excluded_path(normalized.clone()) {
                    added.push(normalized);
                }
            }
            search_from = start.max(search_from + 1);
        }
    }

    added
}

fn extract_target(tail: &str) -> Option<String> {
    let lower = tail.to_ascii_lowercase();
    let mut end = lower.len();
    for delimiter in [" directory", " folder", " for this session", ".\\n", "\\n", ".", "!", "?"] {
        if let Some(index) = lower.find(delimiter) {
            end = end.min(index);
        }
    }
    let target = tail[..end].trim().trim_matches(['"', '\'', '`']).trim();
    (!target.is_empty()).then(|| target.to_string())
}

pub fn normalize_scope_path(path: &str, workspace: Option<&Path>) -> String {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        workspace.map(|root| root.join(path)).unwrap_or_else(|| PathBuf::from(path))
    };

    candidate.canonicalize().unwrap_or(candidate).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::apply_user_scope_constraints;
    use crate::session::Session;

    #[test]
    fn extracts_directory_exclusion() {
        let mut session = Session::new();
        let added = apply_user_scope_constraints(
            &mut session,
            "Go through the lecture slides. Do not search files in CEG5304_AY2627S1_CA1 directory for this session.",
        );
        assert_eq!(added.len(), 1);
        assert!(added[0].ends_with("CEG5304_AY2627S1_CA1"));
    }

    #[test]
    fn deduplicates_repeated_exclusions() {
        let mut session = Session::new();
        assert_eq!(apply_user_scope_constraints(&mut session, "Do not search files in private directory.").len(), 1);
        assert!(apply_user_scope_constraints(&mut session, "Don't search files in private directory.").is_empty());
        assert_eq!(session.excluded_paths().len(), 1);
    }
}