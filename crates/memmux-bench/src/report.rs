//! Report generator (SUM-37).
//!
//! Renders collected time series and gate results into a self-contained Markdown report with
//! tables and inline Unicode sparklines — no plotting dependencies, so a report can be produced
//! anywhere the harness runs and committed as text.

use crate::gates::{GateResult, GateStatus};
use crate::sampler::TimeSeries;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

const MIB: f64 = 1024.0 * 1024.0;

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

/// Per-run summary distilled from one launcher × scenario time series.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    /// Launcher name.
    pub launcher: String,
    /// Resolved launcher version.
    pub launcher_version: String,
    /// Scenario slug.
    pub scenario: String,
    /// Peak tracked-subtree footprint, mebibytes.
    pub peak_root_mib: f64,
    /// First→last growth of the tracked subtree, mebibytes.
    pub growth_mib: f64,
    /// Peak **total** footprint (providers + manager overhead), mebibytes.
    pub peak_total_mib: f64,
    /// Steady-state (last-sample) **total** footprint, mebibytes.
    pub steady_total_mib: f64,
    /// Peak **manager overhead** (multiplexer server/daemon RSS), mebibytes.
    pub peak_manager_mib: f64,
    /// Steady-state (last-sample) **manager overhead**, mebibytes.
    pub steady_manager_mib: f64,
    /// Worst-case attributed fraction.
    pub min_attributed_fraction: f64,
    /// Mean per-sample cost, microseconds.
    pub mean_sample_us: f64,
    /// Sampling overhead percent at the run's interval.
    pub overhead_pct: f64,
    /// Number of samples.
    pub samples: usize,
    /// Peak number of processes in the launched tree.
    pub peak_procs: usize,
    /// Sparkline of the total footprint over time.
    pub footprint_spark: String,
}

impl RunSummary {
    /// Distill a time series into a summary at the given sampling interval.
    pub fn from_series(
        launcher: &str,
        version: &str,
        scenario: &str,
        ts: &TimeSeries,
        interval_ms: u64,
    ) -> Self {
        let footprint: Vec<f64> = ts
            .records
            .iter()
            .map(|r| r.total_bytes as f64 / MIB)
            .collect();
        Self {
            launcher: launcher.to_string(),
            launcher_version: version.to_string(),
            scenario: scenario.to_string(),
            peak_root_mib: ts.peak_root_subtree_bytes() as f64 / MIB,
            growth_mib: ts.root_subtree_growth_bytes() as f64 / MIB,
            peak_total_mib: ts.peak_total_bytes() as f64 / MIB,
            steady_total_mib: ts.steady_total_bytes() as f64 / MIB,
            peak_manager_mib: ts.peak_manager_overhead_bytes() as f64 / MIB,
            steady_manager_mib: ts.steady_manager_overhead_bytes() as f64 / MIB,
            // The launched-tree attribution is the meaningful metric (see sampler docs).
            min_attributed_fraction: if ts.is_empty() {
                1.0
            } else {
                ts.min_tree_attributed_fraction()
            },
            mean_sample_us: ts.mean_sample_duration_us(),
            overhead_pct: ts.overhead_fraction(interval_ms) * 100.0,
            samples: ts.records.len(),
            peak_procs: ts.peak_root_process_count(),
            footprint_spark: sparkline(&footprint),
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
    let _ = writeln!(
        out,
        "_\"Total footprint\" is provider (agent) memory plus the multiplexer's own **manager \
overhead**; the two are broken out side by side. \"Tree attributed\" is the fraction of the \
launched process tree mapped back to the owning task._\n"
    );
    let _ = writeln!(
        out,
        "| Launcher | Version | Scenario | Procs | Total peak MiB | Total steady MiB | Manager overhead peak MiB | Manager overhead steady MiB | Tree attributed | Mean sample (µs) | Overhead % | Samples | Footprint |"
    );
    let _ = writeln!(
        out,
        "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |"
    );
    for s in summaries {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1}% | {:.0} | {:.3} | {} | `{}` |",
            s.launcher,
            s.launcher_version,
            s.scenario,
            s.peak_procs,
            s.peak_total_mib,
            s.steady_total_mib,
            s.peak_manager_mib,
            s.steady_manager_mib,
            s.min_attributed_fraction * 100.0,
            s.mean_sample_us,
            s.overhead_pct,
            s.samples,
            s.footprint_spark,
        );
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
        assert!((s.peak_total_mib - 200.0).abs() < 0.5);
        assert!((s.steady_total_mib - 200.0).abs() < 0.5);
        assert!((s.peak_manager_mib - 20.0).abs() < 0.5);
        assert!((s.steady_manager_mib - 20.0).abs() < 0.5);
        assert_eq!(s.samples, 2);
        assert_eq!(s.footprint_spark.chars().count(), 2);
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
        let md = render_markdown("MemMux benchmark", &summaries, &gates, &meta, &skipped);
        assert!(md.contains("## Runs"));
        assert!(md.contains("## Launch gates"));
        assert!(md.contains("raw-baseline"));
        assert!(md.contains("Total peak MiB"));
        assert!(md.contains("Manager overhead peak MiB"));
        assert!(md.contains("Measured (UTC)"));
        assert!(md.contains("2026-08-09"));
        assert!(md.contains("tmux 3.6a"));
        assert!(md.contains("## Skipped launchers"));
        assert!(md.contains("cmux"));
        assert!(md.contains("✅ pass"));
        assert!(md.contains("⚪ skipped"));
    }

    #[test]
    fn format_utc_is_correct_at_epoch_and_a_known_date() {
        assert_eq!(format_utc(UNIX_EPOCH), "1970-01-01 00:00:00Z");
        // 1_700_000_000 = 2023-11-14 22:13:20 UTC.
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        assert_eq!(format_utc(t), "2023-11-14 22:13:20Z");
    }
}
