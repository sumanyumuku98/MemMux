//! Scheduling score and admission queue (SUM-47 / §7.4).
//!
//! ```text
//! score = w1*priority + w2*interactivity + w3*age + w4*dependency_criticality
//!       - w5*predicted_peak - w6*resume_cost - w7*conflict_risk
//! ```
//!
//! The scheduler admits the highest-scoring tasks that fit the budget; the rest stay `QUEUED`
//! with a visible reason and what they are waiting on. Ordering is deterministic under equal
//! scores (tie-broken by task id).

use memmux_core::{Priority, TaskId};
use serde::{Deserialize, Serialize};

/// Tunable weights for the scoring function. Positive weights reward, the last three penalize.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoreWeights {
    /// Weight on normalized priority.
    pub priority: f64,
    /// Weight on interactivity.
    pub interactivity: f64,
    /// Weight on normalized age.
    pub age: f64,
    /// Weight on dependency criticality.
    pub dependency_criticality: f64,
    /// Penalty weight on predicted peak (as a fraction of budget).
    pub predicted_peak: f64,
    /// Penalty weight on resume cost.
    pub resume_cost: f64,
    /// Penalty weight on conflict risk.
    pub conflict_risk: f64,
    /// Age (ms) that normalizes to 1.0.
    pub age_normalizer_ms: u64,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            priority: 3.0,
            interactivity: 2.0,
            age: 1.0,
            dependency_criticality: 1.5,
            predicted_peak: 2.0,
            resume_cost: 1.0,
            conflict_risk: 1.5,
            age_normalizer_ms: 30 * 60 * 1000, // 30 minutes -> 1.0
        }
    }
}

/// Inputs to the scoring function for a single task. Fractions are expected in `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScoreInputs {
    /// Task priority.
    pub priority: Priority,
    /// How interactive/user-blocking the task is (0..1).
    pub interactivity: f64,
    /// Age since submission (ms).
    pub age_ms: u64,
    /// How critical this task is to unblocking others (0..1).
    pub dependency_criticality: f64,
    /// Predicted peak footprint (bytes).
    pub predicted_peak_bytes: u64,
    /// Cost of resuming this task if it were preempted (0..1).
    pub resume_cost: f64,
    /// Risk this task conflicts with another active writer (0..1).
    pub conflict_risk: f64,
}

fn priority_weight(p: Priority) -> f64 {
    match p {
        Priority::Low => 0.0,
        Priority::Normal => 1.0 / 3.0,
        Priority::High => 2.0 / 3.0,
        Priority::Urgent => 1.0,
    }
}

/// Compute a task's scheduling score against the given budget.
pub fn score(weights: &ScoreWeights, inputs: &ScoreInputs, budget_bytes: u64) -> f64 {
    let age_norm = if weights.age_normalizer_ms == 0 {
        0.0
    } else {
        (inputs.age_ms as f64 / weights.age_normalizer_ms as f64).min(1.0)
    };
    let peak_frac = if budget_bytes == 0 {
        1.0
    } else {
        (inputs.predicted_peak_bytes as f64 / budget_bytes as f64).min(2.0)
    };

    weights.priority * priority_weight(inputs.priority)
        + weights.interactivity * inputs.interactivity.clamp(0.0, 1.0)
        + weights.age * age_norm
        + weights.dependency_criticality * inputs.dependency_criticality.clamp(0.0, 1.0)
        - weights.predicted_peak * peak_frac
        - weights.resume_cost * inputs.resume_cost.clamp(0.0, 1.0)
        - weights.conflict_risk * inputs.conflict_risk.clamp(0.0, 1.0)
}

/// A queued task ready to be considered for admission.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// The task's id.
    pub task_id: TaskId,
    /// Its scheduling score.
    pub score: f64,
    /// Its predicted peak footprint (bytes) — what admission must reserve.
    pub predicted_peak_bytes: u64,
}

/// The outcome of an admission pass.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AdmissionPlan {
    /// Tasks admitted this pass, in admission order.
    pub admitted: Vec<TaskId>,
    /// Tasks left queued, each with a user-visible reason.
    pub deferred: Vec<(TaskId, String)>,
}

