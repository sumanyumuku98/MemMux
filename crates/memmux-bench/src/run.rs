//! Live benchmark orchestration: launch a stub under a launcher and sample it over time.
//!
//! This is the glue that turns the pure pieces (scenarios, launchers, sampler, gates, report)
//! into an end-to-end run and is the integration point that actually exercises
//! `memmux-metrics` against real processes.

use crate::gates::{evaluate_gates, GateInputs, GateResult};
use crate::launcher::{LaunchSpec, Launcher, LauncherKind};
use crate::report::{render_markdown, RunSummary};
use crate::sampler::{sample_once, TimeSeries};
use crate::scenario::Scenario;
use memmux_core::ids::TaskId;
use memmux_core::Provider;
use memmux_metrics::{default_sampler, RootSpec};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const MIB: u64 = 1024 * 1024;

/// Realistic steady-state sampling cadence the overhead gate is evaluated against (ms).
const REFERENCE_CADENCE_MS: u64 = 1000;

/// Configuration for a single benchmark run.
#[derive(Clone, Debug)]
pub struct RunConfig {
    /// Provider profile to stand in for.
    pub provider: Provider,
    /// Workload intensity (1 = fast smoke run).
    pub intensity: u64,
    /// Milliseconds between samples.
    pub interval_ms: u64,
    /// Maximum number of samples per run.
    pub max_samples: usize,
    /// Path to the `memmux-bench` binary (used to execute the stub).
    pub bench_exe: PathBuf,
    /// Directory for recordings and JSONL output.
    pub workdir: PathBuf,
}

/// The result of running one launcher against one scenario.
#[derive(Clone, Debug)]
pub struct LauncherRun {
    /// Launcher name.
    pub launcher: String,
    /// Scenario.
    pub scenario: Scenario,
    /// Sampled time series.
    pub series: TimeSeries,
}

/// Aggregate result of a benchmark across launchers and scenarios.
#[derive(Clone, Debug)]
pub struct BenchOutcome {
    /// Individual runs.
    pub runs: Vec<LauncherRun>,
    /// Per-run summaries.
    pub summaries: Vec<RunSummary>,
    /// Evaluated launch gates.
    pub gates: Vec<GateResult>,
}

impl BenchOutcome {
    /// Render the outcome as a Markdown report.
    pub fn to_markdown(&self, title: &str) -> String {
        render_markdown(title, &self.summaries, &self.gates)
    }
}

