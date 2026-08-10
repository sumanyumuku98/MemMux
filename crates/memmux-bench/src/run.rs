//! Live benchmark orchestration: launch a stub under a launcher and sample it over time.
//!
//! This is the glue that turns the pure pieces (scenarios, launchers, sampler, gates, report)
//! into an end-to-end run and is the integration point that actually exercises
//! `memmux-metrics` against real processes.

use crate::cleanup::{survivors_of, CleanupResult, OwnedProc};
use crate::escape::{confirm_reparented, EscapeResult};
use crate::gates::{evaluate_gates, GateInputs, GateResult};
use crate::launcher::{LaunchSpec, LaunchTopology, Launcher, LauncherKind};
use crate::plot::{line_chart_svg, Series, PALETTE};
use crate::report::{render_markdown, ReportMeta, RunSummary};
use crate::sampler::{
    tagged_processes, write_tagged_processes_jsonl, TaggedProc, TimeSeries, TimeSeriesRecord,
};
use crate::scenario::Scenario;
use memmux_core::ids::Pid;
use memmux_core::Provider;
use memmux_metrics::{default_sampler, process_cpu_seconds, ProcessTree};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MIB: u64 = 1024 * 1024;

/// Realistic steady-state sampling cadence the overhead gate is evaluated against (ms).
const REFERENCE_CADENCE_MS: u64 = 1000;

/// Total grace window we allow a launcher's normal teardown before counting survivors (SUM-165).
const CLEANUP_GRACE_MS: u64 = 10_000;
/// Poll cadence within the cleanup grace window (20 × 500ms = 10s).
const CLEANUP_POLL_MS: u64 = 500;

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
    /// Number of identical stub agents to launch per launcher (default 3).
    pub agents: usize,
    /// Number of repeated trials per (launcher, scenario) for statistics (default 1) (SUM-162).
    pub trials: usize,
    /// Path to the `memmux-bench` binary (used to execute the stub).
    pub bench_exe: PathBuf,
    /// Directory for recordings and JSONL output.
    pub workdir: PathBuf,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            provider: Provider::Generic,
            intensity: 1,
            interval_ms: 100,
            max_samples: 20,
            agents: 3,
            trials: 1,
            bench_exe: PathBuf::new(),
            workdir: PathBuf::from("bench-out"),
        }
    }
}

/// The result of running one launcher against one scenario across K trials.
#[derive(Clone, Debug)]
pub struct LauncherRun {
    /// Launcher name.
    pub launcher: String,
    /// Resolved launcher version at measurement time.
    pub version: String,
    /// Scenario.
    pub scenario: Scenario,
    /// Representative sampled time series (trial 1), retained for backward-compatible accessors.
    pub series: TimeSeries,
    /// All K per-trial time series (SUM-162).
    pub trials: Vec<TimeSeries>,
    /// All K per-trial tagged per-process row sets (trial 1 first), for the swap figure (SUM-163).
    pub trial_proc_rows: Vec<Vec<TaggedProc>>,
    /// Mean measured manager CPU% across the trials that could read it (SUM-164 / H5), or `None`.
    pub manager_cpu_pct: Option<f64>,
    /// Per-trial cleanup / leak-on-teardown measurements (SUM-165 / H2); `None` for a trial where
    /// nothing was owned. Aggregated into the report's "Cleanup on teardown" table.
    pub trial_cleanup: Vec<Option<CleanupResult>>,
    /// Per-trial escaped-process visibility measurements (SUM-166 / H3); `None` for a trial whose
    /// scenario injected no escapes. Aggregated into the report's "Escaped-process detection" table.
    pub trial_escape: Vec<Option<EscapeResult>>,
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
    /// Launchers skipped this run, each paired with the reason (§19.5 claims discipline).
    pub skipped: Vec<(String, String)>,
    /// Report metadata (measurement time, host OS, per-launcher versions).
    pub meta: ReportMeta,
    /// Generated figures as `(title, relative_svg_filename)` pairs, referenced from the report
    /// (SUM-163). Empty when no figure could be produced.
    pub figures: Vec<(String, String)>,
}

