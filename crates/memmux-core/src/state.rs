//! Task lifecycle state machine (§6.2 / §6.3).
//!
//! The full legal-transition table and property tests are a Phase 1 deliverable (SUM-42);
//! this module seeds the enum, residency semantics, and a conservative transition table so
//! earlier crates can reason about state without duplicating the definitions.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Lifecycle state of a logical task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskState {
    /// Durable task record exists but nothing has been scheduled.
    Created,
    /// Durable but not yet granted execution resources.
    Queued,
    /// Reservation is being acquired.
    Admitting,
    /// Provider process is starting.
    Starting,
    /// Provider can accept or produce work.
    Active,
    /// A child command/service is doing task-critical work.
    ToolRunning,
    /// Provider awaits user approval or input.
    WaitingUser,
    /// A dependency is incomplete.
    Blocked,
    /// No meaningful activity within the policy window.
    Idle,
    /// Capturing durable checkpoint state.
    Checkpointing,
    /// Provider and private descendants stopped; durable task remains.
    Hibernated,
    /// Provider runtime is being restarted at a safe point.
    Recycling,
    /// Restoring a checkpointed/hibernated task.
    Resuming,
    /// Task failed and is not automatically continued.
    Failed,
    /// Task is being torn down.
    Terminating,
    /// Fully torn down.
    Terminated,
}

impl TaskState {
    /// Whether a provider process is expected to be resident in this state.
    ///
    /// Follows the "Resident?" column of the §6.3 table. `WaitingUser` and `Blocked` are
    /// "usually / optional" resident; we treat `WaitingUser` as resident (candidate for
    /// delayed hibernation) and `Blocked` as non-resident (should not hold a slot).
    pub fn is_resident(self) -> bool {
        matches!(
            self,
            TaskState::Starting
                | TaskState::Active
                | TaskState::ToolRunning
                | TaskState::WaitingUser
                | TaskState::Idle
                | TaskState::Checkpointing
                | TaskState::Recycling
                | TaskState::Resuming
        )
    }

    /// Whether this is a terminal state with no automatic continuation.
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskState::Terminated)
    }

    /// States this state may legally transition into.
    ///
    /// `Terminating` is reachable from every non-terminal state (`ANY -> TERMINATING`).
    pub fn allowed_next(self) -> &'static [TaskState] {
        use TaskState::*;
        match self {
            Created => &[Queued, Terminating],
            Queued => &[Admitting, Blocked, Terminating],
            Admitting => &[Starting, Queued, Failed, Terminating],
            Starting => &[Active, Failed, Terminating],
            Active => &[
                ToolRunning,
                WaitingUser,
                Blocked,
                Idle,
                Checkpointing,
                Recycling,
                Failed,
                Terminating,
            ],
            ToolRunning => &[Active, WaitingUser, Failed, Terminating],
            WaitingUser => &[Active, Idle, Checkpointing, Terminating],
            Blocked => &[Queued, Active, Terminating],
            Idle => &[Active, Checkpointing, Terminating],
            Checkpointing => &[Hibernated, Active, Failed, Terminating],
            Hibernated => &[Queued, Resuming, Terminating],
            Recycling => &[Resuming, Failed, Terminating],
            Resuming => &[Active, Failed, Terminating],
            Failed => &[Queued, Terminating],
            Terminating => &[Terminated],
            Terminated => &[],
        }
    }

    /// Whether a transition from `self` to `next` is legal.
    pub fn can_transition_to(self, next: TaskState) -> bool {
        self.allowed_next().contains(&next)
    }

    /// Attempt a transition, returning an error describing the illegal move.
    pub fn transition_to(self, next: TaskState) -> Result<TaskState, IllegalTransition> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(IllegalTransition {
                from: self,
                to: next,
            })
        }
    }
}

impl fmt::Display for TaskState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use TaskState::*;
        let s = match self {
            Created => "CREATED",
            Queued => "QUEUED",
            Admitting => "ADMITTING",
            Starting => "STARTING",
            Active => "ACTIVE",
            ToolRunning => "TOOL_RUNNING",
            WaitingUser => "WAITING_USER",
            Blocked => "BLOCKED",
            Idle => "IDLE",
            Checkpointing => "CHECKPOINTING",
            Hibernated => "HIBERNATED",
            Recycling => "RECYCLING",
            Resuming => "RESUMING",
            Failed => "FAILED",
            Terminating => "TERMINATING",
            Terminated => "TERMINATED",
        };
        f.write_str(s)
    }
}

/// Error returned when an illegal state transition is attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("illegal task state transition: {from} -> {to}")]
pub struct IllegalTransition {
    /// State the task was in.
    pub from: TaskState,
    /// State that was illegally requested.
    pub to: TaskState,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[TaskState] = &[
        TaskState::Created,
        TaskState::Queued,
        TaskState::Admitting,
        TaskState::Starting,
        TaskState::Active,
        TaskState::ToolRunning,
        TaskState::WaitingUser,
        TaskState::Blocked,
        TaskState::Idle,
        TaskState::Checkpointing,
        TaskState::Hibernated,
        TaskState::Recycling,
        TaskState::Resuming,
        TaskState::Failed,
        TaskState::Terminating,
        TaskState::Terminated,
    ];

    #[test]
    fn canonical_happy_path_is_legal() {
        let path = [
            TaskState::Created,
            TaskState::Queued,
            TaskState::Admitting,
            TaskState::Starting,
            TaskState::Active,
            TaskState::ToolRunning,
            TaskState::Active,
            TaskState::Idle,
            TaskState::Checkpointing,
            TaskState::Hibernated,
            TaskState::Resuming,
            TaskState::Active,
            TaskState::Terminating,
            TaskState::Terminated,
        ];
        for pair in path.windows(2) {
            assert!(
                pair[0].can_transition_to(pair[1]),
                "expected legal transition {} -> {}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn every_nonterminal_state_can_terminate() {
        for &s in ALL {
            if s == TaskState::Terminated {
                continue;
            }
            let can_reach_terminating =
                s == TaskState::Terminating || s.can_transition_to(TaskState::Terminating);
            assert!(can_reach_terminating, "{s} cannot reach termination");
        }
    }

    #[test]
    fn terminated_is_a_sink() {
        assert!(TaskState::Terminated.allowed_next().is_empty());
        assert!(TaskState::Terminated.is_terminal());
    }

    #[test]
    fn illegal_transition_reports_endpoints() {
        let err = TaskState::Queued
            .transition_to(TaskState::Active)
            .unwrap_err();
        assert_eq!(err.from, TaskState::Queued);
        assert_eq!(err.to, TaskState::Active);
    }

    #[test]
    fn residency_matches_spec_examples() {
        assert!(TaskState::Active.is_resident());
        assert!(TaskState::ToolRunning.is_resident());
        assert!(!TaskState::Queued.is_resident());
        assert!(!TaskState::Hibernated.is_resident());
        assert!(!TaskState::Terminated.is_resident());
    }

    #[test]
    fn state_serializes_screaming_snake_case() {
        let json = serde_json::to_string(&TaskState::ToolRunning).unwrap();
        assert_eq!(json, "\"TOOL_RUNNING\"");
        let back: TaskState = serde_json::from_str("\"WAITING_USER\"").unwrap();
        assert_eq!(back, TaskState::WaitingUser);
    }
}
