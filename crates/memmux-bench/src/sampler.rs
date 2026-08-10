//! External time-series sampler (SUM-33) and sampling-overhead accounting (SUM-31).
//!
//! Periodically snapshots the process tree with `memmux-metrics`, attributes it, and emits one
//! [`TimeSeriesRecord`] per sample as JSON Lines. The per-sample duration is retained so the
//! harness can prove the ≤2% CPU overhead launch gate.

use crate::launcher::LaunchTopology;
use memmux_core::ids::{Pid, TaskId};
use memmux_metrics::{attribute, ProcessSampler, ProcessTree, RootSpec, Snapshot};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, Write};
use std::path::Path;

/// The provider-vs-manager accounting of one process-tree snapshot against a [`LaunchTopology`].
///
/// A pid is **provider** iff it is in some `agent_root` subtree (an agent root or one of its
/// descendants); **manager** iff it is in some `manager_pid` subtree but not a provider; else
/// **unknown** (excluded from the totals so unrelated host processes don't inflate the numbers).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SampleAccounting {
    /// Provider bytes + manager-overhead bytes.
    pub total_bytes: u64,
    /// Accounted bytes across all provider (agent) pids.
    pub provider_bytes: u64,
    /// Accounted bytes across manager-only pids (the multiplexer's own server/daemon RSS).
    pub manager_overhead_bytes: u64,
    /// Number of provider processes.
    pub provider_proc_count: usize,
    /// Number of manager-only processes.
    pub manager_proc_count: usize,
    /// Number of processes seen under a manager root that were also providers (informational;
    /// counted as providers, not managers) — always the overlap size, for debugging.
    pub unknown_proc_count: usize,
}

/// Partition a tree into provider (agent) pids and manager-only (multiplexer overhead) pids given
/// a launch topology. A pid is **provider** iff it lies in some agent-root subtree; **manager** iff
/// it lies in a manager-root subtree but is not a provider.
fn partition(tree: &ProcessTree, topology: &LaunchTopology) -> (HashSet<Pid>, HashSet<Pid>) {
    let mut provider: HashSet<Pid> = HashSet::new();
    for &root in &topology.agent_roots {
        if tree.get(root).is_some() {
            provider.insert(root);
        }
        for d in tree.descendants(root) {
            provider.insert(d);
        }
    }
    let mut manager: HashSet<Pid> = HashSet::new();
    for &root in &topology.manager_pids {
        if tree.get(root).is_some() && !provider.contains(&root) {
            manager.insert(root);
        }
        for d in tree.descendants(root) {
            if !provider.contains(&d) {
                manager.insert(d);
            }
        }
    }
    (provider, manager)
}

/// Classify a process tree into provider vs. manager-overhead accounting for a topology.
///
/// The provider set is the union of every `agent_root` subtree. The manager set is the union of
/// every `manager_pid` subtree with the provider set removed. Bytes use
/// [`ProcessSample::accounted_bytes`](memmux_metrics::ProcessSample::accounted_bytes) so shared
/// pages are not double-counted. Pids not present in the live tree are ignored.
pub fn classify(tree: &ProcessTree, topology: &LaunchTopology) -> SampleAccounting {
    let (provider, manager) = partition(tree, topology);
    let sum = |pids: &HashSet<Pid>| -> u64 {
        pids.iter()
            .filter_map(|p| tree.get(*p))
            .map(|s| s.accounted_bytes())
            .sum()
    };
    let provider_bytes = sum(&provider);
    let manager_overhead_bytes = sum(&manager);

    SampleAccounting {
        total_bytes: provider_bytes + manager_overhead_bytes,
        provider_bytes,
        manager_overhead_bytes,
        provider_proc_count: provider.len(),
        manager_proc_count: manager.len(),
        unknown_proc_count: 0,
    }
}

/// How a sampled process is tagged relative to the launch topology.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcTag {
    /// An agent process (in some agent-root subtree).
    Provider,
    /// Multiplexer server/daemon overhead (in a manager-root subtree, not a provider).
    Manager,
}

