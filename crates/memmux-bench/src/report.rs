//! Report generator (SUM-37).
//!
//! Renders collected time series and gate results into a self-contained Markdown report with
//! tables and inline Unicode sparklines — no plotting dependencies, so a report can be produced
//! anywhere the harness runs and committed as text.

use crate::cleanup::CleanupResult;
use crate::escape::EscapeResult;
use crate::gates::{GateResult, GateStatus};
use crate::h4::{H4Evidence, NoLostWork, OvercommitGovernance};
use crate::sampler::TimeSeries;
use crate::stats::{summarize, TrialStats};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

const MIB: f64 = 1024.0 * 1024.0;

/// Grace window (seconds) cited in the cleanup table caption; matches `run::CLEANUP_GRACE_MS`.
const CLEANUP_GRACE_SECONDS: u64 = 10;

/// Aggregated cleanup / leak-on-teardown statistics for one launcher × scenario (SUM-165 / H2).
///
/// Each field is a [`TrialStats`] over the trials that produced a cleanup measurement (trials with
/// nothing owned are excluded). `reclaimed_fraction` is the mean reclaimed fraction; it and the CI
/// are meaningful across `n >= 2` trials. This whole struct is `None` on a [`RunSummary`] when no
/// trial produced cleanup data (the launcher owned no live agents at teardown).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CleanupSummary {
    /// Owned agent-subtree process count (captured pre-teardown), across trials.
    pub owned_procs: TrialStats,
    /// Leaked (surviving) process count after the grace window, across trials.
    pub leaked_procs: TrialStats,
    /// Leaked bytes (sum of capture-time accounted bytes of survivors), across trials.
    pub leaked_bytes: TrialStats,
    /// Reclaimed fraction `(owned - leaked) / owned`, across trials.
    pub reclaimed_fraction: TrialStats,
}

impl CleanupSummary {
    /// Aggregate the per-trial cleanup results, ignoring trials that produced no measurement.
    ///
    /// Returns `None` when no trial had cleanup data, so the report shows nothing for launchers
    /// that owned no live agents at teardown (n/a, never a fabricated row).
    pub fn from_trials(trials: &[Option<CleanupResult>]) -> Option<Self> {
        let present: Vec<&CleanupResult> = trials.iter().filter_map(|c| c.as_ref()).collect();
        if present.is_empty() {
            return None;
        }
        let owned: Vec<f64> = present.iter().map(|c| c.owned_procs as f64).collect();
        let leaked: Vec<f64> = present.iter().map(|c| c.leaked_procs as f64).collect();
        let bytes: Vec<f64> = present.iter().map(|c| c.leaked_bytes as f64).collect();
        // Only trials whose owned>0 have a defined fraction (that is exactly `present`, since a
        // `Some` cleanup result implies owned>0).
        let frac: Vec<f64> = present.iter().filter_map(|c| c.cleanup_fraction).collect();
        Some(Self {
            owned_procs: summarize(&owned),
            leaked_procs: summarize(&leaked),
            leaked_bytes: summarize(&bytes),
            reclaimed_fraction: summarize(&frac),
        })
    }
}

/// Aggregated escaped-process visibility statistics for one launcher × scenario (SUM-166 / H3).
///
/// Escape counts are fixed by construction (one injection per agent) and detection is
/// deterministic, so this is not a distribution: the fields are the worst-case (min) confirmed /
/// detected across trials, so a single flaky trial can only make the numbers *look worse*, never
/// better. `detected` is `None` for launchers with no escape-detection mechanism (rendered
/// *unsupported*, never a measured-looking `0`). This whole struct is `None` on a [`RunSummary`]
/// when the scenario injected no escapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscapeSummary {
    /// Escapes injected per trial (agents × escapes-per-agent).
    pub injected: usize,
    /// Worst-case (min across trials) independently-confirmed reparented escapes.
    pub injected_confirmed: usize,
    /// Worst-case (min across trials) launcher-detected escapes, or `None` when the launcher has no
    /// escape-detection mechanism.
    pub detected: Option<usize>,
}

impl EscapeSummary {
    /// Aggregate the per-trial escape results, ignoring trials that produced no measurement.
    ///
    /// Returns `None` when no trial had escape data, so the report shows nothing for scenarios that
    /// injected no escapes. Takes the min confirmed/detected across trials (worst case). `detected`
    /// stays `None` iff every present trial reported `None` (the launcher is unsupported).
    pub fn from_trials(trials: &[Option<EscapeResult>]) -> Option<Self> {
        let present: Vec<&EscapeResult> = trials.iter().filter_map(|c| c.as_ref()).collect();
        if present.is_empty() {
            return None;
        }
        let injected = present.iter().map(|e| e.injected).max().unwrap_or(0);
        let injected_confirmed = present
            .iter()
            .map(|e| e.injected_confirmed)
            .min()
            .unwrap_or(0);
        // Detected is `Some` iff any present trial had a mechanism; then take the worst (min).
        let detected = if present.iter().all(|e| e.detected.is_none()) {
            None
        } else {
            present.iter().filter_map(|e| e.detected).min()
        };
        Some(Self {
            injected,
            injected_confirmed,
            detected,
        })
    }
}