/// Run one launcher against one scenario, sampling the launched stub over its lifetime.
pub fn run_launcher_scenario(
    launcher: &dyn Launcher,
    scenario: Scenario,
    cfg: &RunConfig,
) -> anyhow::Result<TimeSeries> {
    std::fs::create_dir_all(&cfg.workdir)?;

    // Materialize the recording as JSON so the stub subprocess can load it.
    let recording = scenario.recording(cfg.provider, cfg.intensity);
    let recording_path = cfg.workdir.join(format!(
        "{}-{}.recording.json",
        launcher.name(),
        scenario.slug()
    ));
    std::fs::write(&recording_path, serde_json::to_vec_pretty(&recording)?)?;

    let spec = LaunchSpec {
        recording_path,
        bench_exe: cfg.bench_exe.clone(),
    };
    let mut launched = launcher.launch(&spec)?;
    let pid = launched.pid;

    let roots = vec![RootSpec::task(pid, "task_bench")];
    let expected: HashMap<_, TaskId> = HashMap::new();
    let sampler = default_sampler();

    let started = Instant::now();
    let mut records = Vec::new();
    loop {
        let elapsed_ms = started.elapsed().as_millis() as u64;
        if let Ok(record) = sample_once(
            sampler.as_ref(),
            &roots,
            &expected,
            launcher.name(),
            scenario.slug(),
            elapsed_ms,
            Some(pid),
        ) {
            records.push(record);
        }

        if records.len() >= cfg.max_samples {
            break;
        }
        // Stop once the child has exited.
        if matches!(launched.child.try_wait(), Ok(Some(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(cfg.interval_ms));
    }

    // Reap the child so we never leak the process we launched (§2.4 "own what you launch").
    let _ = launched.child.wait();
    Ok(TimeSeries::new(records))
}

/// Run every *available* launcher against the given scenarios and evaluate the launch gates.
pub fn run_benchmark(
    launchers: &[Box<dyn Launcher>],
    scenarios: &[Scenario],
    cfg: &RunConfig,
) -> anyhow::Result<BenchOutcome> {
    let mut runs = Vec::new();
    let mut summaries = Vec::new();

    for launcher in launchers {
        if !launcher.is_available() {
            continue;
        }
        for &scenario in scenarios {
            let series = run_launcher_scenario(launcher.as_ref(), scenario, cfg)?;
            // Persist the raw series next to the report for auditability.
            let jsonl = cfg
                .workdir
                .join(format!("{}-{}.jsonl", launcher.name(), scenario.slug()));
            series.write_jsonl(&jsonl)?;
            summaries.push(RunSummary::from_series(
                launcher.name(),
                scenario.slug(),
                &series,
                cfg.interval_ms,
            ));
            runs.push(LauncherRun {
                launcher: launcher.name().to_string(),
                scenario,
                series,
            });
        }
    }

    let gates = evaluate_gates(&derive_gate_inputs(launchers, &runs));
    Ok(BenchOutcome {
        runs,
        summaries,
        gates,
    })
}

/// Derive launch-gate inputs from the collected runs.
fn derive_gate_inputs(launchers: &[Box<dyn Launcher>], runs: &[LauncherRun]) -> GateInputs {
    let mut inputs = GateInputs::new();

    // Attribution: worst launched-tree attribution across every run.
    let min_attr = runs
        .iter()
        .filter(|r| !r.series.is_empty())
        .map(|r| r.series.min_tree_attributed_fraction())
        .fold(f64::INFINITY, f64::min);
    if min_attr.is_finite() {
        inputs.min_attributed_fraction = Some(min_attr);
    }

    // Overhead: cost of one sample against the daemon's realistic steady-state cadence, not
    // the (much tighter) benchmark sampling interval. §4.2 asks for < 2% CPU at 20 tasks; a
    // steady daemon samples on the order of once per second, so we evaluate against that.
    let memmux_names: Vec<&str> = launchers
        .iter()
        .filter(|l| l.kind() == LauncherKind::MemMux)
        .map(|l| l.name())
        .collect();
    let max_overhead = runs
        .iter()
        .filter(|r| memmux_names.contains(&r.launcher.as_str()))
        .map(|r| r.series.overhead_fraction(REFERENCE_CADENCE_MS))
        .fold(0.0_f64, f64::max);
    if !memmux_names.is_empty() {
        inputs.sampling_overhead_fraction = Some(max_overhead);
    }

    // Bounded memory: growth of the memmux soak run, if present.
    if let Some(soak) = runs
        .iter()
        .find(|r| r.scenario == Scenario::Soak && memmux_names.contains(&r.launcher.as_str()))
    {
        inputs.bounded_growth_bytes = Some(soak.series.root_subtree_growth_bytes());
    }
    inputs.bounded_growth_limit_bytes = 100 * MIB;

    inputs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::MemMuxLauncher;

    #[test]
    fn gate_inputs_take_worst_attribution() {
        let launchers: Vec<Box<dyn Launcher>> = vec![Box::new(MemMuxLauncher)];
        let runs = vec![LauncherRun {
            launcher: "memmux".into(),
            scenario: Scenario::Burst,
            series: TimeSeries::new(vec![crate::sampler::TimeSeriesRecord {
                t_unix_ms: 0,
                elapsed_ms: 0,
                launcher: "memmux".into(),
                scenario: "burst".into(),
                sample_duration_us: 100,
                process_count: 2,
                total_bytes: 100,
                owned_bytes: 90,
                shared_bytes: 0,
                escaped_bytes: 0,
                unknown_bytes: 10,
                attributed_fraction: 0.9,
                root_subtree_bytes: 90,
                root_process_count: 2,
                tree_attributed_fraction: 0.95,
            }]),
        }];
        let inputs = derive_gate_inputs(&launchers, &runs);
        assert_eq!(inputs.min_attributed_fraction, Some(0.95));
        assert!(inputs.sampling_overhead_fraction.is_some());
    }
}
