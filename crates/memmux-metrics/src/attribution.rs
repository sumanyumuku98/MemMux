//! Task/shared-service attribution and escaped-process reconciliation (§14.2).
//!
//! Given the observed [`ProcessTree`] plus the roots MemMux launched and the descendants its
//! launch wrappers reported, every process is classified as:
//!
//! * [`Attribution::Owned`] — inside a task's process subtree.
//! * [`Attribution::Shared`] — inside a declared shared-service subtree.
//! * [`Attribution::Escaped`] — a process a wrapper reported for a task, but which is no longer
//!   inside that task's subtree (it re-parented / detached).
//! * [`Attribution::Unknown`] — not under any known root and never reported.

use crate::tree::ProcessTree;
use memmux_core::ids::{Pid, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Identifier for a shared service (repository index, MCP gateway pool, browser, …).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServiceId(pub String);

impl ServiceId {
    /// Create a shared-service id.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// The owner a root process belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Owner {
    /// The root belongs to a logical task.
    Task(TaskId),
    /// The root belongs to a shared service.
    Shared(ServiceId),
}

/// A process MemMux launched and knows the owner of (a task provider root, or a service root).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootSpec {
    /// The root process id.
    pub pid: Pid,
    /// Who the root (and, by default, its subtree) belongs to.
    pub owner: Owner,
}

impl RootSpec {
    /// Convenience constructor for a task root.
    pub fn task(pid: Pid, task: impl Into<TaskId>) -> Self {
        Self {
            pid,
            owner: Owner::Task(task.into()),
        }
    }

    /// Convenience constructor for a shared-service root.
    pub fn shared(pid: Pid, service: impl Into<String>) -> Self {
        Self {
            pid,
            owner: Owner::Shared(ServiceId::new(service)),
        }
    }
}

/// Classification of a single process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Attribution {
    /// Attributed to a task via subtree membership.
    Owned {
        /// Owning task.
        task: TaskId,
    },
    /// Attributed to a shared service via subtree membership.
    Shared {
        /// Owning service.
        service: ServiceId,
    },
    /// Reported for a task but found outside that task's subtree.
    Escaped {
        /// Task the wrapper claimed this process for.
        task: TaskId,
    },
    /// Unowned and unreported.
    Unknown,
}

impl Attribution {
    /// Whether this classification maps the process to a task or shared service.
    ///
    /// Escaped processes count as attributed — they are mapped to a task, just misplaced.
    pub fn is_attributed(&self) -> bool {
        !matches!(self, Attribution::Unknown)
    }
}

/// Aggregated attribution result for a whole snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributionReport {
    /// Per-process classification.
    pub by_pid: HashMap<Pid, Attribution>,
    /// Accounted bytes classified as owned.
    pub owned_bytes: u64,
    /// Accounted bytes classified as shared.
    pub shared_bytes: u64,
    /// Accounted bytes classified as escaped.
    pub escaped_bytes: u64,
    /// Accounted bytes classified as unknown.
    pub unknown_bytes: u64,
}

impl AttributionReport {
    /// Total accounted bytes across all classes.
    pub fn total_bytes(&self) -> u64 {
        self.owned_bytes + self.shared_bytes + self.escaped_bytes + self.unknown_bytes
    }

    /// Fraction of accounted bytes mapped to a task or shared service (§18.5 gate: ≥ 0.95).
    ///
    /// Returns `1.0` for an empty snapshot (nothing to misattribute).
    pub fn attributed_fraction(&self) -> f64 {
        let total = self.total_bytes();
        if total == 0 {
            return 1.0;
        }
        let attributed = self.owned_bytes + self.shared_bytes + self.escaped_bytes;
        attributed as f64 / total as f64
    }

    /// Pids classified as escaped (surfaced to the operator per §14.1 "escaped-process alerts").
    pub fn escaped_pids(&self) -> Vec<Pid> {
        self.pids_matching(|a| matches!(a, Attribution::Escaped { .. }))
    }

    /// Pids classified as unknown.
    pub fn unknown_pids(&self) -> Vec<Pid> {
        self.pids_matching(|a| matches!(a, Attribution::Unknown))
    }

    fn pids_matching(&self, pred: impl Fn(&Attribution) -> bool) -> Vec<Pid> {
        let mut v: Vec<Pid> = self
            .by_pid
            .iter()
            .filter(|(_, a)| pred(a))
            .map(|(p, _)| *p)
            .collect();
        v.sort_unstable();
        v
    }
}

