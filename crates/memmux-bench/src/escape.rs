//! Escaped-process visibility measurement (SUM-166 / hypothesis H3).
//!
//! H3 is a *capability* claim, not a memory claim: tmux/herdr/raw have **no concept** of a process
//! escaping its agent — a child that reparents to init just silently leaks. MemMux's daemon
//! *surfaces* it as a `process_escaped` event. We measure, per launcher:
//!
//! * `injected` — how many escapes the harness created (known by construction: one per agent);
//! * `injected_confirmed` — how many of those escapes we independently confirmed actually
//!   reparented (a pid we saw under an agent root during the run is now alive but sits outside
//!   *every* agent-root subtree, i.e. reparented to init); and
//! * `detected` — how many the launcher itself surfaced. Only MemMux has a detection mechanism, so
//!   for the baselines this is `None` (rendered *unsupported*, never a measured-looking `0`).
//!
//! The honest headline: MemMux's `detected` equals `injected_confirmed`; the baselines are
//! unsupported because they cannot detect an escape at all.

use memmux_core::ids::Pid;
use memmux_metrics::ProcessTree;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// The escaped-process visibility outcome for one launcher × scenario trial (SUM-166 / H3).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscapeResult {
    /// Escapes the harness injected this trial (one per agent, by construction).
    pub injected: usize,
    /// Injected escapes the harness independently confirmed actually reparented (alive, and no
    /// longer under any agent-root subtree).
    pub injected_confirmed: usize,
    /// Escapes the launcher itself surfaced (deduped by pid); `None` for launchers with no
    /// escape-detection mechanism (rendered *unsupported*, never a measured `0`).
    pub detected: Option<usize>,
}

/// Independently confirm which `candidate` pids actually reparented out of every agent-root
/// subtree in `tree` while still being alive (SUM-166 / H3).
///
/// A candidate is *confirmed escaped* iff it is present in `tree` (alive) **and** none of the
/// `agent_roots` is one of its ancestors (nor is it itself an agent root) — i.e. it has reparented
/// away from the task tree (to init/pid 1). Pure and side-effect-free so it is trivial to unit-test
/// without spawning real processes.
pub fn confirm_reparented(tree: &ProcessTree, candidates: &[Pid], agent_roots: &[Pid]) -> Vec<Pid> {
    let roots: HashSet<Pid> = agent_roots.iter().copied().collect();
    candidates
        .iter()
        .copied()
        .filter(|&pid| {
            // Must still be alive.
            if tree.get(pid).is_none() {
                return false;
            }
            // Must not be under any agent root any more.
            tree.nearest_ancestor_in(pid, &roots).is_none()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use memmux_metrics::ProcessSample;

    fn s(pid: Pid, ppid: Pid) -> ProcessSample {
        ProcessSample {
            pid,
            ppid,
            name: format!("p{pid}"),
            rss_bytes: 1,
            pss_bytes: None,
            phys_footprint_bytes: None,
            uss_bytes: None,
            swap_bytes: None,
            minflt: None,
            majflt: None,
        }
    }

    #[test]
    fn confirms_only_alive_and_reparented_candidates() {
        // Tree: agent root 100 has a live child 101 (still owned). Pid 200 was a grandchild that
        // reparented to init (ppid 1) and is alive. Pid 300 is dead (absent).
        let tree = ProcessTree::from_samples(vec![
            s(1, 0),     // init
            s(100, 1),   // agent root
            s(101, 100), // still-owned child
            s(200, 1),   // escaped (reparented to init), alive
        ]);
        let confirmed = confirm_reparented(&tree, &[101, 200, 300], &[100]);
        // 101 is still under the root → not escaped. 200 reparented → escaped. 300 is dead.
        assert_eq!(confirmed, vec![200]);
    }

    #[test]
    fn a_candidate_that_is_itself_a_root_is_not_escaped() {
        let tree = ProcessTree::from_samples(vec![s(1, 0), s(100, 1)]);
        assert!(confirm_reparented(&tree, &[100], &[100]).is_empty());
    }

    #[test]
    fn result_is_default_zero() {
        let r = EscapeResult::default();
        assert_eq!(r.injected, 0);
        assert_eq!(r.injected_confirmed, 0);
        assert_eq!(r.detected, None);
    }
}
