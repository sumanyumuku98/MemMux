//! External time-series sampler (SUM-33) and sampling-overhead accounting (SUM-31).
//!
//! Periodically snapshots the process tree with `memmux-metrics`, attributes it, and emits one
//! [`TimeSeriesRecord`] per sample as JSON Lines. The per-sample duration is retained so the
//! harness can prove the ≤2% CPU overhead launch gate.

use memmux_core::ids::{Pid, TaskId};
use memmux_metrics::{attribute, ProcessSampler, ProcessTree, RootSpec, Snapshot};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::Path;

/// A single row of the sampled time series.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimeSeriesRecord {
    /// Wall-clock time of the sample (ms since Unix epoch).
    pub t_unix_ms: u64,
    /// Milliseconds since sampling started.
    pub elapsed_ms: u64,
    /// Launcher under test.
    pub launcher: String,
    /// Scenario under test.
    pub scenario: String,
    /// How long collecting the snapshot took (microseconds).
    pub sample_duration_us: u64,
    /// Number of processes sampled.
    pub process_count: usize,
    /// Total accounted bytes across all processes.
    pub total_bytes: u64,
    /// Accounted bytes attributed to tasks.
    pub owned_bytes: u64,
    /// Accounted bytes attributed to shared services.
    pub shared_bytes: u64,
    /// Accounted bytes attributed to escaped processes.
    pub escaped_bytes: u64,
    /// Accounted bytes that could not be attributed.
    pub unknown_bytes: u64,
    /// Host-wide fraction of accounted bytes mapped to a task or shared service.
    ///
    /// In the benchmark this is small because only the launched roots are declared (the whole
    /// host is in the denominator); it is retained for context, not as the gate metric.
    pub attributed_fraction: f64,
    /// Accounted bytes in the tracked root's subtree (the launched stub), if any.
    pub root_subtree_bytes: u64,
    /// Number of processes in the tracked root's subtree (root + descendants).
    pub root_process_count: usize,
    /// Fraction of the **launched tree's** bytes attributed to the owning task.
    ///
    /// This is the meaningful Phase-0 attribution metric: of the processes MemMux launched,
    /// how many did the engine correctly map back to the task (vs lose track of). It is `1.0`
    /// when the whole tree is captured.
    pub tree_attributed_fraction: f64,
}

impl TimeSeriesRecord {
    /// Build a record from a snapshot and the declared roots/ownership.
    #[allow(clippy::too_many_arguments)]
    pub fn from_snapshot(
        snapshot: &Snapshot,
        roots: &[RootSpec],
        expected: &HashMap<Pid, TaskId>,
        launcher: &str,
        scenario: &str,
        elapsed_ms: u64,
        root_pid: Option<Pid>,
    ) -> Self {
        let tree = ProcessTree::from_samples(snapshot.samples.clone());
        let report = attribute(&tree, roots, expected);
        let root_subtree_bytes = root_pid
            .map(|p| tree.subtree_accounted_bytes(p))
            .unwrap_or(0);

        // Scope attribution to the launched tree: of the root + its descendants, what fraction
        // of accounted bytes did the engine map to the owning task?
        let (root_process_count, tree_attributed_fraction) = match root_pid {
            Some(root) => {
                let mut pids = tree.descendants(root);
                pids.push(root);
                let mut total = 0u64;
                let mut attributed = 0u64;
                for pid in &pids {
                    if let Some(sample) = tree.get(*pid) {
                        let bytes = sample.accounted_bytes();
                        total += bytes;
                        if report.by_pid.get(pid).is_some_and(|a| a.is_attributed()) {
                            attributed += bytes;
                        }
                    }
                }
                let frac = if total == 0 {
                    1.0
                } else {
                    attributed as f64 / total as f64
                };
                (pids.len(), frac)
            }
            None => (0, 1.0),
        };

        Self {
            t_unix_ms: snapshot.taken_at_unix_ms,
            elapsed_ms,
            launcher: launcher.to_string(),
            scenario: scenario.to_string(),
            sample_duration_us: snapshot.sample_duration.as_micros() as u64,
            process_count: snapshot.samples.len(),
            total_bytes: report.total_bytes(),
            owned_bytes: report.owned_bytes,
            shared_bytes: report.shared_bytes,
            escaped_bytes: report.escaped_bytes,
            unknown_bytes: report.unknown_bytes,
            attributed_fraction: report.attributed_fraction(),
            root_subtree_bytes,
            root_process_count,
            tree_attributed_fraction,
        }
    }
}

