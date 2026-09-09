mod agent_lifecycle;

include!("lib_core.rs");

pub use agent_lifecycle::{atomic_write, AgentCoordinator, AgentStatus};