/// Aggregated H4 governance + no-lost-work evidence for one launcher × overcommit run
/// (SUM-167/168). Governance counts are deterministic per trial, so this takes the **maximum**
/// across trials (the strongest evidence that the mechanism engaged); the no-lost-work fraction is
/// the **worst-case (min)** across trials so a single flaky trial can only look worse, never better.
/// `None` on a [`RunSummary`] when the launcher is ungoverned (rendered *unsupported*, never a
/// fabricated zero).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct H4Summary {
    /// Worst-case (max) admission deferrals + pressure reclamations across trials.
    pub governance: OvercommitGovernance,
    /// Worst-case (min-preserved) no-lost-work counts across trials with a reclamation.
    pub no_lost_work: NoLostWork,
}

impl H4Summary {
    /// Aggregate per-trial [`H4Evidence`], ignoring trials with no evidence (ungoverned launchers).
    ///
    /// Returns `None` when no trial produced evidence (the launcher is unsupported / ungoverned).
    /// Governance counts take the max across trials; the no-lost-work counts take the trial with the
    /// lowest preserved fraction (ties broken toward more victims) so the reported fraction is the
    /// worst observed.
    pub fn from_trials(trials: &[Option<H4Evidence>]) -> Option<Self> {
        let present: Vec<&H4Evidence> = trials.iter().filter_map(|h| h.as_ref()).collect();
        if present.is_empty() {
            return None;
        }
        let governance = OvercommitGovernance {
            admission_deferrals: present
                .iter()
                .map(|e| e.governance.admission_deferrals)
                .max()
                .unwrap_or(0),
            reclamations: present
                .iter()
                .map(|e| e.governance.reclamations)
                .max()
                .unwrap_or(0),
        };
        // Worst-case no-lost-work: the trial with the lowest preserved fraction. Trials with no
        // reclamation (fraction n/a) are treated as fraction 1.0 for ranking so they never win the
        // "worst" slot over a trial that actually reclaimed something.
        let worst = present
            .iter()
            .min_by(|a, b| {
                let fa = a.no_lost_work.preserved_fraction().unwrap_or(1.0);
                let fb = b.no_lost_work.preserved_fraction().unwrap_or(1.0);
                fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("present is non-empty");
        Some(Self {
            governance,
            no_lost_work: worst.no_lost_work,
        })
    }
}

/// Report-level metadata cited at the top of the Markdown report (SUM-37 core).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReportMeta {
    /// Measurement time (UTC, `YYYY-MM-DD HH:MM:SSZ`).
    pub measured_at_utc: String,
    /// Host operating system (`std::env::consts::OS`).
    pub host_os: String,
    /// Per-launcher `(name, version)` citations for launchers that actually ran.
    pub launcher_versions: Vec<(String, String)>,
}

impl ReportMeta {
    /// Capture metadata for a report generated now, citing the given launcher versions.
    pub fn now(launcher_versions: Vec<(String, String)>) -> Self {
        Self {
            measured_at_utc: format_utc(SystemTime::now()),
            host_os: std::env::consts::OS.to_string(),
            launcher_versions,
        }
    }
}

/// Per-run summary distilled from one launcher × scenario, aggregated across K trials (SUM-162).
///
/// Each headline metric is a [`TrialStats`] over the K per-trial values (mean / median / sample
/// stddev / 95% CI half-width). A single-trial run (K=1) yields stats with `n == 1` and zero
/// spread, so the single-trial path stays coherent. Scalar fields (`mean_sample_us`, `samples`,
/// `peak_procs`, `footprint_spark`) are taken from the first (representative) trial.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    /// Launcher name.
    pub launcher: String,
    /// Resolved launcher version.
    pub launcher_version: String,
    /// Scenario slug.
    pub scenario: String,
    /// Peak tracked-subtree footprint, mebibytes (representative trial).
    pub peak_root_mib: f64,
    /// First→last growth of the tracked subtree, mebibytes (representative trial).
    pub growth_mib: f64,
    /// Peak **total** footprint (providers + manager overhead), mebibytes, across trials.
    pub peak_total_mib: TrialStats,
    /// Steady-state (last-sample) **total** footprint, mebibytes, across trials.
    pub steady_total_mib: TrialStats,
    /// Peak **manager overhead** (multiplexer server/daemon RSS), mebibytes, across trials.
    pub peak_manager_mib: TrialStats,
    /// Steady-state (last-sample) **manager overhead**, mebibytes, across trials.
    pub steady_manager_mib: TrialStats,
    /// Worst-case launched-tree attributed fraction across trials.
    pub min_attributed_fraction: TrialStats,
    /// Sampling overhead fraction at the run's interval, across trials.
    pub overhead_fraction: TrialStats,
    /// Measured manager-process CPU% over the run (H5), averaged across trials that could read
    /// it. `None` when no launcher had a manager pid or CPU time was unreadable on this host.
    pub manager_cpu_pct: Option<f64>,
    /// Mean per-sample cost, microseconds (representative trial).
    pub mean_sample_us: f64,
    /// Number of samples (representative trial).
    pub samples: usize,
    /// Peak number of processes in the launched tree (representative trial).
    pub peak_procs: usize,
    /// Sparkline of the total footprint over time (representative trial).
    pub footprint_spark: String,
    /// Number of trials aggregated (K).
    pub trials: usize,
    /// Aggregated cleanup / leak-on-teardown stats (SUM-165 / H2); `None` when no trial owned any
    /// live agents at teardown (the "Cleanup on teardown" table then omits this launcher×scenario).
    pub cleanup: Option<CleanupSummary>,
    /// Aggregated escaped-process visibility stats (SUM-166 / H3); `None` when the scenario injected
    /// no escapes (the "Escaped-process detection" table then omits this launcher×scenario).
    pub escape: Option<EscapeSummary>,
    /// Constrained agent budget in MiB for the overcommit run (SUM-167 / H4a), or `None` outside the
    /// overcommit scenario (or when no budget override was set). Shown in the H4a table.
    pub overcommit_budget_mib: Option<f64>,
    /// Aggregated H4 governance + no-lost-work stats (SUM-167/168); `None` for an ungoverned launcher
    /// or outside the overcommit scenario (the H4 tables then render this launcher as unsupported).
    pub h4: Option<H4Summary>,
}

