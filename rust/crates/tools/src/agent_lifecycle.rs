use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Persisted lifecycle state for a background agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl AgentStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        use AgentStatus::{Cancelled, Failed, Queued, Running, Succeeded};
        matches!(
            (self, next),
            (Queued, Running)
                | (Queued, Failed)
                | (Queued, Cancelled)
                | (Running, Succeeded)
                | (Running, Failed)
                | (Running, Cancelled)
        )
    }
}

/// Small in-process coordinator used to enforce a bounded number of active agents.
#[derive(Debug)]
pub struct AgentCoordinator {
    active: Mutex<usize>,
    limit: usize,
}

impl AgentCoordinator {
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            active: Mutex::new(0),
            limit: limit.max(1),
        }
    }

    pub fn try_acquire(&self) -> Result<AgentPermit<'_>, String> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| String::from("agent coordinator lock poisoned"))?;
        if *active >= self.limit {
            return Err(format!(
                "agent coordinator is at capacity ({}); wait for an active agent",
                self.limit
            ));
        }
        *active += 1;
        Ok(AgentPermit { coordinator: self })
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active.lock().map(|value| *value).unwrap_or(self.limit)
    }
}

pub struct AgentPermit<'a> {
    coordinator: &'a AgentCoordinator,
}

impl Drop for AgentPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.coordinator.active.lock() {
            *active = active.saturating_sub(1);
        }
    }
}

/// Atomically replace a UTF-8 file using an adjacent temporary file.
///
/// The temporary file is flushed and synced before rename. A failed rename
/// leaves the original destination untouched and cleans up the temporary file.
pub fn atomic_write(path: &Path, contents: &str) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let tmp_path = unique_temp_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp_path, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

fn unique_temp_path(path: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("agent-state");
    path.with_file_name(format!(".{stem}.{nanos}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::{atomic_write, AgentCoordinator, AgentStatus};
    use std::fs;
    use std::sync::Arc;
    use std::thread;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("claw-agent-lifecycle-{nanos}-{name}"))
    }

    #[test]
    fn lifecycle_transitions_are_explicit() {
        assert!(AgentStatus::Queued.can_transition_to(AgentStatus::Running));
        assert!(AgentStatus::Running.can_transition_to(AgentStatus::Succeeded));
        assert!(AgentStatus::Running.can_transition_to(AgentStatus::Failed));
        assert!(AgentStatus::Running.can_transition_to(AgentStatus::Cancelled));
        assert!(!AgentStatus::Succeeded.can_transition_to(AgentStatus::Running));
        assert!(!AgentStatus::Failed.can_transition_to(AgentStatus::Succeeded));
        assert!(AgentStatus::Cancelled.is_terminal());
    }

    #[test]
    fn coordinator_enforces_capacity_and_releases_on_drop() {
        let coordinator = Arc::new(AgentCoordinator::new(1));
        let permit = coordinator.try_acquire().expect("first slot");
        assert_eq!(coordinator.active_count(), 1);
        assert!(coordinator.try_acquire().is_err());
        drop(permit);
        assert_eq!(coordinator.active_count(), 0);
        assert!(coordinator.try_acquire().is_ok());
    }

    #[test]
    fn coordinator_is_safe_across_threads() {
        let coordinator = Arc::new(AgentCoordinator::new(2));
        let workers = (0..8)
            .map(|_| {
                let coordinator = Arc::clone(&coordinator);
                thread::spawn(move || coordinator.try_acquire().is_ok())
            })
            .collect::<Vec<_>>();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker"))
            .collect::<Vec<_>>();
        assert!(results.iter().any(|acquired| *acquired));
    }

    #[test]
    fn atomic_write_replaces_contents_without_temp_files() {
        let dir = temp_path("store");
        fs::create_dir_all(&dir).expect("create directory");
        let path = dir.join("agent.json");
        atomic_write(&path, "first").expect("first write");
        atomic_write(&path, "second").expect("replacement write");
        assert_eq!(fs::read_to_string(&path).expect("read file"), "second");

        let tmp_files = fs::read_dir(&dir)
            .expect("read directory")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("tmp"))
            .count();
        assert_eq!(tmp_files, 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_write_cleans_up_after_failed_rename() {
        let dir = temp_path("rename-failure");
        fs::create_dir_all(&dir).expect("create directory");
        let path = dir.join("agent.json");
        fs::create_dir(&path).expect("create destination directory");

        let error = atomic_write(&path, "data").expect_err("rename onto directory should fail");
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::AlreadyExists
                | std::io::ErrorKind::PermissionDenied
                | std::io::ErrorKind::InvalidInput
        ));

        let tmp_files = fs::read_dir(&dir)
            .expect("read directory")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("tmp"))
            .count();
        assert_eq!(tmp_files, 0);
        assert!(path.is_dir());
        let _ = fs::remove_dir_all(dir);
    }
}