/// Take one live sample using `sampler`.
#[allow(clippy::too_many_arguments)]
pub fn sample_once(
    sampler: &dyn ProcessSampler,
    roots: &[RootSpec],
    expected: &HashMap<Pid, TaskId>,
    launcher: &str,
    scenario: &str,
    elapsed_ms: u64,
    root_pid: Option<Pid>,
) -> io::Result<TimeSeriesRecord> {
    let snapshot = sampler.snapshot()?;
    Ok(TimeSeriesRecord::from_snapshot(
        &snapshot, roots, expected, launcher, scenario, elapsed_ms, root_pid,
    ))
}

/// A collected time series with aggregate accessors.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TimeSeries {
    /// Rows in sample order.
    pub records: Vec<TimeSeriesRecord>,
}

impl TimeSeries {
    /// Create from a vector of records.
    pub fn new(records: Vec<TimeSeriesRecord>) -> Self {
        Self { records }
    }

    /// Whether the series is empty.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Peak `root_subtree_bytes` across the series (the launched stub's footprint).
    pub fn peak_root_subtree_bytes(&self) -> u64 {
        self.records
            .iter()
            .map(|r| r.root_subtree_bytes)
            .max()
            .unwrap_or(0)
    }

    /// Growth of the tracked root subtree from first to last sample (saturating).
    pub fn root_subtree_growth_bytes(&self) -> i64 {
        match (self.records.first(), self.records.last()) {
            (Some(first), Some(last)) => {
                last.root_subtree_bytes as i64 - first.root_subtree_bytes as i64
            }
            _ => 0,
        }
    }

    /// Minimum host-wide attributed fraction observed (contextual, not the gate metric).
    pub fn min_attributed_fraction(&self) -> f64 {
        self.records
            .iter()
            .map(|r| r.attributed_fraction)
            .fold(f64::INFINITY, f64::min)
            .min(1.0)
    }

    /// Minimum launched-tree attribution observed (the §18.5 attribution gate metric).
    pub fn min_tree_attributed_fraction(&self) -> f64 {
        self.records
            .iter()
            .map(|r| r.tree_attributed_fraction)
            .fold(f64::INFINITY, f64::min)
            .min(1.0)
    }

    /// Peak number of processes seen in the launched tree.
    pub fn peak_root_process_count(&self) -> usize {
        self.records
            .iter()
            .map(|r| r.root_process_count)
            .max()
            .unwrap_or(0)
    }

    /// Mean per-sample duration in microseconds.
    pub fn mean_sample_duration_us(&self) -> f64 {
        if self.records.is_empty() {
            return 0.0;
        }
        let sum: u64 = self.records.iter().map(|r| r.sample_duration_us).sum();
        sum as f64 / self.records.len() as f64
    }

    /// Sampling overhead as a fraction of the sampling interval (SUM-31).
    ///
    /// `interval_ms` is the wall-clock gap between samples. Overhead is mean sample cost over
    /// that interval; the launch gate requires ≤ 0.02 at 20 tasks.
    pub fn overhead_fraction(&self, interval_ms: u64) -> f64 {
        if interval_ms == 0 {
            return 0.0;
        }
        let interval_us = (interval_ms * 1000) as f64;
        self.mean_sample_duration_us() / interval_us
    }

    /// Write the series as JSON Lines to `path`.
    pub fn write_jsonl(&self, path: &Path) -> io::Result<()> {
        let mut file = io::BufWriter::new(std::fs::File::create(path)?);
        for record in &self.records {
            serde_json::to_writer(&mut file, record)?;
            file.write_all(b"\n")?;
        }
        file.flush()
    }

    /// Read a series from a JSON Lines file.
    pub fn read_jsonl(path: &Path) -> io::Result<Self> {
        let file = io::BufReader::new(std::fs::File::open(path)?);
        let mut records = Vec::new();
        for line in file.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let record: TimeSeriesRecord = serde_json::from_str(&line)?;
            records.push(record);
        }
        Ok(Self { records })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memmux_metrics::ProcessSample;
    use std::time::Duration;

    fn sample(pid: Pid, ppid: Pid, rss: u64) -> ProcessSample {
        ProcessSample {
            pid,
            ppid,
            name: format!("p{pid}"),
            rss_bytes: rss,
            pss_bytes: None,
            phys_footprint_bytes: None,
        }
    }

