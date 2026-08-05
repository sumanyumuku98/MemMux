//! Runtime task entity and sub-state classification (SUM-41, SUM-42, SUM-43).
//!
//! [`TaskSpec`] is the durable *description* of work; [`Task`] is its live
//! runtime record — current [`TaskState`], timestamps, and an audited history of transitions.
//! Sub-state classification ([`classify_substate`]) turns raw provider/activity signals into
//! the resident sub-state (`TOOL_RUNNING` / `WAITING_USER` / `BLOCKED` / `IDLE` / `ACTIVE`) the
//! scheduler and TUI consume.

use crate::ids::TaskId;
use crate::state::{IllegalTransition, TaskState};
use crate::task::TaskSpec;
use serde::{Deserialize, Serialize};

/// One recorded state transition, for the audit trail (§15, SUM-42).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateTransition {
    /// State transitioned from.
    pub from: TaskState,
    /// State transitioned to.
    pub to: TaskState,
    /// Milliseconds since the Unix epoch when it happened.
    pub at_ms: u64,
    /// User-visible reason (every intervention is explainable, §2.4).
    pub reason: String,
}

/// The live runtime record for a logical task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Durable specification.
    pub spec: TaskSpec,
    /// Current lifecycle state.
    pub state: TaskState,
    /// Creation time (ms since epoch).
    pub created_at_ms: u64,
    /// Time of the last state change (ms since epoch).
    pub updated_at_ms: u64,
    /// Ordered history of state transitions.
    pub history: Vec<StateTransition>,
}

impl Task {
    /// Create a new task in the [`TaskState::Created`] state.
    pub fn new(spec: TaskSpec, now_ms: u64) -> Self {
        Self {
            spec,
            state: TaskState::Created,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            history: Vec::new(),
        }
    }

    /// The task's id (borrowed from its spec).
    pub fn id(&self) -> &TaskId {
        &self.spec.id
    }

    /// Attempt a state transition, recording it in history on success.
    ///
    /// Returns the recorded [`StateTransition`] (which the daemon maps to an audit event), or an
    /// [`IllegalTransition`] error leaving the task unchanged.
    pub fn transition(
        &mut self,
        to: TaskState,
        reason: impl Into<String>,
        now_ms: u64,
    ) -> Result<StateTransition, IllegalTransition> {
        self.state.transition_to(to)?;
        let transition = StateTransition {
            from: self.state,
            to,
            at_ms: now_ms,
            reason: reason.into(),
        };
        self.state = to;
        self.updated_at_ms = now_ms;
        self.history.push(transition.clone());
        Ok(transition)
    }

    /// Whether the task currently holds a resident provider process.
    pub fn is_resident(&self) -> bool {
        self.state.is_resident()
    }

    /// Milliseconds since the task was created.
    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.created_at_ms)
    }

    /// Milliseconds the task has spent in its current state.
    pub fn time_in_state_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.updated_at_ms)
    }
}

/// Raw signals used to classify a resident task's sub-state (SUM-43).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ActivitySignals {
    /// Time (ms since epoch) of the last output or tool activity.
    pub last_activity_ms: u64,
    /// The provider is awaiting user approval or input.
    pub awaiting_user_input: bool,
    /// A task-critical child command/tool is currently running.
    pub tool_running: bool,
    /// The task has at least one incomplete dependency.
    pub has_unmet_dependencies: bool,
}

/// Classify the resident sub-state from activity signals.
///
/// Precedence follows §6.3 semantics: a blocking dependency dominates, then an awaited user
/// prompt, then an in-flight tool, then idleness (no activity for `idle_after_ms`), else the
/// task is actively working.
pub fn classify_substate(signals: &ActivitySignals, now_ms: u64, idle_after_ms: u64) -> TaskState {
    if signals.has_unmet_dependencies {
        return TaskState::Blocked;
    }
    if signals.awaiting_user_input {
        return TaskState::WaitingUser;
    }
    if signals.tool_running {
        return TaskState::ToolRunning;
    }
    let idle_for = now_ms.saturating_sub(signals.last_activity_ms);
    if idle_for >= idle_after_ms {
        return TaskState::Idle;
    }
    TaskState::Active
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::Provider;

    fn spec() -> TaskSpec {
        TaskSpec::new(
            "task_1",
            "repo_1",
            "/src/product",
            "Refactor auth",
            Provider::ClaudeCode,
            "main",
        )
    }

    #[test]
    fn new_task_starts_in_created() {
        let t = Task::new(spec(), 1000);
        assert_eq!(t.state, TaskState::Created);
        assert!(t.history.is_empty());
        assert_eq!(t.id().as_str(), "task_1");
    }

    #[test]
    fn legal_transition_records_history_and_updates_time() {
        let mut t = Task::new(spec(), 1000);
        let tr = t.transition(TaskState::Queued, "submitted", 1500).unwrap();
        assert_eq!(tr.from, TaskState::Created);
        assert_eq!(tr.to, TaskState::Queued);
        assert_eq!(t.state, TaskState::Queued);
        assert_eq!(t.updated_at_ms, 1500);
        assert_eq!(t.history.len(), 1);
        assert_eq!(t.time_in_state_ms(2000), 500);
        assert_eq!(t.age_ms(2000), 1000);
    }

    #[test]
    fn illegal_transition_leaves_task_unchanged() {
        let mut t = Task::new(spec(), 0);
        let err = t.transition(TaskState::Active, "nope", 10).unwrap_err();
        assert_eq!(err.from, TaskState::Created);
        assert_eq!(t.state, TaskState::Created);
        assert!(t.history.is_empty());
    }

    #[test]
    fn substate_precedence_is_blocked_over_waiting_over_tool_over_idle() {
        let base = ActivitySignals {
            last_activity_ms: 0,
            ..Default::default()
        };

        let mut s = base;
        s.has_unmet_dependencies = true;
        s.awaiting_user_input = true;
        s.tool_running = true;
        assert_eq!(classify_substate(&s, 10_000, 5_000), TaskState::Blocked);

        let mut s = base;
        s.awaiting_user_input = true;
        s.tool_running = true;
        assert_eq!(classify_substate(&s, 10_000, 5_000), TaskState::WaitingUser);

        let mut s = base;
        s.tool_running = true;
        assert_eq!(classify_substate(&s, 10_000, 5_000), TaskState::ToolRunning);
    }

    #[test]
    fn substate_idle_after_threshold_else_active() {
        let s = ActivitySignals {
            last_activity_ms: 1_000,
            ..Default::default()
        };
        // 6s since activity, threshold 5s -> idle.
        assert_eq!(classify_substate(&s, 7_000, 5_000), TaskState::Idle);
        // 2s since activity -> active.
        assert_eq!(classify_substate(&s, 3_000, 5_000), TaskState::Active);
    }

    /// Property (SUM-42): from every non-terminal state, TERMINATED is reachable, and every
    /// transition target is itself a state whose transitions are all valid targets.
    #[test]
    fn every_state_can_reach_terminated() {
        use std::collections::{HashSet, VecDeque};
        let all = [
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
        for &start in &all {
            let mut seen = HashSet::new();
            let mut q = VecDeque::from([start]);
            let mut reached_terminated = false;
            while let Some(s) = q.pop_front() {
                if !seen.insert(s) {
                    continue;
                }
                if s == TaskState::Terminated {
                    reached_terminated = true;
                }
                for &next in s.allowed_next() {
                    q.push_back(next);
                }
            }
            assert!(reached_terminated, "{start} cannot reach TERMINATED");
        }
    }
}