impl BenchOutcome {
    /// Render the outcome as a Markdown report, embedding references to the generated SVG figures.
    pub fn to_markdown(&self, title: &str) -> String {
        render_markdown(
            title,
            &self.summaries,
            &self.gates,
            &self.meta,
            &self.skipped,
            &self.figures,
        )
    }
}

/// The measured outcome of one trial: the sampled series plus, for launchers with a manager
/// process, the real manager-process CPU% over the run (SUM-164 / H5).
#[derive(Clone, Debug)]
pub struct TrialResult {
    /// The sampled time series for this trial.
    pub series: TimeSeries,
    /// Measured manager-process CPU utilization over the run (e.g. `0.6` = 0.6%), or `None` when
    /// the launcher has no manager pid or CPU time could not be read on this host.
    pub manager_cpu_pct: Option<f64>,
    /// Tagged per-process rows for this trial (for the swap-over-time figure, SUM-163).
    pub proc_rows: Vec<TaggedProc>,
    /// Cleanup / leak-on-teardown measurement for this trial (SUM-165 / H2), or `None` when the
    /// launcher owned no live agent processes at teardown (measurement is n/a, never a fake 100%).
    pub cleanup: Option<CleanupResult>,
    /// Escaped-process visibility measurement for this trial (SUM-166 / H3), or `None` when the
    /// scenario injected no escapes (measurement is n/a for it).
    pub escape: Option<EscapeResult>,
}

/// Run one launcher against one scenario, launching `cfg.agents` identical stubs and sampling the
/// whole topology (providers vs. manager overhead) over its lifetime.
pub fn run_launcher_scenario(
    launcher: &dyn Launcher,
    scenario: Scenario,
    cfg: &RunConfig,
) -> anyhow::Result<TimeSeries> {
    Ok(run_launcher_scenario_measured(launcher, scenario, cfg)?.series)
}

