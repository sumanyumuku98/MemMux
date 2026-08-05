//! # memmux-core
//!
//! Shared, dependency-light domain types for the MemMux runtime. Every other crate
//! (`memmuxd`, `memmux` TUI, `memmux-metrics`, `memmux-bench`) speaks in terms of the
//! entities defined here so the durable product contract stays in one place.
//!
//! The module layout mirrors §6 (Task and Agent Runtime Model) and §15 (Eventing) of the
//! MemMux V2 technical specification.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod event;
pub mod ids;
pub mod runtime;
pub mod state;
pub mod task;

pub use event::{Event, EventCategory, EventSeverity, EventType};
pub use ids::{Pid, RepositoryId, RuntimeInstanceId, TaskId};
pub use runtime::{classify_substate, ActivitySignals, StateTransition, Task};
pub use state::{IllegalTransition, TaskState};
pub use task::{
    Isolation, McpProfile, Priority, Provider, ResourceClass, TaskPolicies, TaskSpec, ToolProfile,
};
