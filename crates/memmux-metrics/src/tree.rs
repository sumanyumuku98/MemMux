//! Parent/child process tree reconstructed from a [`Snapshot`](crate::Snapshot).

use crate::sample::ProcessSample;
use memmux_core::ids::Pid;
use std::collections::{HashMap, HashSet, VecDeque};

/// An indexed process tree supporting ancestry and subtree queries.
#[derive(Clone, Debug, Default)]
pub struct ProcessTree {
    by_pid: HashMap<Pid, ProcessSample>,
    children: HashMap<Pid, Vec<Pid>>,
}

impl ProcessTree {
    /// Build a tree from a flat list of samples.
    ///
    /// Duplicate pids keep the last sample seen. Children lists are sorted for deterministic
    /// iteration.
    pub fn from_samples(samples: Vec<ProcessSample>) -> Self {
        let mut by_pid = HashMap::with_capacity(samples.len());
        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for s in samples {
            children.entry(s.ppid).or_default().push(s.pid);
            by_pid.insert(s.pid, s);
        }
        for kids in children.values_mut() {
            kids.sort_unstable();
            kids.dedup();
        }
        Self { by_pid, children }
    }

    /// Number of processes in the tree.
    pub fn len(&self) -> usize {
        self.by_pid.len()
    }

    /// Whether the tree contains no processes.
    pub fn is_empty(&self) -> bool {
        self.by_pid.is_empty()
    }

    /// Look up a process by pid.
    pub fn get(&self, pid: Pid) -> Option<&ProcessSample> {
        self.by_pid.get(&pid)
    }

    /// Iterate over every sampled process.
    pub fn iter(&self) -> impl Iterator<Item = &ProcessSample> {
        self.by_pid.values()
    }

    /// All pids present in the tree.
    pub fn pids(&self) -> impl Iterator<Item = Pid> + '_ {
        self.by_pid.keys().copied()
    }

    /// Direct children of `pid`.
    pub fn children(&self, pid: Pid) -> &[Pid] {
        self.children.get(&pid).map(Vec::as_slice).unwrap_or(&[])
    }

    /// All transitive descendants of `pid` (excluding `pid` itself), in breadth-first order.
    ///
    /// Robust against cycles: each pid is visited at most once.
    pub fn descendants(&self, pid: Pid) -> Vec<Pid> {
        let mut out = Vec::new();
        let mut seen: HashSet<Pid> = HashSet::new();
        seen.insert(pid);
        let mut queue: VecDeque<Pid> = self.children(pid).iter().copied().collect();
        while let Some(next) = queue.pop_front() {
            if !seen.insert(next) {
                continue;
            }
            out.push(next);
            queue.extend(self.children(next).iter().copied());
        }
        out
    }

    /// Ancestors of `pid` from nearest parent to the root, following `ppid` links.
    ///
    /// Stops on a missing parent or a cycle.
    pub fn ancestors(&self, pid: Pid) -> Vec<Pid> {
        let mut out = Vec::new();
        let mut seen: HashSet<Pid> = HashSet::new();
        seen.insert(pid);
        let mut cursor = pid;
        while let Some(sample) = self.by_pid.get(&cursor) {
            let parent = sample.ppid;
            if parent == cursor || !seen.insert(parent) {
                break;
            }
            // Only report ancestors we actually sampled.
            if self.by_pid.contains_key(&parent) {
                out.push(parent);
            } else {
                break;
            }
            cursor = parent;
        }
        out
    }

    /// Sum of RSS across `pid` and all its descendants.
    pub fn subtree_rss_bytes(&self, pid: Pid) -> u64 {
        self.subtree_sum(pid, |s| s.rss_bytes)
    }

    /// Sum of the accounted (proportional-where-possible) memory across `pid` and descendants.
    pub fn subtree_accounted_bytes(&self, pid: Pid) -> u64 {
        self.subtree_sum(pid, ProcessSample::accounted_bytes)
    }

    fn subtree_sum(&self, pid: Pid, f: impl Fn(&ProcessSample) -> u64) -> u64 {
        let mut total = self.by_pid.get(&pid).map(&f).unwrap_or(0);
        for d in self.descendants(pid) {
            if let Some(s) = self.by_pid.get(&d) {
                total += f(s);
            }
        }
        total
    }

    /// The nearest ancestor of `pid` (including `pid` itself) that is present in `roots`.
    pub fn nearest_ancestor_in(&self, pid: Pid, roots: &HashSet<Pid>) -> Option<Pid> {
        if roots.contains(&pid) {
            return Some(pid);
        }
        self.ancestors(pid).into_iter().find(|p| roots.contains(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Tree:  1 ─┬─ 2 ─── 4
    ///           └─ 3
    fn sample_tree() -> ProcessTree {
        ProcessTree::from_samples(vec![s(1, 0, 100), s(2, 1, 200), s(3, 1, 300), s(4, 2, 400)])
    }

    #[test]
    fn children_and_descendants() {
        let t = sample_tree();
        assert_eq!(t.children(1), &[2, 3]);
        let mut d = t.descendants(1);
        d.sort_unstable();
        assert_eq!(d, vec![2, 3, 4]);
        assert_eq!(t.descendants(4), Vec::<Pid>::new());
    }

    #[test]
    fn ancestors_walk_to_sampled_root() {
        let t = sample_tree();
        assert_eq!(t.ancestors(4), vec![2, 1]);
        assert_eq!(t.ancestors(1), Vec::<Pid>::new());
    }

    #[test]
    fn subtree_rss_sums_self_and_descendants() {
        let t = sample_tree();
        assert_eq!(t.subtree_rss_bytes(1), 1000);
        assert_eq!(t.subtree_rss_bytes(2), 600);
        assert_eq!(t.subtree_rss_bytes(4), 400);
    }

    #[test]
    fn nearest_ancestor_in_prefers_closest_root() {
        let t = sample_tree();
        let roots: HashSet<Pid> = [1, 2].into_iter().collect();
        assert_eq!(t.nearest_ancestor_in(4, &roots), Some(2));
        assert_eq!(t.nearest_ancestor_in(3, &roots), Some(1));
        assert_eq!(t.nearest_ancestor_in(2, &roots), Some(2));
    }

    #[test]
    fn cycles_do_not_loop_forever() {
        // Pathological input: 5 <-> 6 point at each other.
        let t = ProcessTree::from_samples(vec![s(5, 6, 10), s(6, 5, 10)]);
        assert!(t.descendants(5).len() <= 2);
        assert!(t.ancestors(5).len() <= 2);
    }
}