impl RunSummary {
    /// Distill a single time series into a summary (the K=1 path); each metric stat has `n == 1`.
    pub fn from_series(
        launcher: &str,
        version: &str,
        scenario: &str,
        ts: &TimeSeries,
        interval_ms: u64,
    ) -> Self {
        Self::from_trials(
            launcher,
            version,
            scenario,
            std::slice::from_ref(ts),
            interval_ms,
            None,
            &[],
            &[],
            &[],
            None,
        )
    }

    /// Aggregate K per-trial time series into one summary, computing a [`TrialStats`] for each
    /// headline metric from the K per-series values (SUM-162).
    ///
    /// `manager_cpu_pct` is the real measured manager-process CPU% for the run (SUM-164 / H5) when
    /// a manager pid could be sampled, else `None` (the report then falls back to the per-sample
    /// overhead proxy). `trial_cleanup` carries each trial's leak-on-teardown result (SUM-165 / H2),
    /// aggregated into [`CleanupSummary`]. `trial_escape` carries each trial's escaped-process
    /// visibility result (SUM-166 / H3), aggregated into [`EscapeSummary`]. The representative
    /// scalar fields come from the first trial.
    // Each argument is a distinct measurement axis for one launcher × scenario (identity, the K
    // per-trial series, the sampling cadence, and the three per-trial measurement vectors); a
    // parameter struct would only rename the same fields, so allow the arity here.
    #[allow(clippy::too_many_arguments)]
    pub fn from_trials(
        launcher: &str,
        version: &str,
        scenario: &str,
        trials: &[TimeSeries],
        interval_ms: u64,
        manager_cpu_pct: Option<f64>,
        trial_cleanup: &[Option<CleanupResult>],
        trial_escape: &[Option<EscapeResult>],
        trial_h4: &[Option<H4Evidence>],
        overcommit_budget_mib: Option<f64>,
    ) -> Self {
        let peak_total: Vec<f64> = trials
            .iter()
            .map(|ts| ts.peak_total_bytes() as f64 / MIB)
            .collect();
        let steady_total: Vec<f64> = trials
            .iter()
            .map(|ts| ts.steady_total_bytes() as f64 / MIB)
            .collect();
        let peak_manager: Vec<f64> = trials
            .iter()
            .map(|ts| ts.peak_manager_overhead_bytes() as f64 / MIB)
            .collect();
        let steady_manager: Vec<f64> = trials
            .iter()
            .map(|ts| ts.steady_manager_overhead_bytes() as f64 / MIB)
            .collect();
        // The launched-tree attribution is the meaningful metric (see sampler docs); an empty
        // series is treated as fully attributed so it never drags the gate metric down.
        let attributed: Vec<f64> = trials
            .iter()
            .map(|ts| {
                if ts.is_empty() {
                    1.0
                } else {
                    ts.min_tree_attributed_fraction()
                }
            })
            .collect();
        let overhead: Vec<f64> = trials
            .iter()
            .map(|ts| ts.overhead_fraction(interval_ms))
            .collect();

        // Representative (first) trial for scalar/shape fields.
        let rep = trials.first();
        let footprint: Vec<f64> = rep
            .map(|ts| {
                ts.records
                    .iter()
                    .map(|r| r.total_bytes as f64 / MIB)
                    .collect()
            })
            .unwrap_or_default();

        Self {
            launcher: launcher.to_string(),
            launcher_version: version.to_string(),
            scenario: scenario.to_string(),
            peak_root_mib: rep
                .map(|ts| ts.peak_root_subtree_bytes() as f64 / MIB)
                .unwrap_or(0.0),
            growth_mib: rep
                .map(|ts| ts.root_subtree_growth_bytes() as f64 / MIB)
                .unwrap_or(0.0),
            peak_total_mib: summarize(&peak_total),
            steady_total_mib: summarize(&steady_total),
            peak_manager_mib: summarize(&peak_manager),
            steady_manager_mib: summarize(&steady_manager),
            min_attributed_fraction: summarize(&attributed),
            overhead_fraction: summarize(&overhead),
            manager_cpu_pct,
            mean_sample_us: rep.map(|ts| ts.mean_sample_duration_us()).unwrap_or(0.0),
            samples: rep.map(|ts| ts.records.len()).unwrap_or(0),
            peak_procs: rep.map(|ts| ts.peak_root_process_count()).unwrap_or(0),
            footprint_spark: sparkline(&footprint),
            trials: trials.len(),
            cleanup: CleanupSummary::from_trials(trial_cleanup),
            escape: EscapeSummary::from_trials(trial_escape),
            overcommit_budget_mib,
            h4: H4Summary::from_trials(trial_h4),
        }
    }
}

