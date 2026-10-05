use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ContentBlock, ConversationMessage, TurnSummary};

const MEMORY_VERSION: u32 = 1;
const MAX_MEMORY_ENTRIES: usize = 500;
const MAX_CONTEXT_ENTRIES: usize = 5;
const MAX_CONTEXT_CHARS: usize = 6_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Episode,
    Lesson,
    Strategy,
    Preference,
}

impl MemoryKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Episode => "episode",
            Self::Lesson => "lesson",
            Self::Strategy => "strategy",
            Self::Preference => "preference",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Project,
}

impl MemoryScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryEntry {
    pub id: String,
    pub kind: MemoryKind,
    pub scope: MemoryScope,
    pub content: String,
    pub tags: Vec<String>,
    pub confidence: f32,
    pub usefulness: f32,
    pub created_at: u64,
    #[serde(default)]
    pub last_used_at: Option<u64>,
    #[serde(default)]
    pub use_count: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemorySearchResult {
    pub score: f32,
    pub entry: MemoryEntry,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LearningReport {
    pub created: usize,
    pub reinforced: usize,
    pub errors_observed: usize,
}

#[derive(Debug)]
pub enum MemoryError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Format(String),
}

impl Display for MemoryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "{error}"),
            Self::Format(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for MemoryError {}

impl From<std::io::Error> for MemoryError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for MemoryError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoryFile {
    version: u32,
    entries: Vec<MemoryEntry>,
}

#[derive(Debug, Clone)]
pub struct MemoryStore {
    path: PathBuf,
    entries: Vec<MemoryEntry>,
}

impl MemoryStore {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, MemoryError> {
        let path = path.into();
        if !path.is_file() {
            return Ok(Self {
                path,
                entries: Vec::new(),
            });
        }

        let contents = fs::read_to_string(&path)?;
        let file = serde_json::from_str::<MemoryFile>(&contents)?;
        if file.version != MEMORY_VERSION {
            return Err(MemoryError::Format(format!(
                "unsupported memory version: {}",
                file.version
            )));
        }

        Ok(Self {
            path,
            entries: file.entries,
        })
    }

    pub fn save(&self) -> Result<(), MemoryError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = MemoryFile {
            version: MEMORY_VERSION,
            entries: self.entries.clone(),
        };
        let contents = serde_json::to_vec_pretty(&file)?;
        let temp_path = atomic_temp_path(&self.path);
        let mut handle = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;

        let result = (|| -> Result<(), MemoryError> {
            handle.write_all(&contents)?;
            handle.sync_all()?;
            drop(handle);

            match fs::rename(&temp_path, &self.path) {
                Ok(()) => Ok(()),
                Err(error)
                    if cfg!(windows) && error.kind() == std::io::ErrorKind::AlreadyExists =>
                {
                    fs::remove_file(&self.path)?;
                    fs::rename(&temp_path, &self.path)?;
                    Ok(())
                }
                Err(error) => Err(MemoryError::Io(error)),
            }
        })();

        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn entries(&self) -> &[MemoryEntry] {
        &self.entries
    }

    pub fn forget(&mut self, id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.id != id);
        self.entries.len() != before
    }

    pub fn clear(&mut self) -> usize {
        let removed = self.entries.len();
        self.entries.clear();
        removed
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<MemorySearchResult> {
        if limit == 0 {
            return Vec::new();
        }

        let query_terms = tokenize(query);
        let mut matches = self
            .entries
            .iter()
            .filter_map(|entry| {
                let searchable = format!("{} {}", entry.content, entry.tags.join(" "));
                let terms = tokenize(&searchable);
                let lexical = if query_terms.is_empty() {
                    0.0
                } else {
                    let overlap = query_terms
                        .iter()
                        .filter(|term| terms.contains(term.as_str()))
                        .count();
                    if overlap == 0 {
                        return None;
                    }
                    overlap as f32 / query_terms.len() as f32
                };
                let kind_bonus = match entry.kind {
                    MemoryKind::Preference => 0.15,
                    MemoryKind::Strategy => 0.10,
                    MemoryKind::Lesson => 0.08,
                    MemoryKind::Episode => 0.03,
                };
                let reinforcement = (entry.use_count.min(10) as f32 / 10.0) * 0.05;
                let score =
                    lexical + entry.confidence * 0.20 + entry.usefulness * 0.15 + kind_bonus
                        + reinforcement;
                Some(MemorySearchResult {
                    score,
                    entry: entry.clone(),
                })
            })
            .collect::<Vec<_>>();

        matches.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| right.entry.use_count.cmp(&left.entry.use_count))
                .then_with(|| right.entry.created_at.cmp(&left.entry.created_at))
        });
        matches.truncate(limit);
        matches
    }

    #[must_use]
    pub fn context_for(&self, query: &str, limit: usize) -> Option<String> {
        let matches = self.search(query, limit.min(MAX_CONTEXT_ENTRIES));
        if matches.is_empty() {
            return None;
        }

        let mut context = String::from(
            "LEARNED MEMORY (project-local, advisory; verify it against the current task before acting):\n",
        );
        for result in matches {
            let line = format!(
                "- [{}:{}] {}\n",
                result.entry.kind.as_str(),
                result.entry.id,
                result.entry.content
            );
            if context.len() + line.len() > MAX_CONTEXT_CHARS {
                break;
            }
            context.push_str(&line);
        }
        Some(context.trim_end().to_string())
    }

    pub fn learn_from_turn(
        &mut self,
        task: &str,
        summary: &TurnSummary,
    ) -> LearningReport {
        let task = truncate(task, 280);
        let tool_names = collect_tool_names(&summary.tool_results);
        let errors = collect_tool_errors(&summary.tool_results);
        let mut report = LearningReport {
            errors_observed: errors.len(),
            ..LearningReport::default()
        };

        if tool_names.is_empty() {
            if let Some(preference) = extract_explicit_preference(task) {
                let (created, reinforced) = self.add_or_reinforce(
                    MemoryKind::Preference,
                    MemoryScope::Project,
                    preference,
                    vec!["preference".to_string()],
                    0.95,
                    0.85,
                );
                report.created += usize::from(created);
                report.reinforced += usize::from(reinforced);
            }
            return report;
        }

        let outcome = if errors.is_empty() {
            "succeeded"
        } else {
            "completed with tool errors"
        };
        let episode = format!(
            "Task experience: {task}. Tools used: {}. Outcome: {outcome}.",
            tool_names.join(", ")
        );
        let (created, reinforced) = self.add_or_reinforce(
            MemoryKind::Episode,
            MemoryScope::Project,
            episode,
            tool_names.iter().map(ToOwned::to_owned).collect(),
            0.75,
            if errors.is_empty() { 0.70 } else { 0.45 },
        );
        report.created += usize::from(created);
        report.reinforced += usize::from(reinforced);

        if errors.is_empty() && tool_names.len() >= 2 {
            let strategy = format!(
                "Successful tool sequence for task \"{task}\": {}. Reuse this sequence only when relevant and re-verify each result.",
                tool_names.join(" -> ")
            );
            let (created, reinforced) = self.add_or_reinforce(
                MemoryKind::Strategy,
                MemoryScope::Project,
                strategy,
                vec!["strategy".to_string()],
                0.65,
                0.65,
            );
            report.created += usize::from(created);
            report.reinforced += usize::from(reinforced);
        }

        for (tool, error) in errors {
            let lesson = format!(
                "Failure observed while solving \"{task}\": tool {tool} returned: {error}. Verify the assumptions before repeating the same approach."
            );
            let (created, reinforced) = self.add_or_reinforce(
                MemoryKind::Lesson,
                MemoryScope::Project,
                lesson,
                vec![tool],
                0.55,
                0.50,
            );
            report.created += usize::from(created);
            report.reinforced += usize::from(reinforced);
        }

        if let Some(preference) = extract_explicit_preference(task) {
            let (created, reinforced) = self.add_or_reinforce(
                MemoryKind::Preference,
                MemoryScope::Project,
                preference,
                vec!["preference".to_string()],
                0.95,
                0.85,
            );
            report.created += usize::from(created);
            report.reinforced += usize::from(reinforced);
        }

        report
    }

    fn add_or_reinforce(
        &mut self,
        kind: MemoryKind,
        scope: MemoryScope,
        content: String,
        tags: Vec<String>,
        confidence: f32,
        usefulness: f32,
    ) -> (bool, bool) {
        let now = now_epoch_secs();
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|entry| entry.kind == kind && entry.scope == scope && entry.content == content)
        {
            existing.use_count = existing.use_count.saturating_add(1);
            existing.last_used_at = Some(now);
            existing.confidence = (existing.confidence + 0.05).min(1.0);
            existing.usefulness = (existing.usefulness + 0.05).min(1.0);
            for tag in tags {
                if !existing.tags.contains(&tag) {
                    existing.tags.push(tag);
                }
            }
            return (false, true);
        }

        let seed = format!("{kind:?}|{scope:?}|{content}|{now}");
        let id = format!("{:x}", Sha256::digest(seed.as_bytes()))
            .chars()
            .take(12)
            .collect::<String>();
        self.entries.push(MemoryEntry {
            id,
            kind,
            scope,
            content,
            tags,
            confidence,
            usefulness,
            created_at: now,
            last_used_at: Some(now),
            use_count: 1,
        });
        self.prune();
        (true, false)
    }

    fn prune(&mut self) {
        if self.entries.len() <= MAX_MEMORY_ENTRIES {
            return;
        }

        self.entries.sort_by(|left, right| {
            let left_score = left.usefulness * 0.6
                + left.confidence * 0.3
                + (left.use_count.min(10) as f32 / 10.0) * 0.1;
            let right_score = right.usefulness * 0.6
                + right.confidence * 0.3
                + (right.use_count.min(10) as f32 / 10.0) * 0.1;
            right_score
                .partial_cmp(&left_score)
                .unwrap_or(Ordering::Equal)
        });
        self.entries.truncate(MAX_MEMORY_ENTRIES);
    }
}

