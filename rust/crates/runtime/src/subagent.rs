use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;

use crate::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl SubagentState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        use SubagentState::{Cancelled, Failed, Queued, Running, Succeeded};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentSnapshot {
    pub id: String,
    pub parent_id: Option<String>,
    pub description: String,
    pub state: SubagentState,
    pub result: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug)]
struct SubagentRecord {
    snapshot: SubagentSnapshot,
    cancellation: CancellationToken,
}

#[derive(Debug, Clone, Default)]
pub struct SubagentRegistry {
    inner: Arc<Mutex<BTreeMap<String, SubagentRecord>>>,
}

impl SubagentRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spawn<F, Fut>(
        &self,
        id: impl Into<String>,
        parent_id: Option<String>,
        description: impl Into<String>,
        task: F,
    ) -> Result<SubagentHandle, SubagentError>
    where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        let id = id.into();
        let description = description.into();
        let cancellation = CancellationToken::new();
        let snapshot = SubagentSnapshot {
            id: id.clone(),
            parent_id,
            description,
            state: SubagentState::Queued,
            result: None,
            error: None,
        };

        let mut records = self
            .inner
            .lock()
            .map_err(|_| SubagentError::RegistryPoisoned)?;
        if records.contains_key(&id) {
            return Err(SubagentError::DuplicateId(id));
        }
        records.insert(
            id.clone(),
            SubagentRecord {
                snapshot,
                cancellation: cancellation.clone(),
            },
        );
        drop(records);

        let registry = self.clone();
        let task_id = id.clone();
        let join = tokio::spawn(async move {
            if cancellation.is_cancelled() {
                return;
            }
            if registry
                .transition(&task_id, SubagentState::Running)
                .is_err()
            {
                return;
            }
            if cancellation.is_cancelled() {
                let _ = registry.transition(&task_id, SubagentState::Cancelled);
                return;
            }

            let result = task(cancellation.clone()).await;
            match result {
                Ok(value) => {
                    if cancellation.is_cancelled() {
                        let _ = registry.transition(&task_id, SubagentState::Cancelled);
                    } else {
                        let _ = registry.complete(&task_id, value);
                    }
                }
                Err(error) => {
                    if cancellation.is_cancelled() {
                        let _ = registry.transition(&task_id, SubagentState::Cancelled);
                    } else {
                        let _ = registry.fail(&task_id, error);
                    }
                }
            }
        });

        Ok(SubagentHandle {
            id,
            registry: self.clone(),
            join: Some(join),
        })
    }

    pub fn cancel(&self, id: &str) -> Result<bool, SubagentError> {
        let records = self
            .inner
            .lock()
            .map_err(|_| SubagentError::RegistryPoisoned)?;
        let Some(record) = records.get(id) else {
            return Ok(false);
        };
        record.cancellation.cancel();
        drop(records);
        let snapshot = self
            .snapshot(id)?
            .ok_or_else(|| SubagentError::UnknownId(id.to_string()))?;
        if snapshot.state.is_terminal() {
            return Ok(true);
        }
        self.transition(id, SubagentState::Cancelled)?;
        Ok(true)
    }

    pub fn snapshot(&self, id: &str) -> Result<Option<SubagentSnapshot>, SubagentError> {
        let records = self
            .inner
            .lock()
            .map_err(|_| SubagentError::RegistryPoisoned)?;
        Ok(records.get(id).map(|record| record.snapshot.clone()))
    }

    pub fn snapshots(&self) -> Result<Vec<SubagentSnapshot>, SubagentError> {
        let records = self
            .inner
            .lock()
            .map_err(|_| SubagentError::RegistryPoisoned)?;
        Ok(records.values().map(|record| record.snapshot.clone()).collect())
    }

    fn transition(&self, id: &str, next: SubagentState) -> Result<(), SubagentError> {
        let mut records = self
            .inner
            .lock()
            .map_err(|_| SubagentError::RegistryPoisoned)?;
        let record = records
            .get_mut(id)
            .ok_or_else(|| SubagentError::UnknownId(id.to_string()))?;
        if record.snapshot.state == next {
            return Ok(());
        }
        if !record.snapshot.state.can_transition_to(next) {
            return Err(SubagentError::InvalidTransition {
                from: record.snapshot.state,
                to: next,
            });
        }
        record.snapshot.state = next;
        Ok(())
    }

    fn complete(&self, id: &str, result: String) -> Result<(), SubagentError> {
        let mut records = self
            .inner
            .lock()
            .map_err(|_| SubagentError::RegistryPoisoned)?;
        let record = records
            .get_mut(id)
            .ok_or_else(|| SubagentError::UnknownId(id.to_string()))?;
        if record.snapshot.state != SubagentState::Running {
            return Err(SubagentError::InvalidTransition {
                from: record.snapshot.state,
                to: SubagentState::Succeeded,
            });
        }
        record.snapshot.state = SubagentState::Succeeded;
        record.snapshot.result = Some(result);
        Ok(())
    }

    fn fail(&self, id: &str, error: String) -> Result<(), SubagentError> {
        let mut records = self
            .inner
            .lock()
            .map_err(|_| SubagentError::RegistryPoisoned)?;
        let record = records
            .get_mut(id)
            .ok_or_else(|| SubagentError::UnknownId(id.to_string()))?;
        if record.snapshot.state != SubagentState::Running {
            return Err(SubagentError::InvalidTransition {
                from: record.snapshot.state,
                to: SubagentState::Failed,
            });
        }
        record.snapshot.state = SubagentState::Failed;
        record.snapshot.error = Some(error);
        Ok(())
    }
}