/// Plan admission: admit the highest-scoring candidates that fit `available_bytes`.
///
/// Candidates are sorted by score (descending), ties broken by task id for determinism. A task
/// that does not fit is deferred with a reason; lower-priority tasks behind it are still
/// considered (a smaller task may fit the remaining headroom, §7.4).
pub fn plan_admission(mut candidates: Vec<Candidate>, available_bytes: u64) -> AdmissionPlan {
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.task_id.as_str().cmp(b.task_id.as_str()))
    });

    let mut remaining = available_bytes;
    let mut plan = AdmissionPlan::default();
    for c in candidates {
        if c.predicted_peak_bytes <= remaining {
            remaining -= c.predicted_peak_bytes;
            plan.admitted.push(c.task_id);
        } else {
            let need_mib = c.predicted_peak_bytes / (1024 * 1024);
            let have_mib = remaining / (1024 * 1024);
            plan.deferred.push((
                c.task_id,
                format!("insufficient headroom: needs {need_mib} MiB, {have_mib} MiB free"),
            ));
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn inputs(priority: Priority, peak: u64) -> ScoreInputs {
        ScoreInputs {
            priority,
            interactivity: 0.5,
            age_ms: 0,
            dependency_criticality: 0.0,
            predicted_peak_bytes: peak,
            resume_cost: 0.0,
            conflict_risk: 0.0,
        }
    }

    #[test]
    fn higher_priority_scores_higher() {
        let w = ScoreWeights::default();
        let urgent = score(&w, &inputs(Priority::Urgent, GIB), 16 * GIB);
        let low = score(&w, &inputs(Priority::Low, GIB), 16 * GIB);
        assert!(urgent > low);
    }

    #[test]
    fn larger_predicted_peak_penalizes_score() {
        let w = ScoreWeights::default();
        let small = score(&w, &inputs(Priority::Normal, GIB), 16 * GIB);
        let big = score(&w, &inputs(Priority::Normal, 8 * GIB), 16 * GIB);
        assert!(small > big);
    }

    #[test]
    fn admission_admits_highest_score_that_fits() {
        let candidates = vec![
            Candidate {
                task_id: TaskId::new("task_big"),
                score: 10.0,
                predicted_peak_bytes: 12 * GIB,
            },
            Candidate {
                task_id: TaskId::new("task_mid"),
                score: 8.0,
                predicted_peak_bytes: 6 * GIB,
            },
            Candidate {
                task_id: TaskId::new("task_small"),
                score: 5.0,
                predicted_peak_bytes: 3 * GIB,
            },
        ];
        // 10 GiB available: big (12) can't fit, mid (6) fits, small (3) fits in remaining 4.
        let plan = plan_admission(candidates, 10 * GIB);
        assert_eq!(
            plan.admitted,
            vec![TaskId::new("task_mid"), TaskId::new("task_small")]
        );
        assert_eq!(plan.deferred.len(), 1);
        assert_eq!(plan.deferred[0].0, TaskId::new("task_big"));
        assert!(plan.deferred[0].1.contains("insufficient headroom"));
    }

    #[test]
    fn equal_scores_break_ties_deterministically_by_id() {
        let candidates = vec![
            Candidate {
                task_id: TaskId::new("task_b"),
                score: 5.0,
                predicted_peak_bytes: GIB,
            },
            Candidate {
                task_id: TaskId::new("task_a"),
                score: 5.0,
                predicted_peak_bytes: GIB,
            },
        ];
        let plan = plan_admission(candidates, 10 * GIB);
        assert_eq!(
            plan.admitted,
            vec![TaskId::new("task_a"), TaskId::new("task_b")]
        );
    }

    #[test]
    fn nothing_fits_defers_all() {
        let candidates = vec![Candidate {
            task_id: TaskId::new("task_x"),
            score: 9.0,
            predicted_peak_bytes: 5 * GIB,
        }];
        let plan = plan_admission(candidates, GIB);
        assert!(plan.admitted.is_empty());
        assert_eq!(plan.deferred.len(), 1);
    }
}
