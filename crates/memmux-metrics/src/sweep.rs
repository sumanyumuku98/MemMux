//! Reconciliation sweep + unknown-process visibility (SUM-77 / §14.2).
//!
//! Periodically the daemon reconciles the observed process tree against the roots it launched
//! and the descendants its wrappers reported, then *surfaces* anything escaped or unknown — the
//! spec is emphatic that unknown processes are reported, never hidden.

use crate::attribution::{attribute, RootSpec};
use crate::sample::ProcessSampler;
use crate::tree::ProcessTree;
use memmux_core::ids::{Pid, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A process that could not be cleanly attributed, surfaced for operator visibility.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlaggedProcess {
    /// Process id.
    pub pid: Pid,
    /// Command name.
    pub name: String,
    /// Accounted bytes.
    pub bytes: u64,
}

/// The result of a reconciliation sweep.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReconciliationReport {
    /// Total processes observed.
    pub process_count: usize,
    /// Fraction of accounted bytes mapped to a task or shared service (§18.5 attribution gate).
    pub attributed_fraction: f64,
    /// Processes reported for a task but found outside its subtree (escaped).
    pub escaped: Vec<FlaggedProcess>,
    /// Processes neither owned, shared, nor reported.
    pub unknown: Vec<FlaggedProcess>,
}

impl ReconciliationReport {
    /// Whether the sweep found anything needing operator attention.
    pub fn needs_attention(&self) -> bool {
        !self.escaped.is_empty() || !self.unknown.is_empty()
    }
}

/// Run a reconciliation sweep: sample, attribute, and surface escaped/unknown processes.
pub fn reconcile(
    sampler: &dyn ProcessSampler,
    roots: &[RootSpec],
    expected: &HashMap<Pid, TaskId>,
) -> std::io::Result<ReconciliationReport> {
    let tree = ProcessTree::from_samples(sampler.snapshot()?.samples);
    Ok(reconcile_tree(&tree, roots, expected))
}

/// The pure core of [`reconcile`], operating on an already-sampled tree (unit-testable).
pub fn reconcile_tree(
    tree: &ProcessTree,
    roots: &[RootSpec],
    expected: &HashMap<Pid, TaskId>,
) -> ReconciliationReport {
    let report = attribute(tree, roots, expected);
    let flag = |pid: Pid| {
        let sample = tree.get(pid);
        FlaggedProcess {
            pid,
            name: sample.map(|s| s.name.clone()).unwrap_or_default(),
            bytes: sample.map(|s| s.accounted_bytes()).unwrap_or(0),
        }
    };
    ReconciliationReport {
        process_count: tree.len(),
        attributed_fraction: report.attributed_fraction(),
        escaped: report.escaped_pids().into_iter().map(flag).collect(),
        unknown: report.unknown_pids().into_iter().map(flag).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::ProcessSample;

    fn s(pid: Pid, ppid: Pid, rss: u64) -> ProcessSample {
        ProcessSample {
            pid,
            ppid,
            name: format!("p{pid}"),
            rss_bytes: rss,
            pss_bytes: None,
            phys_footprint_bytes: None,
        }
    }

    #[test]
    fn sweep_surfaces_escaped_and_unknown() {
        // 1(init) unknown; 100 owned by task_A with child 101; 300 escaped (reported for task_A).
        let tree = ProcessTree::from_samples(vec![
            s(1, 0, 10),
            s(100, 1, 500),
            s(101, 100, 200),
            s(300, 1, 250),
        ]);
        let roots = vec![RootSpec::task(100, "task_A")];
        let mut expected = HashMap::new();
        expected.insert(300, TaskId::new("task_A"));

        let report = reconcile_tree(&tree, &roots, &expected);
        assert!(report.needs_attention());
        assert_eq!(
            report.escaped.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![300]
        );
        assert_eq!(
            report.unknown.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(report.escaped[0].bytes, 250);
    }

    #[test]
    fn fully_owned_tree_needs_no_attention() {
        let tree = ProcessTree::from_samples(vec![s(100, 0, 500), s(101, 100, 200)]);
        let roots = vec![RootSpec::task(100, "task_A")];
        let report = reconcile_tree(&tree, &roots, &HashMap::new());
        assert!(!report.needs_attention());
        assert_eq!(report.attributed_fraction, 1.0);
    }
}