/// Classify every process in `tree` against the given roots and wrapper-reported ownership.
///
/// * `roots` — process roots MemMux launched, each tagged with its [`Owner`].
/// * `expected` — pids a launch wrapper reported as belonging to a task (used to detect
///   escapes: a reported pid found outside its task's subtree is [`Attribution::Escaped`]).
pub fn attribute(
    tree: &ProcessTree,
    roots: &[RootSpec],
    expected: &HashMap<Pid, TaskId>,
) -> AttributionReport {
    let root_owner: HashMap<Pid, Owner> = roots.iter().map(|r| (r.pid, r.owner.clone())).collect();
    let root_pids: HashSet<Pid> = root_owner.keys().copied().collect();

    let mut report = AttributionReport::default();

    for sample in tree.iter() {
        let pid = sample.pid;
        let bytes = sample.accounted_bytes();

        let attribution = match tree.nearest_ancestor_in(pid, &root_pids) {
            Some(root) => match &root_owner[&root] {
                Owner::Task(task) => Attribution::Owned { task: task.clone() },
                Owner::Shared(service) => Attribution::Shared {
                    service: service.clone(),
                },
            },
            None => match expected.get(&pid) {
                Some(task) => Attribution::Escaped { task: task.clone() },
                None => Attribution::Unknown,
            },
        };

        match &attribution {
            Attribution::Owned { .. } => report.owned_bytes += bytes,
            Attribution::Shared { .. } => report.shared_bytes += bytes,
            Attribution::Escaped { .. } => report.escaped_bytes += bytes,
            Attribution::Unknown => report.unknown_bytes += bytes,
        }
        report.by_pid.insert(pid, attribution);
    }

    report
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

    /// Tree:
    ///   100 (task-A provider) ─┬─ 101 tsc
    ///                          └─ 102 pytest
    ///   200 (shared repo-index)
    ///   300 (escaped: wrapper said task-A, now under init pid 1)
    ///   1   (init) ─── 300
    fn scenario() -> ProcessTree {
        ProcessTree::from_samples(vec![
            s(1, 0, 5),
            s(100, 1, 800),
            s(101, 100, 400),
            s(102, 100, 600),
            s(200, 1, 300),
            s(300, 1, 250),
        ])
    }

    #[test]
    fn owned_shared_escaped_unknown_are_classified() {
        let tree = scenario();
        let roots = vec![
            RootSpec::task(100, "task_A"),
            RootSpec::shared(200, "repo-index"),
        ];
        let mut expected = HashMap::new();
        expected.insert(300, TaskId::new("task_A"));

        let report = attribute(&tree, &roots, &expected);

        assert_eq!(
            report.by_pid[&100],
            Attribution::Owned {
                task: TaskId::new("task_A")
            }
        );
        assert_eq!(
            report.by_pid[&102],
            Attribution::Owned {
                task: TaskId::new("task_A")
            }
        );
        assert_eq!(
            report.by_pid[&200],
            Attribution::Shared {
                service: ServiceId::new("repo-index")
            }
        );
        assert_eq!(
            report.by_pid[&300],
            Attribution::Escaped {
                task: TaskId::new("task_A")
            }
        );
        assert_eq!(report.by_pid[&1], Attribution::Unknown);

        assert_eq!(report.owned_bytes, 800 + 400 + 600);
        assert_eq!(report.shared_bytes, 300);
        assert_eq!(report.escaped_bytes, 250);
        assert_eq!(report.unknown_bytes, 5);
        assert_eq!(report.escaped_pids(), vec![300]);
        assert_eq!(report.unknown_pids(), vec![1]);
    }

    #[test]
    fn attributed_fraction_matches_hand_calc() {
        let tree = scenario();
        let roots = vec![
            RootSpec::task(100, "task_A"),
            RootSpec::shared(200, "repo-index"),
        ];
        let mut expected = HashMap::new();
        expected.insert(300, TaskId::new("task_A"));
        let report = attribute(&tree, &roots, &expected);

        // total = 2355, unknown = 5 -> attributed = 2350
        let expected_fraction = 2350.0 / 2355.0;
        assert!((report.attributed_fraction() - expected_fraction).abs() < 1e-9);
        assert!(report.attributed_fraction() > 0.95);
    }

    #[test]
    fn empty_snapshot_is_fully_attributed() {
        let tree = ProcessTree::default();
        let report = attribute(&tree, &[], &HashMap::new());
        assert_eq!(report.attributed_fraction(), 1.0);
    }

    #[test]
    fn nested_task_root_under_shared_root_prefers_nearest() {
        // shared 200 ── task-provider 210 ── worker 211
        let tree = ProcessTree::from_samples(vec![s(200, 1, 10), s(210, 200, 20), s(211, 210, 30)]);
        let roots = vec![RootSpec::shared(200, "svc"), RootSpec::task(210, "task_X")];
        let report = attribute(&tree, &roots, &HashMap::new());
        assert_eq!(
            report.by_pid[&211],
            Attribution::Owned {
                task: TaskId::new("task_X")
            }
        );
        assert_eq!(
            report.by_pid[&200],
            Attribution::Shared {
                service: ServiceId::new("svc")
            }
        );
    }
}
