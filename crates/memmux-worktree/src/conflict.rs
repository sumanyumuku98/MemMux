//! Overlapping changed-file detection and conflict-risk scoring (SUM-61 / §9.4).
//!
//! MemMux warns when two active writers touch the same files; it never auto-merges conflicting
//! semantic changes. The [`conflict_risk`] score feeds the scheduler's `conflict_risk` term.

use memmux_core::ids::TaskId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The set of files a task has changed in its worktree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFiles {
    /// Owning task.
    pub task_id: TaskId,
    /// Repository-relative changed paths.
    pub files: BTreeSet<String>,
}

impl ChangedFiles {
    /// Construct from any iterator of paths.
    pub fn new(task_id: impl Into<TaskId>, files: impl IntoIterator<Item = String>) -> Self {
        Self {
            task_id: task_id.into(),
            files: files.into_iter().collect(),
        }
    }
}

/// A detected overlap between two tasks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overlap {
    /// First task (lexicographically smaller id).
    pub a: TaskId,
    /// Second task.
    pub b: TaskId,
    /// The overlapping files.
    pub files: Vec<String>,
}

/// All pairwise overlaps between active tasks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictReport {
    /// Pairwise overlaps, deterministically ordered.
    pub overlaps: Vec<Overlap>,
}

impl ConflictReport {
    /// Whether any two tasks touch a common file.
    pub fn has_conflicts(&self) -> bool {
        !self.overlaps.is_empty()
    }
}

/// Detect every pairwise file overlap across the given task manifests.
pub fn detect_conflicts(manifests: &[ChangedFiles]) -> ConflictReport {
    let mut overlaps = Vec::new();
    for i in 0..manifests.len() {
        for j in (i + 1)..manifests.len() {
            let (m, n) = (&manifests[i], &manifests[j]);
            let mut shared: Vec<String> = m.files.intersection(&n.files).cloned().collect();
            if !shared.is_empty() {
                shared.sort();
                // Order the pair by id for a deterministic report.
                let (a, b) = if m.task_id.as_str() <= n.task_id.as_str() {
                    (m.task_id.clone(), n.task_id.clone())
                } else {
                    (n.task_id.clone(), m.task_id.clone())
                };
                overlaps.push(Overlap {
                    a,
                    b,
                    files: shared,
                });
            }
        }
    }
    overlaps.sort_by(|x, y| (x.a.as_str(), x.b.as_str()).cmp(&(y.a.as_str(), y.b.as_str())));
    ConflictReport { overlaps }
}

/// Conflict risk for `task`: the fraction of its changed files also touched by any other task
/// (0.0 = isolated, 1.0 = every changed file is contended). Suitable for the scheduler's
/// `conflict_risk` term.
pub fn conflict_risk(task: &ChangedFiles, others: &[ChangedFiles]) -> f64 {
    if task.files.is_empty() {
        return 0.0;
    }
    let mut contended: BTreeSet<&String> = BTreeSet::new();
    for other in others {
        if other.task_id == task.task_id {
            continue;
        }
        for f in task.files.intersection(&other.files) {
            contended.insert(f);
        }
    }
    contended.len() as f64 / task.files.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cf(id: &str, files: &[&str]) -> ChangedFiles {
        ChangedFiles::new(id, files.iter().map(|s| s.to_string()))
    }

    #[test]
    fn detects_overlapping_files() {
        let manifests = vec![
            cf("task_a", &["src/auth.rs", "src/lib.rs"]),
            cf("task_b", &["src/lib.rs", "src/ui.rs"]),
            cf("task_c", &["docs/readme.md"]),
        ];
        let report = detect_conflicts(&manifests);
        assert!(report.has_conflicts());
        assert_eq!(report.overlaps.len(), 1);
        assert_eq!(report.overlaps[0].a, TaskId::new("task_a"));
        assert_eq!(report.overlaps[0].b, TaskId::new("task_b"));
        assert_eq!(report.overlaps[0].files, vec!["src/lib.rs".to_string()]);
    }

    #[test]
    fn no_overlap_is_no_conflict() {
        let manifests = vec![cf("task_a", &["a.rs"]), cf("task_b", &["b.rs"])];
        assert!(!detect_conflicts(&manifests).has_conflicts());
    }

    #[test]
    fn conflict_risk_is_contended_fraction() {
        let task = cf("task_a", &["a.rs", "b.rs", "c.rs", "d.rs"]);
        let others = vec![cf("task_b", &["b.rs"]), cf("task_c", &["c.rs", "z.rs"])];
        // 2 of task_a's 4 files are contended.
        assert!((conflict_risk(&task, &others) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn isolated_task_has_zero_risk() {
        let task = cf("task_a", &["only.rs"]);
        let others = vec![cf("task_b", &["other.rs"])];
        assert_eq!(conflict_risk(&task, &others), 0.0);
        assert_eq!(conflict_risk(&cf("task_a", &[]), &others), 0.0);
    }
}