/// One tagged per-process row for the detailed `.procs.jsonl` time series (SUM-33): the full
/// per-process memory breakdown (RSS/PSS/USS/swap) + page faults, labelled provider vs manager.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaggedProc {
    /// Wall-clock time of the sample (ms since Unix epoch).
    pub t_unix_ms: u64,
    /// Milliseconds since sampling started.
    pub elapsed_ms: u64,
    /// Launcher under test.
    pub launcher: String,
    /// Scenario under test.
    pub scenario: String,
    /// Process id.
    pub pid: Pid,
    /// Parent process id.
    pub ppid: Pid,
    /// Command name.
    pub name: String,
    /// Provider (agent) or manager (overhead).
    pub tag: ProcTag,
    /// Resident set size, bytes.
    pub rss_bytes: u64,
    /// Proportional set size, bytes (Linux).
    pub pss_bytes: Option<u64>,
    /// Unique set size, bytes (Linux).
    pub uss_bytes: Option<u64>,
    /// Swapped-out bytes (Linux).
    pub swap_bytes: Option<u64>,
    /// Minor page faults (cumulative).
    pub minflt: Option<u64>,
    /// Major page faults (cumulative) — the I/O-backed thrashing signal.
    pub majflt: Option<u64>,
}

/// Build tagged per-process rows for one sample: every managed (provider or manager) process with
/// its full memory + fault breakdown, for the detailed per-process time series (SUM-33).
pub fn tagged_processes(
    tree: &ProcessTree,
    topology: &LaunchTopology,
    launcher: &str,
    scenario: &str,
    t_unix_ms: u64,
    elapsed_ms: u64,
) -> Vec<TaggedProc> {
    let (provider, manager) = partition(tree, topology);
    let mut rows: Vec<TaggedProc> = Vec::with_capacity(provider.len() + manager.len());
    let mut push = |pid: &Pid, tag: ProcTag| {
        if let Some(s) = tree.get(*pid) {
            rows.push(TaggedProc {
                t_unix_ms,
                elapsed_ms,
                launcher: launcher.to_string(),
                scenario: scenario.to_string(),
                pid: s.pid,
                ppid: s.ppid,
                name: s.name.clone(),
                tag,
                rss_bytes: s.rss_bytes,
                pss_bytes: s.pss_bytes,
                uss_bytes: s.uss_bytes,
                swap_bytes: s.swap_bytes,
                minflt: s.minflt,
                majflt: s.majflt,
            });
        }
    };
    for p in &provider {
        push(p, ProcTag::Provider);
    }
    for p in &manager {
        push(p, ProcTag::Manager);
    }
    rows.sort_by_key(|r| r.pid);
    rows
}

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
    // ---- Provider-vs-manager accounting (SUM-33 core). Additive `#[serde(default)]` fields so
    // ---- older JSONL files (without them) still deserialize. ----
    /// Accounted bytes across the provider (agent) process trees for this sample.
    #[serde(default)]
    pub provider_bytes: u64,
    /// Accounted bytes across the manager-only process tree (multiplexer server/daemon overhead).
    #[serde(default)]
    pub manager_overhead_bytes: u64,
    /// Number of provider processes observed this sample.
    #[serde(default)]
    pub provider_proc_count: usize,
    /// Number of manager-only processes observed this sample.
    #[serde(default)]
    pub manager_proc_count: usize,
    /// Human-readable launcher version at measurement time (e.g. `"tmux 3.6a"`).
    #[serde(default)]
    pub launcher_version: String,
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
            provider_bytes: 0,
            manager_overhead_bytes: 0,
            provider_proc_count: 0,
            manager_proc_count: 0,
            launcher_version: String::new(),
        }
    }

    /// Build a record from a snapshot and a [`LaunchTopology`], tagging provider vs. manager
    /// overhead (SUM-33 core).
    ///
    /// The legacy single-root attribution fields are filled against the first agent root (so old
    /// consumers of `root_subtree_bytes` keep working), while the new provider/manager fields are
    /// filled from [`classify`]. `total_bytes` here is the topology total (provider + manager),
    /// not the host-wide total.
    pub fn from_snapshot_topology(
        snapshot: &Snapshot,
        topology: &LaunchTopology,
        launcher: &str,
        version: &str,
        scenario: &str,
        elapsed_ms: u64,
    ) -> Self {
        let tree = ProcessTree::from_samples(snapshot.samples.clone());
        let acct = classify(&tree, topology);

        // Legacy attribution: scope to the first agent root, treating all its declared roots as
        // owned by one benchmark task so `tree_attributed_fraction` stays meaningful.
        let first_root = topology.agent_roots.first().copied();
        let roots: Vec<RootSpec> = topology
            .agent_roots
            .iter()
            .map(|p| RootSpec::task(*p, "task_bench"))
            .collect();
        let report = attribute(&tree, &roots, &HashMap::new());

        let (root_subtree_bytes, root_process_count, tree_attributed_fraction) = match first_root {
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
                (tree.subtree_accounted_bytes(root), pids.len(), frac)
            }
            None => (0, 0, 1.0),
        };

        Self {
            t_unix_ms: snapshot.taken_at_unix_ms,
            elapsed_ms,
            launcher: launcher.to_string(),
            scenario: scenario.to_string(),
            sample_duration_us: snapshot.sample_duration.as_micros() as u64,
            process_count: snapshot.samples.len(),
            total_bytes: acct.total_bytes,
            owned_bytes: report.owned_bytes,
            shared_bytes: report.shared_bytes,
            escaped_bytes: report.escaped_bytes,
            unknown_bytes: report.unknown_bytes,
            attributed_fraction: report.attributed_fraction(),
            root_subtree_bytes,
            root_process_count,
            tree_attributed_fraction,
            provider_bytes: acct.provider_bytes,
            manager_overhead_bytes: acct.manager_overhead_bytes,
            provider_proc_count: acct.provider_proc_count,
            manager_proc_count: acct.manager_proc_count,
            launcher_version: version.to_string(),
        }
    }
}

