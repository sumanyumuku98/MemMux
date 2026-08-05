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

    // 4-7. Gates whose evidence arrives in later phases.
    results.push(skipped(
        "No lost work",
        "fault-injection lifecycle trials arrive in Phase 2",
    ));
    results.push(skipped(
        "Cleanup",
        "recursive termination + reconciliation arrive in Phase 1",
    ));
    results.push(skipped(
        "Pressure avoidance",
        "budget + pressure ladder arrive in Phase 1",
    ));
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
