//! V2 launch-gate checks (SUM-40 / §18.5).
//!
//! Encodes the six launch gates plus the sampling-overhead NFR as evaluable checks. Gates that
//! cannot be measured until a later phase (crash-safety, cleanup, resume, pressure) return
//! [`GateStatus::Skipped`] with the phase that will supply the evidence — the harness never
//! reports a gate as *passed* on missing data.

use serde::{Deserialize, Serialize};

/// Bytes in one mebibyte.
const MIB: u64 = 1024 * 1024;

/// Outcome of evaluating a single gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GateStatus {
    /// The gate was measured and met its threshold.
    Pass,
    /// The gate was measured and failed its threshold.
    Fail,
    /// The gate cannot be measured yet (evidence arrives in a later phase).
    Skipped,
}

/// A single gate evaluation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GateResult {
    /// Gate name (matches the §18.5 table).
    pub name: String,
    /// Pass / fail / skipped.
    pub status: GateStatus,
    /// Human-readable explanation.
    pub detail: String,
}

/// Measured inputs available to the gate evaluator. `None` means "not measured this run".
#[derive(Clone, Debug, Default)]
pub struct GateInputs {
    /// Daemon (or stub proxy) resident growth over the soak, in bytes.
    pub bounded_growth_bytes: Option<i64>,
    /// Allowed growth before the bounded-memory gate fails (default 100 MiB).
    pub bounded_growth_limit_bytes: u64,
    /// Worst-case fraction of RSS attributed to a task or shared service.
    pub min_attributed_fraction: Option<f64>,
    /// Sampling overhead as a fraction of the sampling interval.
    pub sampling_overhead_fraction: Option<f64>,
    /// Worst-case (minimum) teardown cleanup fraction across MemMux runs (SUM-165 / H2). `None`
    /// when no cleanup data exists this run — the Cleanup gate then stays skipped.
    pub min_cleanup_fraction: Option<f64>,
    /// MemMux steady total footprint in the overcommit run, bytes (SUM-167 / H4a). `None` when no
    /// overcommit run was present — the Pressure-avoidance gate then stays skipped.
    pub overcommit_steady_footprint_bytes: Option<u64>,
    /// Constrained agent budget for that overcommit run, bytes (SUM-167 / H4a). `None` when unset.
    pub overcommit_budget_bytes: Option<u64>,
    /// Worst-case preserved fraction across MemMux pressure reclamations in the overcommit run
    /// (SUM-168 / H4b). `None` when no victim was reclaimed — the No-lost-work gate stays skipped.
    pub no_lost_work_fraction: Option<f64>,
}

impl GateInputs {
    /// Inputs with the default 100 MiB bounded-growth limit.
    pub fn new() -> Self {
        Self {
            bounded_growth_limit_bytes: 100 * MIB,
            ..Default::default()
        }
    }
}

const ATTRIBUTION_MIN: f64 = 0.95;
const OVERHEAD_MAX: f64 = 0.02;
/// Minimum teardown cleanup fraction for the Cleanup gate to pass (SUM-165 / H2).
const CLEANUP_MIN: f64 = 0.995;
/// Steady footprint may exceed the budget by at most this factor for the Pressure-avoidance gate to
/// pass on a dev/macOS host (SUM-167 / H4a): governed footprint stays near/under budget.
const OVERCOMMIT_FOOTPRINT_SLACK: f64 = 1.1;
/// Minimum preserved fraction for the No-lost-work gate to pass (SUM-168 / H4b): every pressure
/// reclamation must have captured a checkpoint carrying a non-empty git patch.
const NO_LOST_WORK_MIN: f64 = 1.0;