fn collect_tool_names(messages: &[ConversationMessage]) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();
    for message in messages {
        for block in &message.blocks {
            if let ContentBlock::ToolResult { tool_name, .. } = block {
                if seen.insert(tool_name.clone()) {
                    names.push(tool_name.clone());
                }
            }
        }
    }
    names
}

fn collect_tool_errors(messages: &[ConversationMessage]) -> Vec<(String, String)> {
    let mut errors = Vec::new();
    for message in messages {
        for block in &message.blocks {
            let ContentBlock::ToolResult {
                tool_name,
                output,
                is_error,
                ..
            } = block
            else {
                continue;
            };
            if *is_error {
                errors.push((tool_name.clone(), truncate(output, 360)));
            }
        }
    }
    errors
}

fn extract_explicit_preference(input: &str) -> Option<String> {
    let lowered = input.to_ascii_lowercase();
    if lowered.contains("for this session") {
        return None;
    }

    let scoped = lowered.contains("for this project")
        || lowered.contains("for this repo")
        || lowered.contains("in this repo")
        || lowered.contains("from now on");
    let directive = lowered.contains("always ")
        || lowered.contains("never ")
        || lowered.contains("prefer ")
        || lowered.contains("avoid ")
        || lowered.contains("use ");

    if scoped && directive {
        Some(format!(
            "Explicit project preference from the user: {}",
            truncate(input, 320)
        ))
    } else {
        None
    }
}

