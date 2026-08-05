//! Safe-point detection (§13.3 / SUM-91).
//!
//! A *safe point* is a moment when a task can be frozen — for hibernation or recycling — without
//! corrupting a tool invocation or a half-written file. Detection is deliberately **conservative**:
//! when in doubt it says "wait", never "force". The same logic backs both hibernation (SUM-90/92)
//! and recycling (SUM-94/95), so both share one definition of "safe".

use memmux_core::TaskState;

/// The verdict for a single moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SafePoint {
    /// It is safe to freeze the task now.
    Ready,
    /// Not safe yet; the string explains why (surfaced in decision events).
    Wait(String),
}

impl SafePoint {
    /// Whether the task can be frozen right now.
    pub fn is_ready(&self) -> bool {
        matches!(self, SafePoint::Ready)
    }

    /// The wait reason, if any.
    pub fn reason(&self) -> Option<&str> {
        match self {
            SafePoint::Wait(r) => Some(r),
            SafePoint::Ready => None,
        }
    }
}

/// A snapshot of what a task is doing, used to assess a safe point.
#[derive(Clone, Copy, Debug)]
pub struct ActivitySnapshot {
    /// The task's current resident sub-state.
    pub sub_state: TaskState,
    /// A task-critical child command is currently running.
    pub tool_running: bool,
    /// The task is mid-write to the repository (a checkpoint now could tear a file).
    pub writing: bool,
}

/// Assess whether `a` represents a safe point to freeze at.
///
/// Safe when the provider is quiescent (`ACTIVE` between turns, `IDLE`, `WAITING_USER`, or
/// `BLOCKED`). Never safe mid-tool-call, mid-write, or during a transitional state.
pub fn assess(a: &ActivitySnapshot) -> SafePoint {
    if a.tool_running {
        return SafePoint::Wait("a tool call is in progress".into());
    }
    if a.writing {
        return SafePoint::Wait("a repository write is in progress".into());
    }
    match a.sub_state {
        TaskState::ToolRunning => SafePoint::Wait("provider is running a tool".into()),
        TaskState::Admitting
        | TaskState::Starting
        | TaskState::Checkpointing
        | TaskState::Recycling
        | TaskState::Resuming
        | TaskState::Terminating => SafePoint::Wait("transitional state".into()),
        // Quiescent / freezable states.
        TaskState::Active
        | TaskState::Idle
        | TaskState::WaitingUser
        | TaskState::Blocked
        | TaskState::Created
        | TaskState::Queued
        | TaskState::Hibernated
        | TaskState::Failed
        | TaskState::Terminated => SafePoint::Ready,
    }
}

/// The outcome of polling a [`SafePointWaiter`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitDecision {
    /// A safe point was reached; proceed with the freeze.
    Proceed,
    /// Not safe yet; keep waiting (reason for observability).
    KeepWaiting(String),
    /// The deadline passed without a safe point. The freeze is **abandoned**, not forced —
    /// the task keeps running and the caller may retry later.
    Abandon(String),
}

/// Waits for a safe point up to a deadline, then gives up rather than forcing a freeze.
///
/// This encodes the SUM-91 acceptance criterion "conservative: waits for a safe point rather than
/// forcing". `poll` is pure over the injected clock, so it is fully unit-testable.
#[derive(Clone, Copy, Debug)]
pub struct SafePointWaiter {
    started_ms: u64,
    deadline_ms: u64,
}

impl SafePointWaiter {
    /// Start waiting at `now_ms`, giving up `timeout_ms` later.
    pub fn new(now_ms: u64, timeout_ms: u64) -> Self {
        Self {
            started_ms: now_ms,
            deadline_ms: now_ms.saturating_add(timeout_ms),
        }
    }

    /// Assess the current moment, honouring the deadline.
    pub fn poll(&self, a: &ActivitySnapshot, now_ms: u64) -> WaitDecision {
        match assess(a) {
            SafePoint::Ready => WaitDecision::Proceed,
            SafePoint::Wait(reason) => {
                if now_ms >= self.deadline_ms {
                    WaitDecision::Abandon(format!(
                        "no safe point within {} ms ({reason})",
                        self.deadline_ms.saturating_sub(self.started_ms)
                    ))
                } else {
                    WaitDecision::KeepWaiting(reason)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(sub: TaskState, tool: bool, writing: bool) -> ActivitySnapshot {
        ActivitySnapshot {
            sub_state: sub,
            tool_running: tool,
            writing,
        }
    }

    #[test]
    fn quiescent_states_are_safe() {
        for s in [TaskState::Active, TaskState::Idle, TaskState::WaitingUser] {
            assert!(
                assess(&snap(s, false, false)).is_ready(),
                "{s} should be safe"
            );
        }
    }

    #[test]
    fn tool_or_write_in_progress_is_never_safe() {
        assert!(!assess(&snap(TaskState::Active, true, false)).is_ready());
        assert!(!assess(&snap(TaskState::Active, false, true)).is_ready());
        assert!(!assess(&snap(TaskState::ToolRunning, false, false)).is_ready());
    }

    #[test]
    fn wait_reason_is_populated() {
        let sp = assess(&snap(TaskState::Active, true, false));
        assert!(sp.reason().unwrap().contains("tool"));
    }

    #[test]
    fn waiter_proceeds_when_safe() {
        let w = SafePointWaiter::new(1_000, 5_000);
        assert_eq!(
            w.poll(&snap(TaskState::Idle, false, false), 1_100),
            WaitDecision::Proceed
        );
    }

    #[test]
    fn waiter_keeps_waiting_before_deadline() {
        let w = SafePointWaiter::new(1_000, 5_000);
        match w.poll(&snap(TaskState::Active, true, false), 2_000) {
            WaitDecision::KeepWaiting(r) => assert!(r.contains("tool")),
            other => panic!("expected KeepWaiting, got {other:?}"),
        }
    }

    #[test]
    fn waiter_abandons_after_deadline_without_forcing() {
        let w = SafePointWaiter::new(1_000, 5_000);
        // Still unsafe at t=7000 (past the 6000 deadline): abandon, do not force.
        match w.poll(&snap(TaskState::ToolRunning, true, true), 7_000) {
            WaitDecision::Abandon(r) => assert!(r.contains("no safe point")),
            other => panic!("expected Abandon, got {other:?}"),
        }
    }
}