/// Render a run of the benchmark as a Markdown document.
///
/// The per-run table shows **Total footprint (peak / steady)** and **Manager overhead
/// (peak / steady)** side by side, plus the footprint sparkline. A metadata header cites the
/// measurement time (UTC), host OS, and each launcher's version, and lists any skipped launchers
/// with their reason (§19.5 claims discipline).
pub fn render_markdown(
    title: &str,
    summaries: &[RunSummary],
    gates: &[GateResult],
    meta: &ReportMeta,
    skipped: &[(String, String)],
    figures: &[(String, String)],
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {title}\n");

    // Metadata header.
    let _ = writeln!(out, "- **Measured (UTC):** {}", meta.measured_at_utc);
    let _ = writeln!(out, "- **Host OS:** {}", meta.host_os);
    if !meta.launcher_versions.is_empty() {
        let cited = meta
            .launcher_versions
            .iter()
            .map(|(name, ver)| format!("{name} = {ver}"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "- **Launcher versions:** {cited}");
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "_Generated by `memmux-bench`. Competitor launchers appear only when their binary was \
present on `PATH` and could be driven headlessly; others are listed as skipped-with-reason \
rather than estimated (§19.5)._\n"
    );

    // Per-run table.
    let _ = writeln!(out, "## Runs\n");
    // Report the max K across summaries so the ± note states the actual trial count.
    let trials = summaries.iter().map(|s| s.trials).max().unwrap_or(1);
    let _ = writeln!(
        out,
        "_\"Total footprint\" is provider (agent) memory plus the multiplexer's own **manager \
overhead**; the two are broken out side by side. \"Tree attributed\" is the fraction of the \
launched process tree mapped back to the owning task._\n"
    );
    let _ = writeln!(
        out,
        "_Each `mean ±halfwidth` cell is the mean over **K = {trials}** trial(s); ± is the 95% \
confidence-interval half-width (`1.96·σ/√n`, sample σ), so it is `0.0` for a single trial. \
\"Manager CPU %\" is the measured CPU utilization of the multiplexer's manager process across \
the run (H5); it is `n/a` when the launcher has no manager process or CPU time is unreadable on \
this host._\n"
    );
    let _ = writeln!(
        out,
        "| Launcher | Version | Scenario | Procs | Total peak MiB | Total steady MiB | Manager overhead peak MiB | Manager overhead steady MiB | Tree attributed | Mean sample (µs) | Overhead % | Manager CPU % | Samples | Footprint |"
    );
    let _ = writeln!(
        out,
        "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |"
    );
    for s in summaries {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.0} | {} | {} | {} | `{}` |",
            s.launcher,
            s.launcher_version,
            s.scenario,
            s.peak_procs,
            fmt_stat(&s.peak_total_mib, 1),
            fmt_stat(&s.steady_total_mib, 1),
            fmt_stat(&s.peak_manager_mib, 1),
            fmt_stat(&s.steady_manager_mib, 1),
            fmt_stat_pct(&s.min_attributed_fraction),
            s.mean_sample_us,
            fmt_stat_pct(&s.overhead_fraction),
            fmt_cpu_pct(s.manager_cpu_pct),
            s.samples,
            s.footprint_spark,
        );
    }

    // Cleanup on teardown (H2, SUM-165): one row per launcher×scenario that produced cleanup data.
    let cleanup_rows: Vec<&RunSummary> = summaries.iter().filter(|s| s.cleanup.is_some()).collect();
    if !cleanup_rows.is_empty() {
        let _ = writeln!(out, "\n## Cleanup on teardown (H2)\n");
        let _ = writeln!(
            out,
            "_Reclaimed % = fraction of the owned agent subtree gone within {}s of the launcher's \
normal teardown. `mean ±halfwidth` is over trials with cleanup data (± is the 95% CI half-width, \
`0.0` for a single trial)._\n",
            CLEANUP_GRACE_SECONDS
        );
        let _ = writeln!(
            out,
            "| Launcher | Scenario | Owned procs | Leaked procs | Leaked MiB | Reclaimed % |"
        );
        let _ = writeln!(out, "| --- | --- | ---: | ---: | ---: | ---: |");
        for s in cleanup_rows {
            let c = s.cleanup.expect("filtered to Some above");
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} |",
                s.launcher,
                s.scenario,
                fmt_stat(&c.owned_procs, 1),
                fmt_stat(&c.leaked_procs, 1),
                fmt_stat(&mib_stat(&c.leaked_bytes), 1),
                fmt_stat_pct(&c.reclaimed_fraction),
            );
        }
    }

    // Escaped-process detection (H3, SUM-166): one row per launcher×scenario that injected escapes.
    let escape_rows: Vec<&RunSummary> = summaries.iter().filter(|s| s.escape.is_some()).collect();
    if !escape_rows.is_empty() {
        let _ = writeln!(out, "\n## Escaped-process detection (H3)\n");
        let _ = writeln!(
            out,
            "_A process that reparents to init escapes its agent and silently leaks. MemMux's \
daemon surfaces it as a `process_escaped` event; tmux/herdr/raw have no such concept, so their \
detection is **unsupported** — an honest \"n/a — no such capability\", not a measured `0`. \
\"Reparented (confirmed)\" is the harness's independent check that the injected process actually \
left every agent subtree while still alive._\n"
        );
        let _ = writeln!(
            out,
            "| Launcher | Injected | Reparented (confirmed) | Detected | Notes |"
        );
        let _ = writeln!(out, "| --- | ---: | ---: | ---: | --- |");
        for s in escape_rows {
            let e = s.escape.expect("filtered to Some above");
            let (detected, notes) = match e.detected {
                Some(d) => (
                    d.to_string(),
                    format!(
                        "detected {}/{} escaped process(es)",
                        d, e.injected_confirmed
                    ),
                ),
                None => (
                    "n/a".to_string(),
                    "unsupported (no escape-detection mechanism)".to_string(),
                ),
            };
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                s.launcher, e.injected, e.injected_confirmed, detected, notes,
            );
        }
    }

    // Overcommit / bounded footprint (H4a, SUM-167) + No-lost-work (H4b, SUM-168): one row per
    // launcher that ran the overcommit scenario. MemMux is governed (real deferral/reclamation
    // counts); the baselines are ungoverned (no governance mechanism).
    let overcommit_rows: Vec<&RunSummary> = summaries
        .iter()
        .filter(|s| s.scenario == "overcommit")
        .collect();
    if !overcommit_rows.is_empty() {
        let _ = writeln!(out, "\n## Overcommit / bounded footprint (H4a)\n");
        let _ = writeln!(
            out,
            "_Under a **constrained agent budget** (set below the aggregate predicted peak of N \
agents) MemMux governs — its admission planner defers starts that would not fit and its pressure \
ladder reclaims resident agents — so its footprint stays near/under budget. raw/tmux/herdr are \
**ungoverned**: they run all N agents, so \"Governance actions\" is an honest \"n/a — no such \
mechanism\", not a measured `0`. \"Budget MiB\" applies only to MemMux (the others have no budget \
to enforce). Swap-growth SLO is the Linux corollary, measured on the Linux reference host via the \
per-process swap columns; on this host the gate is evaluated on footprint._\n"
        );
        let _ = writeln!(
            out,
            "| Launcher | Budget MiB | Peak total MiB | Steady total MiB | Governance actions | Notes |"
        );
        let _ = writeln!(out, "| --- | ---: | ---: | ---: | ---: | --- |");
        for s in &overcommit_rows {
            let budget = s
                .overcommit_budget_mib
                .map(|b| format!("{b:.0}"))
                .unwrap_or_else(|| "n/a".to_string());
            let (actions, notes) = match &s.h4 {
                Some(h) => (
                    h.governance.total_actions().to_string(),
                    format!(
                        "governed: {} deferral(s) + {} reclamation(s)",
                        h.governance.admission_deferrals, h.governance.reclamations
                    ),
                ),
                None => (
                    "n/a".to_string(),
                    "none (ungoverned — no admission/pressure mechanism)".to_string(),
                ),
            };
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} |",
                s.launcher,
                budget,
                fmt_stat(&s.peak_total_mib, 1),
                fmt_stat(&s.steady_total_mib, 1),
                actions,
                notes,
            );
        }

        let _ = writeln!(out, "\n## No-lost-work (H4b)\n");
        let _ = writeln!(
            out,
            "_Before the pressure ladder terminates a victim it first captures a durable checkpoint \
(git HEAD + patch hash + dirty manifest). \"Checkpoints captured\" counts victims for which a \
checkpoint carrying a **non-empty** git patch hash was persisted first; \"Preserved %\" is \
captured/reclaimed. raw/tmux/herdr have no checkpoint mechanism, so this is **unsupported** — a \
capability comparison, NOT a fabricated \"lost N files\". When no victim was reclaimed this run, \
the metric is n/a (honest — the budget did not force a termination)._\n"
        );
        let _ = writeln!(
            out,
            "| Launcher | Victims reclaimed | Checkpoints captured | Preserved % | Notes |"
        );
        let _ = writeln!(out, "| --- | ---: | ---: | ---: | --- |");
        for s in &overcommit_rows {
            let (victims, captured, preserved, notes) = match &s.h4 {
                Some(h) => {
                    let n = &h.no_lost_work;
                    let preserved = match n.preserved_fraction() {
                        Some(f) => format!("{:.1}%", f * 100.0),
                        None => "n/a".to_string(),
                    };
                    let notes = if n.victims_reclaimed == 0 {
                        "governed: no reclamation triggered this run".to_string()
                    } else {
                        format!(
                            "governed: {}/{} preserved before termination",
                            n.checkpoints_captured, n.victims_reclaimed
                        )
                    };
                    (
                        n.victims_reclaimed.to_string(),
                        n.checkpoints_captured.to_string(),
                        preserved,
                        notes,
                    )
                }
                None => (
                    "n/a".to_string(),
                    "n/a".to_string(),
                    "n/a".to_string(),
                    "unsupported (no checkpoint mechanism)".to_string(),
                ),
            };
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                s.launcher, victims, captured, preserved, notes,
            );
        }
    }

    // Figures (embedded dependency-free SVG next to the report; SUM-163).
    if !figures.is_empty() {
        let _ = writeln!(out, "\n## Figures\n");
        for (fig_title, file) in figures {
            let _ = writeln!(out, "![{fig_title}]({file})\n");
        }
    }

    // Skipped launchers.
    if !skipped.is_empty() {
        let _ = writeln!(out, "\n## Skipped launchers\n");
        let _ = writeln!(out, "| Launcher | Reason |");
        let _ = writeln!(out, "| --- | --- |");
        for (name, reason) in skipped {
            let _ = writeln!(out, "| {name} | {reason} |");
        }
    }

    // Gates table.
    let _ = writeln!(out, "\n## Launch gates (§18.5)\n");
    let _ = writeln!(out, "| Gate | Status | Detail |");
    let _ = writeln!(out, "| --- | --- | --- |");
    for g in gates {
        let _ = writeln!(
            out,
            "| {} | {} | {} |",
            g.name,
            status_badge(g.status),
            g.detail
        );
    }

    out
}