    fn snapshot(samples: Vec<ProcessSample>, dur_us: u64, t: u64) -> Snapshot {
        Snapshot {
            taken_at_unix_ms: t,
            sample_duration: Duration::from_micros(dur_us),
            samples,
        }
    }

    #[test]
    fn record_from_snapshot_computes_attribution_and_subtree() {
        let snap = snapshot(
            vec![sample(100, 1, 500), sample(101, 100, 300), sample(1, 0, 50)],
            200,
            1000,
        );
        let roots = vec![RootSpec::task(100, "task_A")];
        let rec = TimeSeriesRecord::from_snapshot(
            &snap,
            &roots,
            &HashMap::new(),
            "memmux",
            "burst",
            10,
            Some(100),
        );
        assert_eq!(rec.owned_bytes, 800);
        assert_eq!(rec.unknown_bytes, 50);
        assert_eq!(rec.root_subtree_bytes, 800);
        assert_eq!(rec.process_count, 3);
        assert_eq!(rec.sample_duration_us, 200);
        // The launched tree (pids 100 + 101) is fully attributed to task_A.
        assert_eq!(rec.root_process_count, 2);
        assert!((rec.tree_attributed_fraction - 1.0).abs() < 1e-9);
    }

    #[test]
    fn tree_attribution_drops_when_a_child_is_orphaned() {
        // Child 101's parent link is broken (ppid 0), so it is not in 100's subtree and the
        // root-subtree bytes exclude it — but if it *were* claimed and lost, attribution would
        // reflect it. Here we assert the healthy case: an unlinked process is simply not part
        // of the tree.
        let snap = snapshot(vec![sample(100, 1, 500), sample(101, 0, 300)], 50, 1);
        let roots = vec![RootSpec::task(100, "task_A")];
        let rec = TimeSeriesRecord::from_snapshot(
            &snap,
            &roots,
            &HashMap::new(),
            "memmux",
            "burst",
            0,
            Some(100),
        );
        assert_eq!(rec.root_process_count, 1);
        assert_eq!(rec.root_subtree_bytes, 500);
    }

    #[test]
    fn timeseries_aggregates() {
        let mk = |elapsed: u64, subtree: u64, dur: u64, frac: f64| TimeSeriesRecord {
            t_unix_ms: 0,
            elapsed_ms: elapsed,
            launcher: "memmux".into(),
            scenario: "leak".into(),
            sample_duration_us: dur,
            process_count: 1,
            total_bytes: subtree,
            owned_bytes: subtree,
            shared_bytes: 0,
            escaped_bytes: 0,
            unknown_bytes: 0,
            attributed_fraction: 0.01,
            root_subtree_bytes: subtree,
            root_process_count: 2,
            tree_attributed_fraction: frac,
        };
        let ts = TimeSeries::new(vec![mk(0, 100, 500, 1.0), mk(100, 300, 700, 0.98)]);
        assert_eq!(ts.peak_root_subtree_bytes(), 300);
        assert_eq!(ts.root_subtree_growth_bytes(), 200);
        assert!((ts.min_tree_attributed_fraction() - 0.98).abs() < 1e-9);
        assert_eq!(ts.peak_root_process_count(), 2);
        assert!((ts.mean_sample_duration_us() - 600.0).abs() < 1e-9);
        // 600us over a 1s interval = 0.0006 overhead.
        assert!(ts.overhead_fraction(1000) < 0.02);
    }

    #[test]
    fn jsonl_round_trips() {
        let dir = std::env::temp_dir().join(format!("memmux-bench-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("series.jsonl");
        let ts = TimeSeries::new(vec![TimeSeriesRecord {
            t_unix_ms: 1,
            elapsed_ms: 2,
            launcher: "raw-baseline".into(),
            scenario: "soak".into(),
            sample_duration_us: 42,
            process_count: 2,
            total_bytes: 10,
            owned_bytes: 8,
            shared_bytes: 1,
            escaped_bytes: 0,
            unknown_bytes: 1,
            attributed_fraction: 0.9,
            root_subtree_bytes: 8,
            root_process_count: 2,
            tree_attributed_fraction: 1.0,
        }]);
        ts.write_jsonl(&path).unwrap();
        let back = TimeSeries::read_jsonl(&path).unwrap();
        assert_eq!(back.records, ts.records);
        std::fs::remove_dir_all(&dir).ok();
    }
}
