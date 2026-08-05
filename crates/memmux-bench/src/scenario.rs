//! Benchmark scenarios (SUM-36 / §18.4).
//!
//! Each scenario deterministically produces one or more [`SessionRecording`]s that exercise a
//! specific memory-pressure behaviour the benchmark must characterise.

use crate::stub::{SessionRecording, Step};
use memmux_core::Provider;
use serde::{Deserialize, Serialize};

/// The canonical benchmark scenarios (§18.4 "Required scenarios", trimmed to the four
/// Phase 0 targets).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scenario {
    /// Short high-intensity output burst (terminal flood).
    Burst,
    /// Long-running session with steady output and stable resident memory.
    Soak,
    /// Mostly idle session (detach/reattach persistence).
    Idle,
    /// A child that allocates memory monotonically and never frees it.
    Leak,
}

impl Scenario {
    /// All scenarios, in declaration order.
    pub const ALL: [Scenario; 4] = [
        Scenario::Burst,
        Scenario::Soak,
        Scenario::Idle,
        Scenario::Leak,
    ];

    /// Stable slug for filenames and report tables.
    pub fn slug(self) -> &'static str {
        match self {
            Scenario::Burst => "burst",
            Scenario::Soak => "soak",
            Scenario::Idle => "idle",
            Scenario::Leak => "leak",
        }
    }

    /// One-line description.
    pub fn description(self) -> &'static str {
        match self {
            Scenario::Burst => "Short, intense terminal-output burst with stable memory.",
            Scenario::Soak => {
                "Long session emitting steady output; resident memory must stay flat."
            }
            Scenario::Idle => "Mostly idle session; candidate for hibernation.",
            Scenario::Leak => "Monotonic, un-freed memory growth (leak injection).",
        }
    }

    /// Whether this scenario is expected to keep resident memory bounded.
    ///
    /// Burst/Soak/Idle should stay flat; Leak should grow (that is the point).
    pub fn expects_bounded_memory(self) -> bool {
        !matches!(self, Scenario::Leak)
    }

    /// Build the stub recording for this scenario at the given intensity.
    ///
    /// `intensity` scales the workload (e.g. number of output ticks); 1 is a fast smoke run.
    pub fn recording(self, provider: Provider, intensity: u64) -> SessionRecording {
        let intensity = intensity.max(1);
        let base = provider_base_mib(provider);
        match self {
            Scenario::Burst => {
                // Spawn a "test worker" child so there is a real multi-process tree to
                // attribute; it outlives the output burst so samplers observe both processes.
                let mut rec = SessionRecording::new("burst", provider, base)
                    .with(Step::Allocate { mib: 40 })
                    .with(Step::SpawnChild {
                        mib: 30,
                        hold_ms: 800 * intensity,
                    });
                for _ in 0..(20 * intensity) {
                    rec = rec.with(Step::Emit {
                        lines: 500,
                        line_bytes: 120,
                    });
                }
                rec.with(Step::Free { mib: 40 })
            }
            Scenario::Soak => {
                let mut rec = SessionRecording::new("soak", provider, base);
                for _ in 0..(100 * intensity) {
                    rec = rec
                        .with(Step::Emit {
                            lines: 200,
                            line_bytes: 100,
                        })
                        .with(Step::Sleep { ms: 0 });
                }
                rec
            }
            Scenario::Idle => {
                let mut rec = SessionRecording::new("idle", provider, base);
                for _ in 0..(10 * intensity) {
                    rec = rec.with(Step::Sleep { ms: 0 });
                }
                rec.with(Step::Emit {
                    lines: 1,
                    line_bytes: 40,
                })
            }
            Scenario::Leak => SessionRecording::new("leak", provider, base)
                .with(Step::SpawnChild {
                    mib: 24,
                    hold_ms: 600 * intensity,
                })
                .with(Step::Leak {
                    mib_per_tick: 8,
                    ticks: 10 * intensity,
                }),
        }
    }
}

/// Bootstrap baseline resident memory per provider, in mebibytes (rough §7.3 priors).
fn provider_base_mib(provider: Provider) -> u64 {
    match provider {
        Provider::ClaudeCode => 320,
        Provider::Codex => 300,
        Provider::GeminiCli => 280,
        Provider::OpenCode => 260,
        Provider::Generic => 64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_and_soak_keep_memory_bounded() {
        for scenario in [Scenario::Burst, Scenario::Soak, Scenario::Idle] {
            let traj = scenario.recording(Provider::ClaudeCode, 1).simulate();
            // Peak must not exceed the baseline by more than the scenario's transient alloc.
            assert!(
                traj.peak_resident_mib() <= 320 + 40,
                "{} peak {} too high",
                scenario.slug(),
                traj.peak_resident_mib()
            );
            assert!(
                !traj.is_monotonic_growth(),
                "{} looks like a leak",
                scenario.slug()
            );
            assert!(scenario.expects_bounded_memory());
        }
    }

    #[test]
    fn burst_emits_a_lot_of_output_without_growing_memory() {
        let traj = Scenario::Burst.recording(Provider::Codex, 2).simulate();
        assert!(traj.total_emitted_bytes() > 1_000_000);
        // Ends back at baseline after freeing the transient allocation.
        assert_eq!(traj.final_resident_mib(), 300);
    }

    #[test]
    fn leak_scenario_grows_monotonically() {
        let traj = Scenario::Leak.recording(Provider::GeminiCli, 1).simulate();
        assert!(traj.is_monotonic_growth());
        assert!(!Scenario::Leak.expects_bounded_memory());
        assert_eq!(traj.final_resident_mib(), 280 + 80);
    }

    #[test]
    fn intensity_scales_workload() {
        let small = Scenario::Soak.recording(Provider::Generic, 1);
        let big = Scenario::Soak.recording(Provider::Generic, 3);
        assert!(big.steps.len() > small.steps.len());
    }

    #[test]
    fn all_scenarios_have_unique_slugs() {
        let mut slugs: Vec<&str> = Scenario::ALL.iter().map(|s| s.slug()).collect();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), Scenario::ALL.len());
    }
}