/// Evaluate all launch gates against the measured inputs.
pub fn evaluate_gates(inputs: &GateInputs) -> Vec<GateResult> {
    let limit = if inputs.bounded_growth_limit_bytes == 0 {
        100 * MIB
    } else {
        inputs.bounded_growth_limit_bytes
    };

    let mut results = Vec::new();

    // 1. Bounded daemon memory (< 100 MB growth over stabilized baseline).
    results.push(match inputs.bounded_growth_bytes {
        Some(growth) => {
            let over = growth > limit as i64;
            GateResult {
                name: "Bounded memory".into(),
                status: if over {
                    GateStatus::Fail
                } else {
                    GateStatus::Pass
                },
                detail: format!(
                    "soak growth {:.1} MiB vs limit {:.1} MiB",
                    growth as f64 / MIB as f64,
                    limit as f64 / MIB as f64
                ),
            }
        }
        None => skipped("Bounded memory", "no soak growth measured this run"),
    });

    // 2. Attribution (>= 95% of sampled private RSS mapped to a task or shared service).
    results.push(match inputs.min_attributed_fraction {
        Some(frac) => GateResult {
            name: "Attribution".into(),
            status: if frac >= ATTRIBUTION_MIN {
                GateStatus::Pass
            } else {
                GateStatus::Fail
            },
            detail: format!(
                "min attributed {:.2}% vs >= {:.0}%",
                frac * 100.0,
                ATTRIBUTION_MIN * 100.0
            ),
        },
        None => skipped("Attribution", "no attribution measured this run"),
    });

    // 3. Sampling overhead (<= 2% CPU at 20 tasks) — NFR backing FR/§4.2.
    results.push(match inputs.sampling_overhead_fraction {
        Some(ovh) => GateResult {
            name: "Sampling overhead".into(),
            status: if ovh <= OVERHEAD_MAX {
                GateStatus::Pass
            } else {
                GateStatus::Fail
            },
            detail: format!(
                "overhead {:.3}% vs <= {:.0}%",
                ovh * 100.0,
                OVERHEAD_MAX * 100.0
            ),
        },
        None => skipped("Sampling overhead", "no overhead measured this run"),
    });

    // 4. Cleanup (SUM-165 / H2): the whole owned agent subtree must be reclaimed within the grace
    // window of MemMux's own normal teardown. MEASURED: pass iff the worst-case cleanup fraction
    // across MemMux runs >= 99.5%. Skipped only when no cleanup data exists this run.
    results.push(match inputs.min_cleanup_fraction {
        Some(frac) => GateResult {
            name: "Cleanup".into(),
            status: if frac >= CLEANUP_MIN {
                GateStatus::Pass
            } else {
                GateStatus::Fail
            },
            detail: format!(
                "min reclaimed {:.2}% vs >= {:.1}%",
                frac * 100.0,
                CLEANUP_MIN * 100.0
            ),
        },
        None => skipped("Cleanup", "no teardown cleanup measured this run"),
    });

    // 5. No lost work (SUM-168 / H4b): every MemMux pressure reclamation must have captured a
    // checkpoint carrying a non-empty git patch. MEASURED: pass iff the worst-case preserved
    // fraction >= 1.0. Skipped when no victim was reclaimed this run (honest — nothing to preserve).
    results.push(match inputs.no_lost_work_fraction {
        Some(frac) => GateResult {
            name: "No lost work".into(),
            status: if frac >= NO_LOST_WORK_MIN {
                GateStatus::Pass
            } else {
                GateStatus::Fail
            },
            detail: format!(
                "preserved {:.1}% of reclaimed victims vs >= {:.0}%",
                frac * 100.0,
                NO_LOST_WORK_MIN * 100.0
            ),
        },
        None => skipped(
            "No lost work",
            "no pressure reclamation triggered this run (no victim to preserve)",
        ),
    });

    // 6. Pressure avoidance (SUM-167 / H4a): under a constrained budget the governed steady
    // footprint must stay near/under budget. MEASURED on this (macOS/dev) host as footprint:
    // pass iff steady footprint <= budget * 1.1. The swap-growth-SLO variant is the Linux
    // corollary, measured on the Linux reference host via the per-process swap columns. Skipped
    // when no overcommit run (or no budget) was present.
    results.push(
        match (
            inputs.overcommit_steady_footprint_bytes,
            inputs.overcommit_budget_bytes,
        ) {
            (Some(steady), Some(budget)) if budget > 0 => {
                let limit = budget as f64 * OVERCOMMIT_FOOTPRINT_SLACK;
                GateResult {
                    name: "Pressure avoidance".into(),
                    status: if steady as f64 <= limit {
                        GateStatus::Pass
                    } else {
                        GateStatus::Fail
                    },
                    detail: format!(
                        "steady footprint {:.1} MiB vs <= {:.1} MiB (budget {:.1} MiB × {:.2})",
                        steady as f64 / MIB as f64,
                        limit / MIB as f64,
                        budget as f64 / MIB as f64,
                        OVERCOMMIT_FOOTPRINT_SLACK,
                    ),
                }
            }
            _ => skipped(
                "Pressure avoidance",
                "no constrained-budget overcommit run present this run",
            ),
        },
    );

    // 7. Resume — evidence arrives in a later phase.
    results.push(skipped(
        "Resume",
        "checkpoint / native resume arrives in Phase 2",
    ));

    results
}

