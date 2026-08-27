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
    /// A long-lived resident agent (with a child subtree) that stays alive ~60s.
    ///
    /// Explicitly-selectable and **not** part of [`Scenario::ALL`]: it is the concurrency
    /// prerequisite for the cleanup/leak-on-teardown measurement (SUM-165 / H2), where N agents
    /// must stay concurrently resident long enough to outlive sampling **and** the 10s teardown
    /// grace. The other scenarios finish in ~1s, so they cannot host that measurement.
    Hold,
    /// An agent that injects an **escaped** process (double-fork → reparent to init) and stays
    /// alive long enough for the daemon to sample it under the task and then flag the reparent
    /// (SUM-166 / H3).
    ///
    /// Explicitly-selectable and **not** part of [`Scenario::ALL`]: it is the driver for the
    /// escaped-process visibility measurement, where MemMux's daemon surfaces the reparented pid
    /// as a `process_escaped` event and the baselines cannot detect it at all.
    Escape,
    /// N long-lived resident agents (each with a child subtree) held concurrently live under a
    /// **constrained agent budget** for the bounded-footprint-under-overcommit measurement
    /// (SUM-167 / H4a) and the no-lost-work measurement (SUM-168 / H4b).
    ///
    /// Explicitly-selectable and **not** part of [`Scenario::ALL`]: it is the driver for H4, where
    /// the daemon is started with an agent budget below the aggregate predicted peak of N agents so
    /// the admission planner must defer and/or the pressure ladder must reclaim. Like [`Hold`], the
    /// agents stay resident (Allocate + long-hold SpawnChild + Sleep) so all N are concurrently live
    /// long enough to be sampled; the baselines run all N ungoverned.
    ///
    /// [`Hold`]: Scenario::Hold
    Overcommit,
}