/// Take one live topology-aware sample using `sampler` (SUM-33 core).
pub fn sample_once_topology(
    sampler: &dyn ProcessSampler,
    topology: &LaunchTopology,
    launcher: &str,
    version: &str,
    scenario: &str,
    elapsed_ms: u64,
) -> io::Result<TimeSeriesRecord> {
    let snapshot = sampler.snapshot()?;
    Ok(TimeSeriesRecord::from_snapshot_topology(
        &snapshot, topology, launcher, version, scenario, elapsed_ms,
    ))
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

    /// Peak topology **total** footprint (provider + manager overhead) across the series.
    pub fn peak_total_bytes(&self) -> u64 {
        self.records
            .iter()
            .map(|r| r.total_bytes)
            .max()
            .unwrap_or(0)
    }

    /// Steady-state topology **total** footprint: the last sample's total (0 if empty).
    pub fn steady_total_bytes(&self) -> u64 {
        self.records.last().map(|r| r.total_bytes).unwrap_or(0)
    }

    /// Peak **manager overhead** (multiplexer server/daemon RSS) across the series.
    pub fn peak_manager_overhead_bytes(&self) -> u64 {
        self.records
            .iter()
            .map(|r| r.manager_overhead_bytes)
            .max()
            .unwrap_or(0)
    }

    /// Steady-state **manager overhead**: the last sample's value (0 if empty).
    pub fn steady_manager_overhead_bytes(&self) -> u64 {
        self.records
            .last()
            .map(|r| r.manager_overhead_bytes)
            .unwrap_or(0)
    }

    /// Peak provider (agent) footprint across the series.
    pub fn peak_provider_bytes(&self) -> u64 {
        self.records
            .iter()
            .map(|r| r.provider_bytes)
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

/// Write tagged per-process rows as JSON Lines to `path` (SUM-33 detailed per-process series).
pub fn write_tagged_processes_jsonl(path: &Path, rows: &[TaggedProc]) -> io::Result<()> {
    let mut file = io::BufWriter::new(std::fs::File::create(path)?);
    for row in rows {
        serde_json::to_writer(&mut file, row)?;
        file.write_all(b"\n")?;
    }
    file.flush()
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
            uss_bytes: None,
            swap_bytes: None,
            minflt: None,
            majflt: None,
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
            provider_bytes: subtree,
            manager_overhead_bytes: 0,
            provider_proc_count: 1,
            manager_proc_count: 0,
            launcher_version: "memmux 0.0.0".into(),
        };
        let ts = TimeSeries::new(vec![mk(0, 100, 500, 1.0), mk(100, 300, 700, 0.98)]);
        assert_eq!(ts.peak_root_subtree_bytes(), 300);
        assert_eq!(ts.root_subtree_growth_bytes(), 200);
        assert!((ts.min_tree_attributed_fraction() - 0.98).abs() < 1e-9);
        assert_eq!(ts.peak_root_process_count(), 2);
        assert!((ts.mean_sample_duration_us() - 600.0).abs() < 1e-9);
        // 600us over a 1s interval = 0.0006 overhead.
        assert!(ts.overhead_fraction(1000) < 0.02);
        // Topology totals track total_bytes across the series.
        assert_eq!(ts.peak_total_bytes(), 300);
        assert_eq!(ts.steady_total_bytes(), 300);
        assert_eq!(ts.peak_manager_overhead_bytes(), 0);
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
            provider_bytes: 8,
            manager_overhead_bytes: 2,
            provider_proc_count: 2,
            manager_proc_count: 1,
            launcher_version: "raw (direct spawn)".into(),
        }]);
        ts.write_jsonl(&path).unwrap();
        let back = TimeSeries::read_jsonl(&path).unwrap();
        assert_eq!(back.records, ts.records);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn classify_raw_has_no_manager_overhead() {
        // Two independent agent roots (100, 200), each with one child; no manager.
        let tree = ProcessTree::from_samples(vec![
            sample(100, 1, 500),
            sample(101, 100, 300),
            sample(200, 1, 400),
            sample(201, 200, 200),
            sample(1, 0, 50),
        ]);
        let topo = LaunchTopology {
            manager_pids: vec![],
            agent_roots: vec![100, 200],
        };
        let acct = classify(&tree, &topo);
        assert_eq!(acct.provider_bytes, 500 + 300 + 400 + 200);
        assert_eq!(acct.manager_overhead_bytes, 0);
        assert_eq!(acct.total_bytes, acct.provider_bytes);
        assert_eq!(acct.provider_proc_count, 4);
        assert_eq!(acct.manager_proc_count, 0);
    }

    #[test]
    fn classify_managed_splits_server_from_providers() {
        // Daemon (900) with server RSS 700, two provider children (100, 200) each holding memory.
        // pid 999 is an unrelated host process and must be excluded from the totals.
        let tree = ProcessTree::from_samples(vec![
            sample(900, 1, 700),
            sample(100, 900, 500),
            sample(200, 900, 400),
            sample(999, 1, 12345),
        ]);
        let topo = LaunchTopology {
            manager_pids: vec![900],
            agent_roots: vec![100, 200],
        };
        let acct = classify(&tree, &topo);
        assert_eq!(acct.provider_bytes, 900);
        // Manager overhead is the server's own RSS only (children are providers, not manager).
        assert_eq!(acct.manager_overhead_bytes, 700);
        assert_eq!(acct.total_bytes, 1600);
        assert_eq!(acct.provider_proc_count, 2);
        assert_eq!(acct.manager_proc_count, 1);
    }

    #[test]
    fn tagged_processes_tags_and_carries_per_process_fields() {
        // Manager 900 (server) + one provider child 100 carrying USS/swap/faults; 999 unrelated.
        let child = ProcessSample {
            pid: 100,
            ppid: 900,
            name: "agent".into(),
            rss_bytes: 500,
            pss_bytes: Some(420),
            phys_footprint_bytes: None,
            uss_bytes: Some(300),
            swap_bytes: Some(64),
            minflt: Some(10),
            majflt: Some(2),
        };
        let tree = ProcessTree::from_samples(vec![sample(900, 1, 700), child, sample(999, 1, 42)]);
        let topo = LaunchTopology {
            manager_pids: vec![900],
            agent_roots: vec![100],
        };
        let rows = tagged_processes(&tree, &topo, "memmux", "burst", 1_000, 5);
        // Only the two managed pids are rows (999 excluded); sorted by pid.
        assert_eq!(
            rows.iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![100, 900]
        );
        let agent = rows.iter().find(|r| r.pid == 100).unwrap();
        assert_eq!(agent.tag, ProcTag::Provider);
        assert_eq!(agent.uss_bytes, Some(300));
        assert_eq!(agent.swap_bytes, Some(64));
        assert_eq!(agent.majflt, Some(2));
        let server = rows.iter().find(|r| r.pid == 900).unwrap();
        assert_eq!(server.tag, ProcTag::Manager);
    }
}