fn skipped(name: &str, why: &str) -> GateResult {
    GateResult {
        name: name.into(),
        status: GateStatus::Skipped,
        detail: why.into(),
    }
}

/// Whether every *measured* gate passed (skipped gates do not fail the run).
pub fn all_measured_gates_pass(results: &[GateResult]) -> bool {
    results.iter().all(|r| r.status != GateStatus::Fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passing_inputs_pass_measured_gates() {
        let mut inputs = GateInputs::new();
        inputs.bounded_growth_bytes = Some(10 * MIB as i64);
        inputs.min_attributed_fraction = Some(0.97);
        inputs.sampling_overhead_fraction = Some(0.005);
        let results = evaluate_gates(&inputs);
        assert!(all_measured_gates_pass(&results));
        let attribution = results.iter().find(|r| r.name == "Attribution").unwrap();
        assert_eq!(attribution.status, GateStatus::Pass);
    }

    #[test]
    fn low_attribution_fails() {
        let mut inputs = GateInputs::new();
        inputs.min_attributed_fraction = Some(0.80);
        let results = evaluate_gates(&inputs);
        assert!(!all_measured_gates_pass(&results));
    }

    #[test]
    fn excessive_growth_fails_bounded_memory() {
        let mut inputs = GateInputs::new();
        inputs.bounded_growth_bytes = Some(250 * MIB as i64);
        let results = evaluate_gates(&inputs);
        let g = results.iter().find(|r| r.name == "Bounded memory").unwrap();
        assert_eq!(g.status, GateStatus::Fail);
    }

    #[test]
    fn high_overhead_fails() {
        let mut inputs = GateInputs::new();
        inputs.sampling_overhead_fraction = Some(0.05);
        let results = evaluate_gates(&inputs);
        let g = results
            .iter()
            .find(|r| r.name == "Sampling overhead")
            .unwrap();
        assert_eq!(g.status, GateStatus::Fail);
    }

    #[test]
    fn cleanup_gate_passes_at_or_above_threshold() {
        let mut inputs = GateInputs::new();
        inputs.min_cleanup_fraction = Some(1.0);
        let results = evaluate_gates(&inputs);
        let g = results.iter().find(|r| r.name == "Cleanup").unwrap();
        assert_eq!(g.status, GateStatus::Pass);
        assert!(g.detail.contains("100.00%"));
    }

    #[test]
    fn cleanup_gate_fails_below_threshold() {
        let mut inputs = GateInputs::new();
        // A leak: some of the owned subtree survived teardown.
        inputs.min_cleanup_fraction = Some(0.75);
        let results = evaluate_gates(&inputs);
        let g = results.iter().find(|r| r.name == "Cleanup").unwrap();
        assert_eq!(g.status, GateStatus::Fail);
        assert!(!all_measured_gates_pass(&results));
    }

    #[test]
    fn cleanup_gate_skipped_without_data() {
        let inputs = GateInputs::new();
        let results = evaluate_gates(&inputs);
        let g = results.iter().find(|r| r.name == "Cleanup").unwrap();
        assert_eq!(g.status, GateStatus::Skipped);
    }

    #[test]
    fn unmeasured_gates_are_skipped_not_failed() {
        let inputs = GateInputs::new();
        let results = evaluate_gates(&inputs);
        assert!(all_measured_gates_pass(&results)); // nothing measured -> nothing failed
        assert!(
            results
                .iter()
                .filter(|r| r.status == GateStatus::Skipped)
                .count()
                >= 4
        );
    }
}