impl Scenario {
    /// The canonical scenarios, in declaration order.
    ///
    /// This is deliberately the four Phase-0 targets so that `--scenario all` behaviour is
    /// unchanged; [`Scenario::Hold`] is an explicitly-selectable extra, not part of `ALL`.
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
            Scenario::Hold => "hold",
            Scenario::Escape => "escape",
            Scenario::Overcommit => "overcommit",
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
            Scenario::Hold => {
                "Long-lived resident agent + child subtree (~60s) for the teardown-cleanup measurement."
            }
            Scenario::Escape => {
                "Agent that injects an escaped process (double-fork → reparent to init) for the escape-detection measurement."
            }
            Scenario::Overcommit => {
                "N resident agents held concurrently under a constrained agent budget (bounded-footprint + no-lost-work measurement)."
            }
        }
    }

    /// Whether this scenario is expected to keep resident memory bounded.
    ///
    /// Burst/Soak/Idle/Hold/Escape should stay flat; Leak should grow (that is the point). Escape
    /// is about *process visibility*, not memory growth — the agent's own footprint stays flat.
    pub fn expects_bounded_memory(self) -> bool {
        !matches!(self, Scenario::Leak)
    }

    /// Whether this scenario is the overcommit driver (SUM-167 / H4): the harness constrains the
    /// MemMux agent budget below the aggregate predicted peak of N agents for this scenario only.
    pub fn is_overcommit(self) -> bool {
        matches!(self, Scenario::Overcommit)
    }

    /// How many escaped processes each agent injects in this scenario (SUM-166 / H3).
    ///
    /// Counts the [`Step::Orphan`] steps in the recording; only [`Scenario::Escape`] injects any,
    /// so every other scenario returns `0` (and the escape measurement is n/a for them).
    pub fn escapes_per_agent(self, provider: Provider) -> usize {
        self.recording(provider, 1)
            .steps
            .iter()
            .filter(|s| matches!(s, Step::Orphan { .. }))
            .count()
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
            // Fixed hold, independent of `intensity`: a resident agent WITH a child subtree that
            // stays alive ~60s, long enough to outlive sampling plus the 10s cleanup grace so
            // every agent is still concurrently resident when teardown is measured (SUM-165 / H2).
            //
            // The 40 MiB is held resident for the whole sleep and freed only at the very end, so
            // the modeled trajectory rises to a plateau and returns to baseline — bounded, and
            // (unlike Leak) NOT monotonic growth. The child holds its own 30 MiB live in its own
            // process (observed via the process tree, not this model).
            Scenario::Hold => SessionRecording::new("hold", provider, base)
                .with(Step::Allocate { mib: 40 })
                .with(Step::SpawnChild {
                    mib: 30,
                    hold_ms: 60_000,
                })
                .with(Step::Sleep { ms: 60_000 })
                .with(Step::Free { mib: 40 }),
            // Fixed timing, independent of `intensity` (the escape mechanism is timing-critical):
            //  * Allocate a small transient so the agent has a real footprint;
            //  * Orphan{settle_ms: 2500}: the intermediate lives ~2.5s so ≥2 daemon sample ticks
            //    (SAMPLE_INTERVAL_MS≈1000ms) record the grandchild as a descendant of this task's
            //    root BEFORE it reparents, then the intermediate exits → the grandchild reparents
            //    to init (escaped, still alive because hold_ms is long);
            //  * Sleep so the AGENT itself stays alive for the whole run (≥3–4 more sample ticks),
            //    giving the daemon time to reconcile the reparent and emit `process_escaped`.
            Scenario::Escape => SessionRecording::new("escape", provider, base)
                .with(Step::Allocate { mib: 20 })
                .with(Step::Orphan {
                    settle_ms: 2_500,
                    hold_ms: 30_000,
                })
                .with(Step::Sleep { ms: 30_000 })
                // Freed only at the very end so the modeled trajectory is a plateau that returns to
                // baseline — bounded, and (like Hold) NOT the monotonic growth that flags a leak.
                .with(Step::Free { mib: 20 }),
            // Fixed timing, independent of `intensity` (like Hold): a resident agent WITH a child
            // subtree, held ~60s so all N stay concurrently live under the constrained budget long
            // enough to be sampled and, if the budget is tight enough, reclaimed (SUM-167/168 / H4).
            // The held MiB is held for the whole sleep then freed, so a single agent's own footprint
            // is a bounded plateau (peak base+hold) that returns to baseline — NOT a leak — while N
            // such agents aggregate well past the budget, forcing MemMux to govern.
            //
            // The per-agent resident footprint is configurable via `MEMMUX_BENCH_HOLD_MIB` (default
            // 120). Setting it near the Standard-class reservation prior (~1.4 GiB) makes the
            // constrained budget genuinely BINDING against a realistic footprint — so admission is
            // well-calibrated (reservation ≈ actual) rather than reserving ~7× the tiny default.
            Scenario::Overcommit => {
                let hold = overcommit_hold_mib();
                SessionRecording::new("overcommit", provider, base)
                    .with(Step::Allocate { mib: hold })
                    .with(Step::SpawnChild {
                        mib: 30,
                        hold_ms: 60_000,
                    })
                    .with(Step::Sleep { ms: 60_000 })
                    .with(Step::Free { mib: hold })
            }
        }
    }
}

