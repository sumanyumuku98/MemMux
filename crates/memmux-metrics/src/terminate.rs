//! Recursive process-tree termination (SUM-76 / §14.1).
//!
//! When a task ends, MemMux must reclaim the *entire* owned descendant tree, not just the
//! provider process (§4.2: ≥99.5% of owned descendants gone within 10s). This samples the tree
//! rooted at a pid, sends `SIGTERM` to every member, waits a grace period, then `SIGKILL`s any
//! survivors, and reports the cleanup fraction.

use crate::sample::ProcessSampler;
use crate::tree::ProcessTree;
use memmux_core::ids::Pid;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The outcome of a termination sweep.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminationReport {
    /// Pids that were targeted (root + descendants at the first sample).
    pub targeted: Vec<Pid>,
    /// Pids still alive after `SIGTERM` + grace + `SIGKILL`.
    pub survivors: Vec<Pid>,
    /// Whether `SIGKILL` had to be used on any survivor.
    pub used_sigkill: bool,
}

impl TerminationReport {
    /// Fraction of targeted processes that were successfully reaped (1.0 if none targeted).
    pub fn cleanup_fraction(&self) -> f64 {
        if self.targeted.is_empty() {
            return 1.0;
        }
        let cleaned = self.targeted.len().saturating_sub(self.survivors.len());
        cleaned as f64 / self.targeted.len() as f64
    }

    /// Whether every targeted process was reclaimed.
    pub fn fully_cleaned(&self) -> bool {
        self.survivors.is_empty()
    }
}

/// The set of pids to terminate for `root`: the root and every descendant, excluding pids ≤ 1
/// (never signal init/kernel) — the safety guard from §14.
pub fn termination_targets(tree: &ProcessTree, root: Pid) -> Vec<Pid> {
    let mut targets: Vec<Pid> = tree.descendants(root);
    targets.push(root);
    targets.retain(|&p| p > 1);
    targets.sort_unstable();
    targets.dedup();
    targets
}

/// Terminate the process subtree rooted at `root`, escalating from `SIGTERM` to `SIGKILL`.
#[cfg(unix)]
pub fn terminate_subtree(
    sampler: &dyn ProcessSampler,
    root: Pid,
    grace: Duration,
) -> std::io::Result<TerminationReport> {
    if root <= 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to terminate pid <= 1",
        ));
    }

    let tree = ProcessTree::from_samples(sampler.snapshot()?.samples);
    let targeted = termination_targets(&tree, root);

    // Phase 1: polite SIGTERM to every target (children before parents helps some shells).
    for &pid in targeted.iter().rev() {
        signal(pid, libc::SIGTERM);
    }

    // Wait for graceful exit.
    std::thread::sleep(grace);

    // Who is still alive?
    let alive = ProcessTree::from_samples(sampler.snapshot()?.samples);
    let mut survivors: Vec<Pid> = targeted
        .iter()
        .copied()
        .filter(|&p| alive.get(p).is_some())
        .collect();

    let mut used_sigkill = false;
    if !survivors.is_empty() {
        used_sigkill = true;
        for &pid in &survivors {
            signal(pid, libc::SIGKILL);
        }
        std::thread::sleep(Duration::from_millis(100));
        let after = ProcessTree::from_samples(sampler.snapshot()?.samples);
        survivors.retain(|&p| after.get(p).is_some());
    }

    Ok(TerminationReport {
        targeted,
        survivors,
        used_sigkill,
    })
}

/// Non-Unix stub: termination via Job Objects arrives with Windows support (§4.2 Portability).
#[cfg(not(unix))]
pub fn terminate_subtree(
    _sampler: &dyn ProcessSampler,
    _root: Pid,
    _grace: Duration,
) -> std::io::Result<TerminationReport> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "recursive termination is only implemented on Unix",
    ))
}

#[cfg(unix)]
fn signal(pid: Pid, sig: libc::c_int) {
    // SAFETY: `kill` is always safe to call; an invalid pid simply returns ESRCH, which we ignore.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::ProcessSample;

    fn s(pid: Pid, ppid: Pid) -> ProcessSample {
        ProcessSample {
            pid,
            ppid,
            name: format!("p{pid}"),
            rss_bytes: 0,
            pss_bytes: None,
            phys_footprint_bytes: None,
        }
    }

    #[test]
    fn targets_are_subtree_minus_init() {
        // 1(init) -> 100 -> {101, 102 -> 103}
        let tree = ProcessTree::from_samples(vec![
            s(1, 0),
            s(100, 1),
            s(101, 100),
            s(102, 100),
            s(103, 102),
        ]);
        let targets = termination_targets(&tree, 100);
        assert_eq!(targets, vec![100, 101, 102, 103]);
        assert!(!targets.contains(&1), "init must never be targeted");
    }

    #[test]
    fn cleanup_fraction_math() {
        let report = TerminationReport {
            targeted: vec![10, 11, 12, 13],
            survivors: vec![13],
            used_sigkill: true,
        };
        assert!((report.cleanup_fraction() - 0.75).abs() < 1e-9);
        assert!(!report.fully_cleaned());

        let clean = TerminationReport {
            targeted: vec![10, 11],
            survivors: vec![],
            used_sigkill: false,
        };
        assert_eq!(clean.cleanup_fraction(), 1.0);
        assert!(clean.fully_cleaned());
    }
}
