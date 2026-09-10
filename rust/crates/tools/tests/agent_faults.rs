use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use tools::agent_lifecycle::{AgentCoordinator, AgentLifecycle, AgentStatus};

#[test]
fn capacity_is_released_after_worker_panic() {
    let coordinator = Arc::new(AgentCoordinator::new(1));
    let barrier = Arc::new(Barrier::new(2));
    let coordinator_for_worker = Arc::clone(&coordinator);
    let barrier_for_worker = Arc::clone(&barrier);

    let handle = thread::spawn(move || {
        let permit = coordinator_for_worker
            .try_acquire()
            .expect("worker should acquire capacity");
        barrier_for_worker.wait();
        let _permit = permit;
        let result = catch_unwind(AssertUnwindSafe(|| panic!("simulated sub-agent crash")));
        assert!(result.is_err());
    });

    barrier.wait();
    handle.join().expect("worker panic should be contained by the guard");
    assert_eq!(coordinator.active_count(), 0);
    assert!(coordinator.try_acquire().is_ok());
}

#[test]
fn terminal_lifecycle_state_is_not_reused_after_failure() {
    let lifecycle = Arc::new(Mutex::new(AgentLifecycle::new()));
    {
        let mut state = lifecycle.lock().expect("lifecycle lock");
        state.transition(AgentStatus::Running).expect("queued -> running");
        state.transition(AgentStatus::Failed).expect("running -> failed");
        assert!(state.is_terminal());
    }

    let state = lifecycle.lock().expect("lifecycle lock");
    assert_eq!(state.status(), AgentStatus::Failed);
    assert!(state.transition(AgentStatus::Running).is_err());
}