/// Per-agent resident footprint (MiB) held by the `overcommit` scenario, from
/// `MEMMUX_BENCH_HOLD_MIB` (default 120). A positive parse wins; anything else falls back to the
/// default. Used to make the H4 budget binding against a realistic footprint (SUM-167).
fn overcommit_hold_mib() -> u64 {
    std::env::var("MEMMUX_BENCH_HOLD_MIB")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&m| m > 0)
        .unwrap_or(120)
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
        // Hold, Escape, and Overcommit are deliberately NOT in ALL, but their slugs must be distinct.
        assert_eq!(Scenario::Hold.slug(), "hold");
        assert_eq!(Scenario::Escape.slug(), "escape");
        assert_eq!(Scenario::Overcommit.slug(), "overcommit");
        assert!(!slugs.contains(&Scenario::Hold.slug()));
        assert!(!slugs.contains(&Scenario::Escape.slug()));
        assert!(!slugs.contains(&Scenario::Overcommit.slug()));
    }

    #[test]
    fn hold_is_bounded_and_not_a_leak() {
        // Only ever `.simulate()` Hold in a unit test — `.execute()` sleeps for a real 60s.
        let traj = Scenario::Hold.recording(Provider::Generic, 1).simulate();
        assert!(Scenario::Hold.expects_bounded_memory());
        // The 40 MiB is held during the sleep then freed: the trajectory rises to a plateau and
        // returns to baseline — bounded, and NOT the monotonic growth that flags a leak.
        assert!(!traj.is_monotonic_growth(), "hold looks like a leak");
        assert_eq!(traj.peak_resident_mib(), 64 + 40);
        assert_eq!(traj.final_resident_mib(), 64);
    }

    #[test]
    fn escape_is_bounded_and_not_a_leak() {
        // Only ever `.simulate()` Escape in a unit test — `.execute()` spawns real processes and
        // sleeps for tens of seconds. The agent's own footprint is a flat plateau (it allocates a
        // small transient and never frees it during the run), NOT monotonic growth.
        let traj = Scenario::Escape.recording(Provider::Generic, 1).simulate();
        assert!(Scenario::Escape.expects_bounded_memory());
        assert!(
            !traj.is_monotonic_growth(),
            "escape must not look like a leak"
        );
        // A 20 MiB transient is held during the run then freed at the very end: the trajectory
        // rises to a plateau (peak) and returns to baseline — bounded, not a leak.
        assert_eq!(traj.peak_resident_mib(), 64 + 20);
        assert_eq!(traj.final_resident_mib(), 64);
    }

    #[test]
    fn escape_ignores_intensity() {
        // The escape timing is fixed; the step list must not scale with intensity.
        let a = Scenario::Escape.recording(Provider::Generic, 1);
        let b = Scenario::Escape.recording(Provider::Generic, 8);
        assert_eq!(a.steps, b.steps);
    }

    #[test]
    fn only_escape_injects_escapes() {
        assert_eq!(Scenario::Escape.escapes_per_agent(Provider::Generic), 1);
        for s in Scenario::ALL
            .iter()
            .copied()
            .chain([Scenario::Hold, Scenario::Overcommit])
        {
            assert_eq!(
                s.escapes_per_agent(Provider::Generic),
                0,
                "{} must not inject escapes",
                s.slug()
            );
        }
    }

    #[test]
    fn overcommit_is_bounded_and_not_a_leak() {
        // Only ever `.simulate()` Overcommit in a unit test — `.execute()` spawns real processes
        // and sleeps ~60s. The agent's own footprint is a bounded plateau (it allocates 120 MiB,
        // holds it during the sleep, then frees it), NOT monotonic growth.
        let traj = Scenario::Overcommit
            .recording(Provider::Generic, 1)
            .simulate();
        assert!(Scenario::Overcommit.expects_bounded_memory());
        assert!(Scenario::Overcommit.is_overcommit());
        assert!(
            !traj.is_monotonic_growth(),
            "overcommit must not look like a leak"
        );
        assert_eq!(traj.peak_resident_mib(), 64 + 120);
        assert_eq!(traj.final_resident_mib(), 64);
    }

    #[test]
    fn overcommit_ignores_intensity() {
        // The hold timing is fixed; the step list must not scale with intensity.
        let a = Scenario::Overcommit.recording(Provider::Generic, 1);
        let b = Scenario::Overcommit.recording(Provider::Generic, 8);
        assert_eq!(a.steps, b.steps);
    }

    #[test]
    fn hold_ignores_intensity() {
        // The hold is fixed; the step list must not scale with intensity.
        let a = Scenario::Hold.recording(Provider::Generic, 1);
        let b = Scenario::Hold.recording(Provider::Generic, 8);
        assert_eq!(a.steps.len(), b.steps.len());
        assert_eq!(a.steps, b.steps);
    }
}