/// Format a metric [`TrialStats`] as `mean ±halfwidth` at `prec` decimal places (SUM-162).
fn fmt_stat(s: &TrialStats, prec: usize) -> String {
    format!("{:.*} ±{:.*}", prec, s.mean, prec, s.ci95_halfwidth)
}

/// Format a fractional metric [`TrialStats`] as a percentage `mean% ±halfwidth%`.
fn fmt_stat_pct(s: &TrialStats) -> String {
    format!("{:.1}% ±{:.1}", s.mean * 100.0, s.ci95_halfwidth * 100.0)
}

/// Rescale a bytes-valued [`TrialStats`] into mebibytes for display (mean, median, stddev, CI all
/// divide by [`MIB`]; `n` is unchanged).
fn mib_stat(s: &TrialStats) -> TrialStats {
    TrialStats {
        mean: s.mean / MIB,
        median: s.median / MIB,
        stddev: s.stddev / MIB,
        ci95_halfwidth: s.ci95_halfwidth / MIB,
        n: s.n,
    }
}

/// Format the optional measured manager CPU% (H5); `n/a` when unmeasured.
fn fmt_cpu_pct(pct: Option<f64>) -> String {
    match pct {
        Some(p) => format!("{p:.3}%"),
        None => "n/a".to_string(),
    }
}

