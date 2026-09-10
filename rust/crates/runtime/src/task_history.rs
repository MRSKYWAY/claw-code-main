use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{SubagentSnapshot, SubagentState};

const HISTORY_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TaskHistoryFile {
    version: u32,
    tasks: Vec<PersistedTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PersistedTask {
    id: String,
    parent_id: Option<String>,
    description: String,
    state: PersistedState,
    result: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PersistedState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl From<SubagentState> for PersistedState {
    fn from(state: SubagentState) -> Self {
        match state {
            SubagentState::Queued => Self::Queued,
            SubagentState::Running => Self::Running,
            SubagentState::Succeeded => Self::Succeeded,
            SubagentState::Failed => Self::Failed,
            SubagentState::Cancelled => Self::Cancelled,
        }
    }
}

impl From<PersistedState> for SubagentState {
    fn from(state: PersistedState) -> Self {
        match state {
            PersistedState::Queued => Self::Queued,
            PersistedState::Running => Self::Running,
            PersistedState::Succeeded => Self::Succeeded,
            PersistedState::Failed => Self::Failed,
            PersistedState::Cancelled => Self::Cancelled,
        }
    }
}

impl From<&SubagentSnapshot> for PersistedTask {
    fn from(snapshot: &SubagentSnapshot) -> Self {
        Self {
            id: snapshot.id.clone(),
            parent_id: snapshot.parent_id.clone(),
            description: snapshot.description.clone(),
            state: snapshot.state.into(),
            result: snapshot.result.clone(),
            error: snapshot.error.clone(),
        }
    }
}

impl From<PersistedTask> for SubagentSnapshot {
    fn from(task: PersistedTask) -> Self {
        Self {
            id: task.id,
            parent_id: task.parent_id,
            description: task.description,
            state: task.state.into(),
            result: task.result,
            error: task.error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskHistoryError {
    Io(String),
    Format(String),
}

impl std::fmt::Display for TaskHistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) | Self::Format(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for TaskHistoryError {}

#[derive(Debug, Clone)]
pub struct TaskHistory {
    path: PathBuf,
}

impl TaskHistory {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Vec<SubagentSnapshot>, TaskHistoryError> {
        if !self.path.is_file() {
            return Ok(Vec::new());
        }
        let contents = fs::read_to_string(&self.path)
            .map_err(|error| TaskHistoryError::Io(format!("failed to read task history: {error}")))?;
        let file = serde_json::from_str::<TaskHistoryFile>(&contents)
            .map_err(|error| TaskHistoryError::Format(format!("invalid task history: {error}")))?;
        if file.version != HISTORY_VERSION {
            return Err(TaskHistoryError::Format(format!(
                "unsupported task history version: {}",
                file.version
            )));
        }
        Ok(file
            .tasks
            .into_iter()
            .map(SubagentSnapshot::from)
            .collect())
    }

    pub fn save(&self, snapshots: &[SubagentSnapshot]) -> Result<(), TaskHistoryError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                TaskHistoryError::Io(format!("failed to create task history directory: {error}"))
            })?;
        }

        let file = TaskHistoryFile {
            version: HISTORY_VERSION,
            tasks: snapshots.iter().map(PersistedTask::from).collect(),
        };
        let contents = serde_json::to_vec_pretty(&file)
            .map_err(|error| TaskHistoryError::Format(format!("failed to encode task history: {error}")))?;
        let temp_path = atomic_temp_path(&self.path);
        let mut handle = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|error| TaskHistoryError::Io(format!("failed to create task history temp file: {error}")))?;

        let result = (|| -> Result<(), TaskHistoryError> {
            handle.write_all(&contents).map_err(|error| {
                TaskHistoryError::Io(format!("failed to write task history: {error}"))
            })?;
            handle.sync_all().map_err(|error| {
                TaskHistoryError::Io(format!("failed to sync task history: {error}"))
            })?;
            drop(handle);
            fs::rename(&temp_path, &self.path).map_err(|error| {
                TaskHistoryError::Io(format!("failed to replace task history: {error}"))
            })?;
            Ok(())
        })();

        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }
}

fn atomic_temp_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("tasks.json");
    path.with_file_name(format!(".{file_name}.{nanos}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::TaskHistory;
    use crate::{SubagentSnapshot, SubagentState};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("claw-task-history-{nanos}.json"))
    }

    #[test]
    fn saves_and_restores_task_snapshots() {
        let path = temp_path();
        let history = TaskHistory::new(&path);
        let snapshots = vec![SubagentSnapshot {
            id: "task-1".to_string(),
            parent_id: Some("parent-1".to_string()),
            description: "research".to_string(),
            state: SubagentState::Succeeded,
            result: Some("done".to_string()),
            error: None,
        }];

        history.save(&snapshots).expect("history should save");
        let restored = history.load().expect("history should load");
        assert_eq!(restored, snapshots);
        fs::remove_file(path).expect("history file should be removable");
    }

    #[test]
    fn missing_history_is_empty() {
        let history = TaskHistory::new(temp_path());
        assert!(history.load().expect("missing history should load").is_empty());
    }
}