fn tokenize(value: &str) -> BTreeSet<String> {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect()
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

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn atomic_temp_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("memory.json");
    path.with_file_name(format!(".{file_name}.{nanos}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::{MemoryKind, MemoryScope, MemoryStore};
    use crate::{ConversationMessage, ContentBlock, TurnSummary, UsageTracker};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("claw-memory-{nanos}.json"))
    }

    fn summary(tool_name: &str, is_error: bool) -> TurnSummary {
        TurnSummary {
            assistant_messages: vec![ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "done".to_string(),
            }])],
            tool_results: vec![ConversationMessage::tool_result(
                "tool-1",
                tool_name,
                if is_error { "failed" } else { "ok" },
                is_error,
            )],
            iterations: 1,
            usage: UsageTracker::from_session(&crate::Session::new()).cumulative_usage(),
        }
    }

    #[test]
    fn learns_episode_and_lesson_from_tool_error() {
        let mut store = MemoryStore::load(temp_path()).expect("store should load");
        let report = store.learn_from_turn(
            "fix the build",
            &summary("cargo_check", true),
        );
        assert_eq!(report.created, 2);
        assert_eq!(report.errors_observed, 1);
        assert!(store
            .entries()
            .iter()
            .any(|entry| entry.kind == MemoryKind::Episode));
        assert!(store
            .entries()
            .iter()
            .any(|entry| entry.kind == MemoryKind::Lesson));
    }

    #[test]
    fn successful_multi_tool_turn_creates_strategy() {
        let mut store = MemoryStore::load(temp_path()).expect("store should load");
        let result = TurnSummary {
            assistant_messages: Vec::new(),
            tool_results: vec![
                ConversationMessage::tool_result("1", "read_file", "ok", false),
                ConversationMessage::tool_result("2", "edit_file", "ok", false),
            ],
            iterations: 2,
            usage: UsageTracker::from_session(&crate::Session::new()).cumulative_usage(),
        };
        let report = store.learn_from_turn("fix parser", &result);
        assert_eq!(report.created, 2);
        let strategy = store
            .entries()
            .iter()
            .find(|entry| entry.kind == MemoryKind::Strategy)
            .expect("strategy should be recorded");
        assert!(strategy.content.contains("read_file -> edit_file"));
    }

    #[test]
    fn explicit_preferences_are_project_scoped_and_session_only_directives_are_ignored() {
        let mut store = MemoryStore::load(temp_path()).expect("store should load");
        let preference = TurnSummary {
            assistant_messages: Vec::new(),
            tool_results: Vec::new(),
            iterations: 0,
            usage: UsageTracker::from_session(&crate::Session::new()).cumulative_usage(),
        };
        let first = store.learn_from_turn(
            "For this project, always run cargo fmt before committing.",
            &preference,
        );
        assert_eq!(first.created, 1);
        assert_eq!(store.entries()[0].scope, MemoryScope::Project);

        let second = store.learn_from_turn(
            "For this session, always avoid the generated folder.",
            &preference,
        );
        assert_eq!(second.created, 0);
        assert_eq!(store.entries().len(), 1);
    }

    #[test]
    fn search_ranks_matching_preferences() {
        let mut store = MemoryStore::load(temp_path()).expect("store should load");
        let result = TurnSummary {
            assistant_messages: Vec::new(),
            tool_results: Vec::new(),
            iterations: 0,
            usage: UsageTracker::from_session(&crate::Session::new()).cumulative_usage(),
        };
        store.learn_from_turn(
            "For this project, always run cargo fmt before committing.",
            &result,
        );
        assert_eq!(store.search("cargo fmt", 3).len(), 1);
        assert!(store.context_for("cargo fmt", 3).is_some());
    }

    #[test]
    fn persists_and_restores_memory() {
        let path = temp_path();
        let mut store = MemoryStore::load(&path).expect("store should load");
        let result = TurnSummary {
            assistant_messages: Vec::new(),
            tool_results: Vec::new(),
            iterations: 0,
            usage: UsageTracker::from_session(&crate::Session::new()).cumulative_usage(),
        };
        store.learn_from_turn(
            "For this project, always use cargo fmt.",
            &result,
        );
        store.save().expect("store should save");

        let restored = MemoryStore::load(&path).expect("store should restore");
        assert_eq!(restored.entries().len(), 1);
        std::fs::remove_file(path).expect("memory file should be removable");
    }
}