fn status_badge(status: GateStatus) -> &'static str {
    match status {
        GateStatus::Pass => "✅ pass",
        GateStatus::Fail => "❌ fail",
        GateStatus::Skipped => "⚪ skipped",
    }
}

/// Format a [`SystemTime`] as a UTC `YYYY-MM-DD HH:MM:SSZ` string (no external date crate).
///
/// Uses a civil-from-days algorithm on the Unix epoch; times before 1970 format as the epoch.
fn format_utc(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{year:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}Z")
}

/// Render a slice of values as a Unicode block sparkline.
///
/// An empty slice yields an empty string; a flat slice yields the lowest block.
pub fn sparkline(values: &[f64]) -> String {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if values.is_empty() {
        return String::new();
    }
    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let range = max - min;
    values
        .iter()
        .map(|&v| {
            if range <= f64::EPSILON {
                BLOCKS[0]
            } else {
                let idx = ((v - min) / range * (BLOCKS.len() as f64 - 1.0)).round() as usize;
                BLOCKS[idx.min(BLOCKS.len() - 1)]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampler::TimeSeriesRecord;

    fn rec(elapsed: u64, provider: u64, manager: u64) -> TimeSeriesRecord {
        TimeSeriesRecord {
            t_unix_ms: 0,
            elapsed_ms: elapsed,
            launcher: "memmux".into(),
            scenario: "leak".into(),
            sample_duration_us: 300,
            process_count: 1,
            total_bytes: provider + manager,
            owned_bytes: provider,
            shared_bytes: 0,
            escaped_bytes: 0,
            unknown_bytes: 0,
            attributed_fraction: 1.0,
            root_subtree_bytes: provider,
            root_process_count: 2,
            tree_attributed_fraction: 1.0,
            provider_bytes: provider,
            manager_overhead_bytes: manager,
            provider_proc_count: 2,
            manager_proc_count: if manager > 0 { 1 } else { 0 },
            launcher_version: "memmux 0.0.0".into(),
        }
    }

    #[test]
    fn sparkline_shapes() {
        assert_eq!(sparkline(&[]), "");
        assert_eq!(sparkline(&[5.0, 5.0, 5.0]), "▁▁▁");
        let s = sparkline(&[0.0, 1.0, 2.0, 3.0]);
        assert_eq!(s.chars().count(), 4);
        assert_eq!(s.chars().next().unwrap(), '▁');
        assert_eq!(s.chars().last().unwrap(), '█');
    }

    #[test]
    fn summary_from_series_breaks_out_manager_overhead() {
        let ts = TimeSeries::new(vec![
            rec(0, (100.0 * MIB) as u64, (10.0 * MIB) as u64),
            rec(100, (180.0 * MIB) as u64, (20.0 * MIB) as u64),
        ]);
        let s = RunSummary::from_series("memmux", "memmux 0.0.0", "leak", &ts, 1000);
        // Single-trial: each metric stat has n == 1 and zero spread.
        assert_eq!(s.trials, 1);
        assert_eq!(s.peak_total_mib.n, 1);
        assert!((s.peak_total_mib.mean - 200.0).abs() < 0.5);
        assert_eq!(s.peak_total_mib.ci95_halfwidth, 0.0);
        assert!((s.steady_total_mib.mean - 200.0).abs() < 0.5);
        assert!((s.peak_manager_mib.mean - 20.0).abs() < 0.5);
        assert!((s.steady_manager_mib.mean - 20.0).abs() < 0.5);
        assert_eq!(s.samples, 2);
        assert_eq!(s.footprint_spark.chars().count(), 2);
    }

    #[test]
    fn from_trials_aggregates_across_k_series() {
        // Two trials with different peak totals → mean stat with non-zero CI.
        let t1 = TimeSeries::new(vec![rec(0, (100.0 * MIB) as u64, (10.0 * MIB) as u64)]);
        let t2 = TimeSeries::new(vec![rec(0, (140.0 * MIB) as u64, (10.0 * MIB) as u64)]);
        let s = RunSummary::from_trials(
            "memmux",
            "memmux 0.0.0",
            "burst",
            &[t1, t2],
            1000,
            Some(1.23),
            &[],
            &[],
            &[],
            None,
        );
        assert_eq!(s.trials, 2);
        assert_eq!(s.peak_total_mib.n, 2);
        // Provider 100 + manager 10 = 110, and 140 + 10 = 150 → mean 130.
        assert!((s.peak_total_mib.mean - 130.0).abs() < 0.5);
        assert!(s.peak_total_mib.ci95_halfwidth > 0.0);
        assert_eq!(s.manager_cpu_pct, Some(1.23));
    }

    #[test]
    fn markdown_contains_tables_metadata_and_columns() {
        let ts = TimeSeries::new(vec![rec(0, (50.0 * MIB) as u64, (5.0 * MIB) as u64)]);
        let summaries = vec![RunSummary::from_series(
            "raw-baseline",
            "raw (direct spawn)",
            "burst",
            &ts,
            1000,
        )];
        let gates = vec![
            GateResult {
                name: "Attribution".into(),
                status: GateStatus::Pass,
                detail: "min 99%".into(),
            },
            GateResult {
                name: "Resume".into(),
                status: GateStatus::Skipped,
                detail: "Phase 2".into(),
            },
        ];
        let meta = ReportMeta {
            measured_at_utc: "2026-08-09 12:00:00Z".into(),
            host_os: "macos".into(),
            launcher_versions: vec![("tmux".into(), "tmux 3.6a".into())],
        };
        let skipped = vec![("cmux".into(), "binary not available on this host".into())];
        let figures = vec![(
            "Footprint over time".to_string(),
            "footprint-over-time.svg".to_string(),
        )];
        let md = render_markdown(
            "MemMux benchmark",
            &summaries,
            &gates,
            &meta,
            &skipped,
            &figures,
        );
        assert!(md.contains("## Runs"));
        assert!(md.contains("## Launch gates"));
        assert!(md.contains("raw-baseline"));
        assert!(md.contains("Total peak MiB"));
        assert!(md.contains("Manager overhead peak MiB"));
        assert!(md.contains("Manager CPU %"));
        // The ± CI note states the trial count K and the manager-CPU column semantics.
        assert!(md.contains("95%"));
        assert!(md.contains("K = 1"));
        // A single-trial cell renders `mean ±0.0`.
        assert!(md.contains("±0.0"));
        // raw baseline has no manager pid → CPU cell is n/a.
        assert!(md.contains("n/a"));
        assert!(md.contains("Measured (UTC)"));
        assert!(md.contains("2026-08-09"));
        assert!(md.contains("tmux 3.6a"));
        assert!(md.contains("## Skipped launchers"));
        assert!(md.contains("cmux"));
        // The figures section references the embedded SVG.
        assert!(md.contains("## Figures"));
        assert!(md.contains("![Footprint over time](footprint-over-time.svg)"));
        assert!(md.contains("✅ pass"));
        assert!(md.contains("⚪ skipped"));
    }

    #[test]
    fn cleanup_summary_aggregates_present_trials_only() {
        let full = CleanupResult {
            owned_procs: 6,
            leaked_procs: 0,
            leaked_bytes: 0,
            cleanup_fraction: Some(1.0),
        };
        let leaky = CleanupResult {
            owned_procs: 6,
            leaked_procs: 3,
            leaked_bytes: 3 * MIB as u64,
            cleanup_fraction: Some(0.5),
        };
        // A `None` trial (nothing owned) is ignored, not averaged in as a zero.
        let summary = CleanupSummary::from_trials(&[Some(full), None, Some(leaky)]).unwrap();
        assert_eq!(summary.owned_procs.n, 2);
        assert!((summary.owned_procs.mean - 6.0).abs() < 1e-9);
        assert!((summary.leaked_procs.mean - 1.5).abs() < 1e-9);
        assert!((summary.reclaimed_fraction.mean - 0.75).abs() < 1e-9);
        // Leaked bytes mean is (0 + 3 MiB) / 2 = 1.5 MiB.
        assert!((summary.leaked_bytes.mean - 1.5 * MIB).abs() < 1.0);

        // No cleanup data at all → None (n/a, never a fabricated row).
        assert!(CleanupSummary::from_trials(&[None, None]).is_none());
        assert!(CleanupSummary::from_trials(&[]).is_none());
    }

    #[test]
    fn markdown_renders_cleanup_table_when_present() {
        let ts = TimeSeries::new(vec![rec(0, (50.0 * MIB) as u64, (5.0 * MIB) as u64)]);
        // raw-baseline leaks 1 of 2 owned; memmux reclaims all 2.
        let raw_cleanup = vec![Some(CleanupResult {
            owned_procs: 2,
            leaked_procs: 1,
            leaked_bytes: 30 * MIB as u64,
            cleanup_fraction: Some(0.5),
        })];
        let mmx_cleanup = vec![Some(CleanupResult {
            owned_procs: 2,
            leaked_procs: 0,
            leaked_bytes: 0,
            cleanup_fraction: Some(1.0),
        })];
        let summaries = vec![
            RunSummary::from_trials(
                "raw-baseline",
                "raw (direct spawn)",
                "hold",
                std::slice::from_ref(&ts),
                1000,
                None,
                &raw_cleanup,
                &[],
                &[],
                None,
            ),
            RunSummary::from_trials(
                "memmux",
                "memmux 0.0.0",
                "hold",
                std::slice::from_ref(&ts),
                1000,
                None,
                &mmx_cleanup,
                &[],
                &[],
                None,
            ),
        ];
        let meta = ReportMeta {
            measured_at_utc: "2026-08-10 00:00:00Z".into(),
            host_os: "macos".into(),
            launcher_versions: vec![],
        };
        let md = render_markdown("t", &summaries, &[], &meta, &[], &[]);
        assert!(md.contains("## Cleanup on teardown (H2)"));
        assert!(md.contains("Reclaimed %"));
        // The honest differentiator: raw reclaims 50%, memmux reclaims 100%.
        assert!(md.contains("50.0%"));
        assert!(md.contains("100.0%"));
        // Caption cites the 10s grace window.
        assert!(md.contains("10s"));
    }

    #[test]
    fn markdown_omits_cleanup_table_when_no_data() {
        let ts = TimeSeries::new(vec![rec(0, (50.0 * MIB) as u64, 0)]);
        let summaries = vec![RunSummary::from_series(
            "raw-baseline",
            "raw",
            "burst",
            &ts,
            1000,
        )];
        let meta = ReportMeta::default();
        let md = render_markdown("t", &summaries, &[], &meta, &[], &[]);
        assert!(!md.contains("## Cleanup on teardown"));
    }

    #[test]
    fn escape_summary_aggregates_and_marks_unsupported() {
        // MemMux: two trials, one detects 2, one detects 1 → worst-case min 1; confirmed min 2.
        let mmx = EscapeSummary::from_trials(&[
            Some(EscapeResult {
                injected: 2,
                injected_confirmed: 2,
                detected: Some(2),
            }),
            Some(EscapeResult {
                injected: 2,
                injected_confirmed: 2,
                detected: Some(1),
            }),
        ])
        .unwrap();
        assert_eq!(mmx.injected, 2);
        assert_eq!(mmx.injected_confirmed, 2);
        assert_eq!(mmx.detected, Some(1));

        // A baseline: injected/confirmed but no detection mechanism → detected stays None.
        let raw = EscapeSummary::from_trials(&[Some(EscapeResult {
            injected: 2,
            injected_confirmed: 2,
            detected: None,
        })])
        .unwrap();
        assert_eq!(raw.detected, None);

        // No escape data at all → None (n/a, table omits the row).
        assert!(EscapeSummary::from_trials(&[None, None]).is_none());
        assert!(EscapeSummary::from_trials(&[]).is_none());
    }

    #[test]
    fn markdown_renders_escape_table_with_memmux_and_unsupported_baseline() {
        let ts = TimeSeries::new(vec![rec(0, (50.0 * MIB) as u64, (5.0 * MIB) as u64)]);
        // raw-baseline: injected+confirmed but cannot detect; memmux detects all.
        let raw_escape = vec![Some(EscapeResult {
            injected: 2,
            injected_confirmed: 2,
            detected: None,
        })];
        let mmx_escape = vec![Some(EscapeResult {
            injected: 2,
            injected_confirmed: 2,
            detected: Some(2),
        })];
        let summaries = vec![
            RunSummary::from_trials(
                "raw-baseline",
                "raw (direct spawn)",
                "escape",
                std::slice::from_ref(&ts),
                1000,
                None,
                &[],
                &raw_escape,
                &[],
                None,
            ),
            RunSummary::from_trials(
                "memmux",
                "memmux 0.0.0",
                "escape",
                std::slice::from_ref(&ts),
                1000,
                None,
                &[],
                &mmx_escape,
                &[],
                None,
            ),
        ];
        let meta = ReportMeta::default();
        let md = render_markdown("t", &summaries, &[], &meta, &[], &[]);
        assert!(md.contains("## Escaped-process detection (H3)"));
        assert!(md.contains("Reparented (confirmed)"));
        // MemMux reports a real detection; the baseline is explicitly unsupported (not a `0`).
        assert!(md.contains("detected 2/2 escaped process(es)"));
        assert!(md.contains("unsupported (no escape-detection mechanism)"));
    }

    #[test]
    fn markdown_omits_escape_table_when_no_data() {
        let ts = TimeSeries::new(vec![rec(0, (50.0 * MIB) as u64, 0)]);
        let summaries = vec![RunSummary::from_series(
            "raw-baseline",
            "raw",
            "burst",
            &ts,
            1000,
        )];
        let md = render_markdown("t", &summaries, &[], &ReportMeta::default(), &[], &[]);
        assert!(!md.contains("## Escaped-process detection"));
    }

    #[test]
    fn format_utc_is_correct_at_epoch_and_a_known_date() {
        assert_eq!(format_utc(UNIX_EPOCH), "1970-01-01 00:00:00Z");
        // 1_700_000_000 = 2023-11-14 22:13:20 UTC.
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        assert_eq!(format_utc(t), "2023-11-14 22:13:20Z");
    }
}