/// Run one trial and also measure the manager process's CPU% across the run (SUM-164).
///
/// The manager CPU% is `(Σ cpu_seconds(manager_pids, end) − Σ cpu_seconds(manager_pids, start)) /
/// wall_seconds × 100`, sampled right after launch and right before teardown. It is `None` when
/// the topology declares no manager pid (e.g. the raw baseline) or CPU time is unreadable on this
/// host — in which case the caller falls back to the per-sample-duration overhead proxy.
pub fn run_launcher_scenario_measured(
    launcher: &dyn Launcher,
    scenario: Scenario,
    cfg: &RunConfig,
) -> anyhow::Result<TrialResult> {
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
    let session = launcher.start(cfg.agents.max(1), &spec)?;
    let version = launcher.version();
    let sampler = default_sampler();

    // Baseline manager CPU time immediately after launch, before the sampling loop.
    let manager_pids = session.topology().manager_pids.clone();
    let cpu_start = sum_cpu_seconds(&manager_pids);

    // How many escapes this scenario injects per agent (SUM-166 / H3); 0 for every scenario but
    // Escape. When > 0 we track which pids we ever saw under an agent-root subtree, so we can
    // later independently confirm the ones that reparented away.
    let escapes_per_agent = scenario.escapes_per_agent(cfg.provider);
    let mut ever_seen_under_root: std::collections::HashSet<Pid> = std::collections::HashSet::new();

    let started = Instant::now();
    let mut records = Vec::new();
    let mut proc_rows: Vec<TaggedProc> = Vec::new();
    loop {
        let elapsed_ms = started.elapsed().as_millis() as u64;
        // One snapshot per tick feeds both the aggregate record and the per-process rows (SUM-33).
        if let Ok(snapshot) = sampler.snapshot() {
            let tree = ProcessTree::from_samples(snapshot.samples.clone());
            // Record every pid currently in an agent-root subtree so we can later tell which ones
            // reparented out (an escape) vs. which simply exited (SUM-166 / H3).
            if escapes_per_agent > 0 {
                for &root in &session.topology().agent_roots {
                    if tree.get(root).is_some() {
                        ever_seen_under_root.insert(root);
                        ever_seen_under_root.extend(tree.descendants(root));
                    }
                }
            }
            records.push(TimeSeriesRecord::from_snapshot_topology(
                &snapshot,
                session.topology(),
                launcher.name(),
                &version,
                scenario.slug(),
                elapsed_ms,
            ));
            proc_rows.extend(tagged_processes(
                &tree,
                session.topology(),
                launcher.name(),
                scenario.slug(),
                snapshot.taken_at_unix_ms,
                elapsed_ms,
            ));
        }

        if records.len() >= cfg.max_samples {
            break;
        }
        // Stop early once every agent process has exited (provider footprint went to zero).
        if all_agents_gone(session.topology()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(cfg.interval_ms));
    }

    // Final manager CPU time + wall clock, sampled before teardown while the pids still live.
    let cpu_end = sum_cpu_seconds(&manager_pids);
    let wall_seconds = started.elapsed().as_secs_f64();
    let manager_cpu_pct = manager_cpu_pct(cpu_start, cpu_end, wall_seconds);

    // Cleanup / leak-on-teardown (SUM-165 / H2): capture the owned agent subtree WHILE it is still
    // alive, then measure how much of it survives the launcher's own normal teardown.
    let owned = capture_owned_set(session.topology());

    // Escaped-process visibility (SUM-166 / H3): while the daemon is still alive, (1) read the
    // launcher's own detected escapes (only MemMux surfaces any — the trait default is `None`),
    // and (2) independently confirm which of the pids we ever saw under an agent root have now
    // reparented out of every root subtree while still alive. Only meaningful for scenarios that
    // inject escapes; `None` otherwise (n/a, never a fabricated row).
    let escape = if escapes_per_agent > 0 {
        Some(measure_escape(
            session.as_ref(),
            escapes_per_agent.saturating_mul(cfg.agents.max(1)),
            &ever_seen_under_root,
        ))
    } else {
        None
    };

    // Tear the whole session down so we never leak processes (§2.4 "own what you launch"). Each
    // launcher runs its OWN normal teardown here — no MemMux special-casing (fairness is the whole
    // point of H2).
    session.stop();

    // Poll survivors for the grace window; kill any leftovers so the harness never leaks the
    // processes it caused to exist.
    let cleanup = measure_cleanup(&owned);

    // Persist the detailed per-process tagged rows next to the aggregate series (SUM-33).
    if !proc_rows.is_empty() {
        let procs_path = cfg.workdir.join(format!(
            "{}-{}.procs.jsonl",
            launcher.name(),
            scenario.slug()
        ));
        write_tagged_processes_jsonl(&procs_path, &proc_rows)?;
    }
    Ok(TrialResult {
        series: TimeSeries::new(records),
        manager_cpu_pct,
        proc_rows,
        cleanup,
        escape,
    })
}

/// Measure escaped-process visibility for one trial (SUM-166 / H3).
///
/// `injected` is the known count of escapes the harness created (agents × escapes-per-agent).
/// `candidates` is every pid we ever saw under an agent-root subtree during the run. This takes a
/// fresh snapshot, independently confirms which candidates reparented out of every live agent-root
/// subtree (`injected_confirmed`), reads the launcher's own detected escapes (`detected`, `None`
/// for launchers with no such mechanism), and then reaps the confirmed-escaped pids so the harness
/// never leaks the orphans it caused to exist.
fn measure_escape(
    session: &dyn crate::launcher::LaunchedSession,
    injected: usize,
    candidates: &std::collections::HashSet<Pid>,
) -> EscapeResult {
    // The launcher's own detection (dedup already done by the daemon / reader).
    let detected = session.escaped_pids().map(|pids| pids.len());

    // Independent confirmation against a fresh live tree, scoped to the still-alive agent roots.
    let candidate_vec: Vec<Pid> = candidates.iter().copied().collect();
    let agent_roots = &session.topology().agent_roots;
    let confirmed = match default_sampler().snapshot() {
        Ok(snapshot) => {
            let tree = ProcessTree::from_samples(snapshot.samples);
            confirm_reparented(&tree, &candidate_vec, agent_roots)
        }
        // If we cannot sample we simply confirm none (never fabricate a reparent).
        Err(_) => Vec::new(),
    };

    // Reap the orphans the harness caused to exist so nothing is left running (they hold memory and
    // sleep for tens of seconds otherwise). Best-effort SIGKILL, same as the cleanup path.
    kill_survivors(&confirmed);

    EscapeResult {
        injected,
        injected_confirmed: confirmed.len(),
        detected,
    }
}