pub struct SubagentHandle {
    id: String,
    registry: SubagentRegistry,
    join: Option<JoinHandle<()>>,
}

impl fmt::Debug for SubagentHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubagentHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl SubagentHandle {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn cancel(&self) -> Result<bool, SubagentError> {
        self.registry.cancel(&self.id)
    }

    pub async fn join(mut self) -> Result<SubagentSnapshot, SubagentError> {
        if let Some(join) = self.join.take() {
            join.await
                .map_err(|error| SubagentError::Join(error.to_string()))?;
        }
        self.registry
            .snapshot(&self.id)?
            .ok_or_else(|| SubagentError::UnknownId(self.id.clone()))
    }
}

#[derive(Debug)]
pub enum SubagentError {
    DuplicateId(String),
    UnknownId(String),
    RegistryPoisoned,
    InvalidTransition {
        from: SubagentState,
        to: SubagentState,
    },
    Join(String),
}

impl fmt::Display for SubagentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(formatter, "sub-agent id already exists: {id}"),
            Self::UnknownId(id) => write!(formatter, "unknown sub-agent id: {id}"),
            Self::RegistryPoisoned => formatter.write_str("sub-agent registry lock poisoned"),
            Self::InvalidTransition { from, to } => {
                write!(formatter, "invalid sub-agent state transition: {from:?} -> {to:?}")
            }
            Self::Join(error) => write!(formatter, "sub-agent task join failed: {error}"),
        }
    }
}

impl std::error::Error for SubagentError {}

#[cfg(test)]
mod tests {
    use super::{SubagentRegistry, SubagentState};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[tokio::test]
    async fn spawn_tracks_parent_result_and_terminal_state() {
        let registry = SubagentRegistry::new();
        let handle = registry
            .spawn("child-1", Some("parent-1".to_string()), "research task", |_cancel| async {
                tokio::time::sleep(Duration::from_millis(1)).await;
                Ok("done".to_string())
            })
            .expect("spawn should succeed");

        let queued = registry.snapshot("child-1").expect("snapshot").expect("record");
        assert_eq!(queued.parent_id.as_deref(), Some("parent-1"));
        assert_eq!(queued.state, SubagentState::Queued);

        let completed = handle.join().await.expect("join should succeed");
        assert_eq!(completed.state, SubagentState::Succeeded);
        assert_eq!(completed.result.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn cancellation_is_visible_to_running_child() {
        let registry = SubagentRegistry::new();
        let handle = registry
            .spawn("child-2", None, "cancellable task", |cancel| async move {
                while !cancel.is_cancelled() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Ok("observed cancellation".to_string())
            })
            .expect("spawn should succeed");

        tokio::time::sleep(Duration::from_millis(2)).await;
        assert!(handle.cancel().expect("cancel should succeed"));
        let completed = handle.join().await.expect("join should succeed");
        assert_eq!(completed.state, SubagentState::Cancelled);
    }

    #[tokio::test]
    async fn queued_cancellation_prevents_task_execution() {
        let registry = SubagentRegistry::new();
        let started = Arc::new(Mutex::new(false));
        let started_by_task = Arc::clone(&started);
        let handle = registry
            .spawn("child-queued", None, "queued cancellation", move |_cancel| async move {
                *started_by_task.lock().expect("started lock") = true;
                Ok("should not run".to_string())
            })
            .expect("spawn should succeed");

        assert!(handle.cancel().expect("cancel should succeed"));
        let completed = handle.join().await.expect("join should succeed");
        assert_eq!(completed.state, SubagentState::Cancelled);
        assert!(!*started.lock().expect("started lock"));
    }

    #[tokio::test]
    async fn failed_child_preserves_error() {
        let registry = SubagentRegistry::new();
        let handle = registry
            .spawn("child-3", None, "failing task", |_cancel| async {
                Err("boom".to_string())
            })
            .expect("spawn should succeed");

        let completed = handle.join().await.expect("join should succeed");
        assert_eq!(completed.state, SubagentState::Failed);
        assert_eq!(completed.error.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn duplicate_ids_are_rejected() {
        let registry = SubagentRegistry::new();
        let first = registry.spawn("same", None, "first", |_cancel| async {
            Ok("done".to_string())
        });
        assert!(first.is_ok());
        let second = registry.spawn("same", None, "second", |_cancel| async {
            Ok("done".to_string())
        });
        assert!(matches!(second, Err(super::SubagentError::DuplicateId(id)) if id == "same"));
        let _ = first.expect("first spawn").join().await.expect("first join");
    }
}
