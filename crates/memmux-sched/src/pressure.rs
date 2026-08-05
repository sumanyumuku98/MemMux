//! The pressure ladder (SUM-48 / §7.5).
//!
//! Graduated reclamation that reacts to leading indicators (headroom, swap growth) *before*
//! sustained thrashing. Each stage carries deterministic actions; the terminal action always
//! preserves Git state before terminating anything (§2.4 "preserve developer work").

use serde::{Deserialize, Serialize};

/// A pressure stage, from calm to emergency (§7.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PressureStage {
    /// < 70% of the managed envelope; stable swap.
    Normal,
    /// 70–82% or a rising page-fault trend.
    Elevated,
    /// 82–90% or sustained swap growth.
    High,
    /// > 90%, major faults, or responsiveness degradation.
    Critical,
    /// Hard limit exceeded or host pressure critical.
    Emergency,
}

impl PressureStage {
    /// Classify the pressure stage from budget utilization plus leading indicators.
    ///
    /// `swap_growing` escalates by one stage (a leading indicator of thrashing);
    /// `hard_limit_exceeded` forces [`PressureStage::Emergency`].
    pub fn classify(utilization: f64, swap_growing: bool, hard_limit_exceeded: bool) -> Self {
        if hard_limit_exceeded {
            return PressureStage::Emergency;
        }
        let base = if utilization < 0.70 {
            PressureStage::Normal
        } else if utilization < 0.82 {
            PressureStage::Elevated
        } else if utilization < 0.90 {
            PressureStage::High
        } else {
            PressureStage::Critical
        };
        if swap_growing {
            base.escalated()
        } else {
            base
        }
    }

    /// The next stage up (saturating at `Emergency`).
    pub fn escalated(self) -> Self {
        match self {
            PressureStage::Normal => PressureStage::Elevated,
            PressureStage::Elevated => PressureStage::High,
            PressureStage::High => PressureStage::Critical,
            PressureStage::Critical => PressureStage::Emergency,
            PressureStage::Emergency => PressureStage::Emergency,
        }
    }

    /// The deterministic actions to apply at this stage, in order.
    pub fn actions(self) -> &'static [PressureAction] {
        use PressureAction::*;
        match self {
            PressureStage::Normal => &[RoutineCompaction],
            PressureStage::Elevated => &[TrimRenderCaches, RotateBuffers, StopIdleSharedWorkers],
            PressureStage::High => &[
                ReclaimIdleChildren,
                ReduceWorkerConcurrency,
                BlockLowPriorityStarts,
            ],
            PressureStage::Critical => &[HibernateLowPriority, RecycleBloatedProviders],
            PressureStage::Emergency => &[
                PreserveGitState,
                TerminateLowestPriorityTree,
                NotifyOperator,
            ],
        }
    }
}

/// A concrete reclamation action (§7.5). Every action is explainable to the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PressureAction {
    /// Routine cache/buffer compaction.
    RoutineCompaction,
    /// Trim UI render caches.
    TrimRenderCaches,
    /// Rotate oldest terminal buffers to disk.
    RotateBuffers,
    /// Stop idle shared-service workers.
    StopIdleSharedWorkers,
    /// Reclaim idle descendant processes.
    ReclaimIdleChildren,
    /// Reduce test/build/browser worker concurrency.
    ReduceWorkerConcurrency,
    /// Block admission of low-priority tasks.
    BlockLowPriorityStarts,
    /// Hibernate low-priority tasks.
    HibernateLowPriority,
    /// Recycle bloated provider processes at safe points.
    RecycleBloatedProviders,
    /// Persist Git dirty manifest + patch hash before any destructive action.
    PreserveGitState,
    /// Terminate the lowest-priority owned process tree (only after Git state is preserved).
    TerminateLowestPriorityTree,
    /// Notify the operator immediately.
    NotifyOperator,
}

impl PressureAction {
    /// A user-visible description of the action (feeds the audit trail and TUI).
    pub fn describe(self) -> &'static str {
        match self {
            PressureAction::RoutineCompaction => "routine cache and buffer compaction",
            PressureAction::TrimRenderCaches => "trimmed UI render caches",
            PressureAction::RotateBuffers => "rotated oldest terminal buffers to disk",
            PressureAction::StopIdleSharedWorkers => "stopped idle shared-service workers",
            PressureAction::ReclaimIdleChildren => "reclaimed idle descendant processes",
            PressureAction::ReduceWorkerConcurrency => {
                "reduced test/build/browser worker concurrency"
            }
            PressureAction::BlockLowPriorityStarts => "blocked new low-priority task starts",
            PressureAction::HibernateLowPriority => "hibernated low-priority tasks",
            PressureAction::RecycleBloatedProviders => {
                "recycled bloated provider processes at safe points"
            }
            PressureAction::PreserveGitState => "persisted Git dirty manifest and patch hash",
            PressureAction::TerminateLowestPriorityTree => {
                "terminated the lowest-priority process tree"
            }
            PressureAction::NotifyOperator => "notified the operator",
        }
    }

    /// Whether this action can terminate a process. Such actions must never run before Git state
    /// is preserved (enforced by ordering within [`PressureStage::Emergency`]).
    pub fn is_destructive(self) -> bool {
        matches!(self, PressureAction::TerminateLowestPriorityTree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_follow_spec_bands() {
        assert_eq!(
            PressureStage::classify(0.50, false, false),
            PressureStage::Normal
        );
        assert_eq!(
            PressureStage::classify(0.75, false, false),
            PressureStage::Elevated
        );
        assert_eq!(
            PressureStage::classify(0.85, false, false),
            PressureStage::High
        );
        assert_eq!(
            PressureStage::classify(0.95, false, false),
            PressureStage::Critical
        );
    }

    #[test]
    fn hard_limit_forces_emergency() {
        assert_eq!(
            PressureStage::classify(0.10, false, true),
            PressureStage::Emergency
        );
    }

    #[test]
    fn swap_growth_escalates_one_stage() {
        assert_eq!(
            PressureStage::classify(0.50, true, false),
            PressureStage::Elevated
        );
        assert_eq!(
            PressureStage::classify(0.95, true, false),
            PressureStage::Emergency
        );
    }

    #[test]
    fn stages_are_ordered() {
        assert!(PressureStage::Normal < PressureStage::Elevated);
        assert!(PressureStage::High < PressureStage::Critical);
        assert!(PressureStage::Critical < PressureStage::Emergency);
    }

    #[test]
    fn emergency_preserves_git_before_terminating() {
        let actions = PressureStage::Emergency.actions();
        let preserve = actions
            .iter()
            .position(|a| *a == PressureAction::PreserveGitState)
            .unwrap();
        let terminate = actions
            .iter()
            .position(|a| *a == PressureAction::TerminateLowestPriorityTree)
            .unwrap();
        assert!(
            preserve < terminate,
            "Git state must be preserved before any termination"
        );
    }

    #[test]
    fn only_termination_is_destructive_and_every_action_describes() {
        for stage in [
            PressureStage::Normal,
            PressureStage::Elevated,
            PressureStage::High,
            PressureStage::Critical,
            PressureStage::Emergency,
        ] {
            for action in stage.actions() {
                assert!(!action.describe().is_empty());
            }
        }
        assert!(PressureAction::TerminateLowestPriorityTree.is_destructive());
        assert!(!PressureAction::HibernateLowPriority.is_destructive());
    }
}