/// Capture the *owned set* for the cleanup measurement: every live pid in each agent-root subtree,
/// with the bytes we accounted to it (SUM-165 / H2).
///
/// Uses one live snapshot; only agent-root subtrees are included (the agent subtrees the launcher
/// promised to own), deliberately excluding manager pids' non-provider descendants — H2 is about
/// whether the launcher reclaims the agents it launched. Roots that are already dead contribute
/// nothing. An empty owned set later yields a `None` cleanup result (n/a, not a fake 100%).
fn capture_owned_set(topology: &LaunchTopology) -> Vec<OwnedProc> {
    let snapshot = match default_sampler().snapshot() {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let tree = ProcessTree::from_samples(snapshot.samples);
    let mut owned: Vec<OwnedProc> = Vec::new();
    let mut seen: std::collections::HashSet<Pid> = std::collections::HashSet::new();
    for &root in &topology.agent_roots {
        // Only count a root that is actually alive right now.
        if tree.get(root).is_none() {
            continue;
        }
        for pid in std::iter::once(root).chain(tree.descendants(root)) {
            if !seen.insert(pid) {
                continue;
            }
            if let Some(sample) = tree.get(pid) {
                owned.push(OwnedProc {
                    pid,
                    accounted_bytes: sample.accounted_bytes(),
                });
            }
        }
    }
    owned
}

/// Measure teardown cleanup: poll the live tree for up to the grace window, tracking how many owned
/// pids survive, then SIGKILL any survivors so the benchmark itself never leaks them (SUM-165).
///
/// Returns `None` when nothing was owned (measurement is n/a). Otherwise returns a
/// [`CleanupResult`] built from the survivor set at the end of the grace, or as soon as it reaches
/// zero.
fn measure_cleanup(owned: &[OwnedProc]) -> Option<CleanupResult> {
    if owned.is_empty() {
        return None;
    }
    let polls = (CLEANUP_GRACE_MS / CLEANUP_POLL_MS).max(1);
    let mut survivors = owned.iter().map(|o| o.pid).collect::<Vec<Pid>>();
    for i in 0..polls {
        // Sleep first so the launcher's teardown has a moment to take effect before the first poll.
        std::thread::sleep(Duration::from_millis(CLEANUP_POLL_MS));
        survivors = current_survivors(owned);
        if survivors.is_empty() {
            let _ = i; // reached zero early; stop polling.
            break;
        }
    }
    let result = CleanupResult::from_owned_and_survivors(owned, &survivors);
    // Reap what the harness caused to exist: best-effort SIGKILL of any survivors (fine to clean
    // up AFTER measuring). `#![forbid(unsafe_code)]` rules out a raw `libc::kill`, so shell out to
    // the platform `kill` on unix; on other platforms there is nothing portable to do here.
    kill_survivors(&survivors);
    Some(result)
}

/// One live poll: which owned pids are still present in the current process tree.
fn current_survivors(owned: &[OwnedProc]) -> Vec<Pid> {
    match default_sampler().snapshot() {
        Ok(snapshot) => {
            let tree = ProcessTree::from_samples(snapshot.samples);
            survivors_of(owned, &tree)
        }
        // If we cannot sample, assume everything still survives (never under-report a leak).
        Err(_) => owned.iter().map(|o| o.pid).collect(),
    }
}

/// Best-effort SIGKILL of leftover pids so the harness reaps what it caused to exist (SUM-165).
#[cfg(unix)]
fn kill_survivors(pids: &[Pid]) {
    for &pid in pids {
        let _ = std::process::Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// Non-unix hosts have no portable SIGKILL-by-pid here; survivors are left to the launcher.
#[cfg(not(unix))]
fn kill_survivors(_pids: &[Pid]) {}

/// Sum cumulative CPU seconds across a set of manager pids, or `None` if the set is empty or no
/// pid's CPU time can be read (so an absent measurement stays honest, never a fabricated 0).
fn sum_cpu_seconds(pids: &[Pid]) -> Option<f64> {
    if pids.is_empty() {
        return None;
    }
    let mut total = 0.0;
    let mut any = false;
    for &pid in pids {
        if let Some(s) = process_cpu_seconds(pid) {
            total += s;
            any = true;
        }
    }
    if any {
        Some(total)
    } else {
        None
    }
}

/// Compute manager CPU% from start/end CPU seconds and wall seconds, as a percentage.
///
/// H5: the paper's ≤2% CPU-at-N=20 claim is exactly this measured manager-process CPU%.
/// Returns `None` unless both CPU readings and a positive wall duration are available; a tiny
/// negative delta (from CPU-time quantization) is clamped to 0.
fn manager_cpu_pct(start: Option<f64>, end: Option<f64>, wall_seconds: f64) -> Option<f64> {
    let (start, end) = (start?, end?);
    if wall_seconds <= 0.0 {
        return None;
    }
    let delta = (end - start).max(0.0);
    Some(delta / wall_seconds * 100.0)
}

/// Whether none of the topology's agent roots are still alive in the live process tree.
fn all_agents_gone(topology: &crate::launcher::LaunchTopology) -> bool {
    if topology.agent_roots.is_empty() {
        return true;
    }
    match default_sampler().snapshot() {
        Ok(snapshot) => {
            let tree = memmux_metrics::ProcessTree::from_samples(snapshot.samples);
            topology.agent_roots.iter().all(|p| tree.get(*p).is_none())
        }
        // If we cannot sample, keep going rather than stopping prematurely.
        Err(_) => false,
    }
}

/// Run every launcher against the given scenarios and evaluate the launch gates.
///
/// Unavailable launchers, and launchers whose `start()` errors, are recorded in
/// [`BenchOutcome::skipped`] with a reason and never abort the whole benchmark — a fabricated or
/// zero measurement is never emitted for a launcher that could not be driven (§19.5).
pub fn run_benchmark(
    launchers: &[Box<dyn Launcher>],
    scenarios: &[Scenario],
    cfg: &RunConfig,
) -> anyhow::Result<BenchOutcome> {
    let mut runs = Vec::new();
    let mut summaries = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();

    let mut versions: Vec<(String, String)> = Vec::new();
    for launcher in launchers {
        if !launcher.is_available() {
            skipped.push((
                launcher.name().to_string(),
                "binary not available on this host".to_string(),
            ));
            continue;
        }
        let version = launcher.version();
        versions.push((launcher.name().to_string(), version.clone()));
        let n_trials = cfg.trials.max(1);
        for &scenario in scenarios {
            // Run K trials, collecting one TimeSeries + one CPU% + one proc-row set per trial.
            let mut trial_series: Vec<TimeSeries> = Vec::with_capacity(n_trials);
            let mut trial_proc_rows: Vec<Vec<TaggedProc>> = Vec::with_capacity(n_trials);
            let mut trial_cleanup: Vec<Option<CleanupResult>> = Vec::with_capacity(n_trials);
            let mut trial_escape: Vec<Option<EscapeResult>> = Vec::with_capacity(n_trials);
            let mut cpu_samples: Vec<f64> = Vec::new();
            let mut trial_error: Option<String> = None;
            for trial in 0..n_trials {
                match run_launcher_scenario_measured(launcher.as_ref(), scenario, cfg) {
                    Ok(result) => {
                        // Persist each trial's raw JSONL so raw data is kept (SUM-162).
                        let jsonl = cfg.workdir.join(format!(
                            "{}-{}.trial{}.jsonl",
                            launcher.name(),
                            scenario.slug(),
                            trial + 1
                        ));
                        result.series.write_jsonl(&jsonl)?;
                        if let Some(pct) = result.manager_cpu_pct {
                            cpu_samples.push(pct);
                        }
                        trial_series.push(result.series);
                        trial_proc_rows.push(result.proc_rows);
                        trial_cleanup.push(result.cleanup);
                        trial_escape.push(result.escape);
                    }
                    Err(e) => {
                        trial_error = Some(e.to_string());
                        break;
                    }
                }
            }

            // If no trial produced a series, record the launcher/scenario as skipped-with-reason.
            if trial_series.is_empty() {
                skipped.push((
                    format!("{} ({})", launcher.name(), scenario.slug()),
                    trial_error.unwrap_or_else(|| "no trials produced a series".to_string()),
                ));
                continue;
            }

            let manager_cpu_pct = if cpu_samples.is_empty() {
                None
            } else {
                Some(cpu_samples.iter().sum::<f64>() / cpu_samples.len() as f64)
            };

            summaries.push(RunSummary::from_trials(
                launcher.name(),
                &version,
                scenario.slug(),
                &trial_series,
                cfg.interval_ms,
                manager_cpu_pct,
                &trial_cleanup,
                &trial_escape,
            ));
            runs.push(LauncherRun {
                launcher: launcher.name().to_string(),
                version: version.clone(),
                scenario,
                series: trial_series[0].clone(),
                trials: trial_series,
                trial_proc_rows,
                manager_cpu_pct,
                trial_cleanup,
                trial_escape,
            });
        }
    }

    // Emit the embedded SVG figures next to the report (SUM-163); failures are non-fatal.
    let figures = write_figures(&cfg.workdir, &runs).unwrap_or_default();

    let gates = evaluate_gates(&derive_gate_inputs(launchers, &runs));
    let meta = ReportMeta::now(versions);
    Ok(BenchOutcome {
        runs,
        summaries,
        gates,
        skipped,
        meta,
        figures,
    })
}

/// Write the embedded SVG figures next to the report and return `(title, filename)` references
/// for the ones actually produced (SUM-163).
///
/// Figures:
/// 1. `footprint-over-time.svg` — per-launcher total footprint (MiB) vs elapsed ms (trial 1).
/// 2. `swap-over-time.svg` — per-launcher total provider swap (bytes) vs elapsed ms. On a host
///    that never swaps this is a flat zero line, which is rendered honestly rather than dropped.
/// 3. `footprint-vs-N.svg` — peak total vs agent count; only emitted when an N-sweep is present
///    (multiple distinct agent counts across runs), otherwise skipped gracefully (N-sweep
///    orchestration is SUM-169/P3).
fn write_figures(workdir: &Path, runs: &[LauncherRun]) -> anyhow::Result<Vec<(String, String)>> {
    if runs.is_empty() {
        return Ok(Vec::new());
    }
    let mut figures: Vec<(String, String)> = Vec::new();

    // Figure 1: footprint over time (one series per launcher, using the representative trial).
    let footprint_series: Vec<Series> = runs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let points: Vec<(f64, f64)> = r
                .series
                .records
                .iter()
                .map(|rec| (rec.elapsed_ms as f64, rec.total_bytes as f64 / MIB as f64))
                .collect();
            Series::new(
                format!("{} / {}", r.launcher, r.scenario.slug()),
                points,
                PALETTE[i % PALETTE.len()],
            )
        })
        .collect();
    let svg = line_chart_svg(
        "Footprint over time",
        "elapsed (ms)",
        "total footprint (MiB)",
        &footprint_series,
    );
    let name = "footprint-over-time.svg";
    std::fs::write(workdir.join(name), svg)?;
    figures.push(("Footprint over time".to_string(), name.to_string()));

    // Figure 2: swap over time (per-launcher sum of provider swap_bytes per tick, from proc rows).
    let swap_series: Vec<Series> = runs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            // Sum provider swap per elapsed_ms bucket across the representative trial's rows.
            let rows = r.trial_proc_rows.first();
            let mut points: Vec<(f64, f64)> = Vec::new();
            if let Some(rows) = rows {
                let mut by_tick: std::collections::BTreeMap<u64, u64> =
                    std::collections::BTreeMap::new();
                for row in rows {
                    let swap = row.swap_bytes.unwrap_or(0);
                    *by_tick.entry(row.elapsed_ms).or_insert(0) += swap;
                }
                points = by_tick
                    .into_iter()
                    .map(|(t, b)| (t as f64, b as f64))
                    .collect();
            }
            Series::new(
                format!("{} / {}", r.launcher, r.scenario.slug()),
                points,
                PALETTE[i % PALETTE.len()],
            )
        })
        .collect();
    let svg = line_chart_svg(
        "Provider swap over time",
        "elapsed (ms)",
        "provider swap (bytes)",
        &swap_series,
    );
    let name = "swap-over-time.svg";
    std::fs::write(workdir.join(name), svg)?;
    figures.push(("Provider swap over time".to_string(), name.to_string()));

    // Figure 3: footprint vs N — only when an N-sweep is present (multiple agent counts). Here a
    // single run has one agent count, so we detect distinct peak-process counts per launcher as a
    // proxy; with no sweep we skip gracefully (SUM-169/P3 owns N-sweep orchestration).
    let mut by_launcher: std::collections::BTreeMap<String, Vec<(f64, f64)>> =
        std::collections::BTreeMap::new();
    for r in runs {
        let n = r.series.peak_root_process_count() as f64;
        let peak = r.series.peak_total_bytes() as f64 / MIB as f64;
        by_launcher
            .entry(r.launcher.clone())
            .or_default()
            .push((n, peak));
    }
    let has_sweep = by_launcher.values().any(|pts| {
        let mut xs: Vec<f64> = pts.iter().map(|(x, _)| *x).collect();
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        xs.dedup();
        xs.len() > 1
    });
    if has_sweep {
        let series: Vec<Series> = by_launcher
            .into_iter()
            .enumerate()
            .map(|(i, (name, mut pts))| {
                pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                Series::new(name, pts, PALETTE[i % PALETTE.len()])
            })
            .collect();
        let svg = line_chart_svg(
            "Peak footprint vs N (agents)",
            "processes in launched tree (N)",
            "peak total footprint (MiB)",
            &series,
        );
        let name = "footprint-vs-N.svg";
        std::fs::write(workdir.join(name), svg)?;
        figures.push(("Peak footprint vs N".to_string(), name.to_string()));
    }

    Ok(figures)
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

    // Overhead: the §4.2 NFR asks for < 2% CPU at 20 tasks. When we can read the MemMux manager
    // process's real CPU% across the run (SUM-164 / H5), that measured number IS the gate input
    // (converted percent → fraction). Only when CPU time is unreadable do we fall back to the
    // per-sample-duration proxy against the daemon's realistic steady-state cadence (a steady
    // daemon samples on the order of once per second, so we evaluate the proxy against that).
    let memmux_names: Vec<&str> = launchers
        .iter()
        .filter(|l| l.kind() == LauncherKind::MemMux)
        .map(|l| l.name())
        .collect();
    let memmux_runs = || {
        runs.iter()
            .filter(|r| memmux_names.contains(&r.launcher.as_str()))
    };
    // Prefer the real measured manager CPU% (worst-case across MemMux runs).
    let real_overhead = memmux_runs()
        .filter_map(|r| r.manager_cpu_pct)
        .fold(f64::NEG_INFINITY, f64::max);
    if !memmux_names.is_empty() {
        if real_overhead.is_finite() {
            // Measured CPU% is a percentage; the gate compares a fraction.
            inputs.sampling_overhead_fraction = Some(real_overhead / 100.0);
        } else {
            // Fallback proxy: per-sample cost against the steady-state cadence.
            let max_overhead = memmux_runs()
                .map(|r| r.series.overhead_fraction(REFERENCE_CADENCE_MS))
                .fold(0.0_f64, f64::max);
            inputs.sampling_overhead_fraction = Some(max_overhead);
        }
    }

    // Bounded memory: growth of the memmux soak run, if present.
    if let Some(soak) = runs
        .iter()
        .find(|r| r.scenario == Scenario::Soak && memmux_names.contains(&r.launcher.as_str()))
    {
        inputs.bounded_growth_bytes = Some(soak.series.root_subtree_growth_bytes());
    }
    inputs.bounded_growth_limit_bytes = 100 * MIB;

    // Cleanup (SUM-165 / H2): worst-case reclaimed fraction across all MemMux trials that produced
    // a cleanup measurement. `None` (gate stays skipped) if no MemMux trial owned live agents.
    let min_cleanup = memmux_runs()
        .flat_map(|r| r.trial_cleanup.iter())
        .filter_map(|c| c.as_ref().and_then(|c| c.cleanup_fraction))
        .fold(f64::INFINITY, f64::min);
    if min_cleanup.is_finite() {
        inputs.min_cleanup_fraction = Some(min_cleanup);
    }

    inputs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::MemMuxLauncher;

    fn memmux_record() -> crate::sampler::TimeSeriesRecord {
        crate::sampler::TimeSeriesRecord {
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
            provider_bytes: 90,
            manager_overhead_bytes: 10,
            provider_proc_count: 2,
            manager_proc_count: 1,
            launcher_version: "memmux 0.0.0".into(),
        }
    }

    fn memmux_run(manager_cpu_pct: Option<f64>) -> LauncherRun {
        memmux_run_with_cleanup(manager_cpu_pct, vec![None])
    }

    fn memmux_run_with_cleanup(
        manager_cpu_pct: Option<f64>,
        trial_cleanup: Vec<Option<CleanupResult>>,
    ) -> LauncherRun {
        let series = TimeSeries::new(vec![memmux_record()]);
        LauncherRun {
            launcher: "memmux".into(),
            version: "memmux 0.0.0".into(),
            scenario: Scenario::Burst,
            series: series.clone(),
            trials: vec![series],
            trial_proc_rows: vec![Vec::new()],
            manager_cpu_pct,
            trial_cleanup,
            trial_escape: vec![None],
        }
    }

    #[test]
    fn gate_inputs_take_worst_attribution() {
        let launchers: Vec<Box<dyn Launcher>> = vec![Box::new(MemMuxLauncher)];
        let runs = vec![memmux_run(None)];
        let inputs = derive_gate_inputs(&launchers, &runs);
        assert_eq!(inputs.min_attributed_fraction, Some(0.95));
        // With no measured CPU%, the overhead falls back to the per-sample proxy.
        assert!(inputs.sampling_overhead_fraction.is_some());
    }

    #[test]
    fn real_cpu_pct_becomes_the_overhead_gate_input() {
        let launchers: Vec<Box<dyn Launcher>> = vec![Box::new(MemMuxLauncher)];
        // Measured 1.5% CPU → overhead fraction 0.015 (real number, not the proxy).
        let runs = vec![memmux_run(Some(1.5))];
        let inputs = derive_gate_inputs(&launchers, &runs);
        assert_eq!(inputs.sampling_overhead_fraction, Some(0.015));
    }

    #[test]
    fn cleanup_gate_input_takes_worst_memmux_reclaim() {
        use crate::cleanup::CleanupResult;
        let launchers: Vec<Box<dyn Launcher>> = vec![Box::new(MemMuxLauncher)];
        // Two trials: one fully reclaimed, one that leaked a quarter → worst-case 0.75.
        let full = CleanupResult {
            owned_procs: 4,
            leaked_procs: 0,
            leaked_bytes: 0,
            cleanup_fraction: Some(1.0),
        };
        let leaky = CleanupResult {
            owned_procs: 4,
            leaked_procs: 1,
            leaked_bytes: 1024,
            cleanup_fraction: Some(0.75),
        };
        let runs = vec![memmux_run_with_cleanup(None, vec![Some(full), Some(leaky)])];
        let inputs = derive_gate_inputs(&launchers, &runs);
        assert_eq!(inputs.min_cleanup_fraction, Some(0.75));
    }

    #[test]
    fn cleanup_gate_input_absent_when_no_data() {
        let launchers: Vec<Box<dyn Launcher>> = vec![Box::new(MemMuxLauncher)];
        let runs = vec![memmux_run(None)]; // trial_cleanup is [None]
        let inputs = derive_gate_inputs(&launchers, &runs);
        assert_eq!(inputs.min_cleanup_fraction, None);
    }

    #[test]
    fn manager_cpu_pct_math_is_delta_over_wall() {
        // 0.5 CPU-seconds over 2 wall-seconds = 25% CPU.
        assert_eq!(manager_cpu_pct(Some(1.0), Some(1.5), 2.0), Some(25.0));
        // Missing readings or non-positive wall → None; negative delta clamps to 0.
        assert_eq!(manager_cpu_pct(None, Some(1.0), 1.0), None);
        assert_eq!(manager_cpu_pct(Some(1.0), Some(2.0), 0.0), None);
        assert_eq!(manager_cpu_pct(Some(2.0), Some(1.0), 1.0), Some(0.0));
    }
}
