use std::fmt;
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

/// Runtime-owned lifecycle state for one background agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentLifecycle {
    status: AgentStatus,
}

impl AgentLifecycle {
    #[must_use]
    pub const fn new() -> Self {
        Self { status: AgentStatus::Queued }
    }

    #[must_use]
    pub const fn status(self) -> AgentStatus {
        self.status
    }

    pub fn transition(&mut self, next: AgentStatus) -> Result<(), AgentTransitionError> {
        if self.status.can_transition_to(next) {
            self.status = next;
            Ok(())
        } else {
            Err(AgentTransitionError { from: self.status, to: next })
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        self.status.is_terminal()
    }
}

impl Default for AgentLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentTransitionError {
    from: AgentStatus,
    to: AgentStatus,
}

impl AgentTransitionError {
    #[must_use]
    pub const fn from(self) -> AgentStatus {
        self.from
    }

    #[must_use]
    pub const fn to(self) -> AgentStatus {
        self.to
    }
}

impl fmt::Display for AgentTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid agent lifecycle transition: {:?} -> {:?}", self.from, self.to)
    }
}

impl std::error::Error for AgentTransitionError {}

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

    #[must_use]
    pub fn configured_from_env() -> Self {
        let limit = std::env::var("CLAW_MAX_PARALLEL_AGENTS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| (1..=8).contains(value))
            .unwrap_or(2);
        Self::new(limit)
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
    use super::{atomic_write, AgentCoordinator, AgentLifecycle, AgentStatus};
    use std::fs;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("claw-agent-lifecycle-{nanos}-{name}"))
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn lifecycle_transitions_are_explicit() {
        let mut lifecycle = AgentLifecycle::new();
        assert_eq!(lifecycle.status(), AgentStatus::Queued);
        assert!(!lifecycle.is_terminal());
        lifecycle
            .transition(AgentStatus::Running)
            .expect("queued -> running");
        lifecycle
            .transition(AgentStatus::Succeeded)
            .expect("running -> succeeded");
        assert!(lifecycle.is_terminal());
    }

    #[test]
    fn lifecycle_rejects_terminal_revival() {
        let mut lifecycle = AgentLifecycle::new();
        lifecycle
            .transition(AgentStatus::Running)
            .expect("queued -> running");
        lifecycle
            .transition(AgentStatus::Failed)
            .expect("running -> failed");
        let error = lifecycle
            .transition(AgentStatus::Running)
            .expect_err("terminal state must not revive");
        assert_eq!(error.from(), AgentStatus::Failed);
        assert_eq!(error.to(), AgentStatus::Running);
    }

    #[test]
    fn lifecycle_allows_queued_cancellation() {
        let mut lifecycle = AgentLifecycle::new();
        lifecycle
            .transition(AgentStatus::Cancelled)
            .expect("queued -> cancelled");
        assert!(lifecycle.is_terminal());
    }

    #[test]
    fn coordinator_reads_bounded_env_limit() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original = std::env::var("CLAW_MAX_PARALLEL_AGENTS").ok();
        std::env::set_var("CLAW_MAX_PARALLEL_AGENTS", "3");

        let coordinator = AgentCoordinator::configured_from_env();
        let first = coordinator.try_acquire().expect("first slot");
        let second = coordinator.try_acquire().expect("second slot");
        let third = coordinator.try_acquire().expect("third slot");
        assert!(coordinator.try_acquire().is_err());
        drop(third);
        drop(second);
        drop(first);
        assert_eq!(coordinator.active_count(), 0);

        match original {
            Some(value) => std::env::set_var("CLAW_MAX_PARALLEL_AGENTS", value),
            None => std::env::remove_var("CLAW_MAX_PARALLEL_AGENTS"),
        }
    }

    #[test]
    fn coordinator_defaults_invalid_env_to_two() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original = std::env::var("CLAW_MAX_PARALLEL_AGENTS").ok();
        std::env::set_var("CLAW_MAX_PARALLEL_AGENTS", "999");

        let coordinator = AgentCoordinator::configured_from_env();
        let first = coordinator.try_acquire().expect("first slot");
        let second = coordinator.try_acquire().expect("second slot");
        assert!(coordinator.try_acquire().is_err());
        drop(second);
        drop(first);

        match original {
            Some(value) => std::env::set_var("CLAW_MAX_PARALLEL_AGENTS", value),
            None => std::env::remove_var("CLAW_MAX_PARALLEL_AGENTS"),
        }
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
}
