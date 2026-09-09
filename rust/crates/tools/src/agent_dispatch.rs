use crate::agent_lifecycle::{AgentCoordinator, AgentLifecycle, AgentStatus};

/// Coordinates admission and lifecycle transitions for one background agent.
///
/// The dispatcher owns the lifecycle while the supplied worker performs the
/// actual agent execution. This keeps concurrency admission and terminal-state
/// transitions in one place for callers that cannot yet depend on the larger
/// Agent tool implementation.
pub struct AgentDispatcher<'a> {
    coordinator: &'a AgentCoordinator,
}

impl<'a> AgentDispatcher<'a> {
    #[must_use]
    pub const fn new(coordinator: &'a AgentCoordinator) -> Self {
        Self { coordinator }
    }

    /// Admit an agent and run its worker synchronously under the lifecycle
    /// state machine. The permit is released automatically when the worker
    /// returns, including on errors and panics caught by the caller.
    pub fn dispatch<F>(&self, worker: F) -> Result<AgentStatus, String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        let _permit = self.coordinator.try_acquire()?;
        let mut lifecycle = AgentLifecycle::new();
        lifecycle
            .transition(AgentStatus::Running)
            .map_err(|error| error.to_string())?;

        match worker() {
            Ok(()) => {
                lifecycle
                    .transition(AgentStatus::Succeeded)
                    .map_err(|error| error.to_string())?;
            }
            Err(error) => {
                lifecycle
                    .transition(AgentStatus::Failed)
                    .map_err(|transition| transition.to_string())?;
                return Err(error);
            }
        }

        Ok(lifecycle.status())
    }
}

#[cfg(test)]
mod tests {
    use super::AgentDispatcher;
    use crate::agent_lifecycle::{AgentCoordinator, AgentStatus};

    #[test]
    fn dispatch_transitions_successfully() {
        let coordinator = AgentCoordinator::new(1);
        let dispatcher = AgentDispatcher::new(&coordinator);

        let status = dispatcher
            .dispatch(|| Ok(()))
            .expect("worker succeeds");

        assert_eq!(status, AgentStatus::Succeeded);
        assert_eq!(coordinator.active_count(), 0);
    }

    #[test]
    fn dispatch_releases_capacity_after_failure() {
        let coordinator = AgentCoordinator::new(1);
        let dispatcher = AgentDispatcher::new(&coordinator);

        let error = dispatcher
            .dispatch(|| Err(String::from("worker failed")))
            .expect_err("worker fails");
        assert_eq!(error, "worker failed");
        assert_eq!(coordinator.active_count(), 0);

        let status = dispatcher
            .dispatch(|| Ok(()))
            .expect("slot released after failure");
        assert_eq!(status, AgentStatus::Succeeded);
    }

    #[test]
    fn dispatch_rejects_when_at_capacity() {
        let coordinator = AgentCoordinator::new(1);
        let permit = coordinator.try_acquire().expect("reserve only slot");
        let dispatcher = AgentDispatcher::new(&coordinator);

        let error = dispatcher
            .dispatch(|| Ok(()))
            .expect_err("capacity is exhausted");
        assert!(error.contains("at capacity"));
        drop(permit);
    }
}
