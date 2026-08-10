//! Cleanup / leak-on-teardown measurement (SUM-165 / hypothesis H2).
//!
//! H2 asks an honesty-critical question: when a launcher tears a session down *its own normal way*,
//! does the whole agent subtree it promised to own actually go away — or does it leak spawned
//! grandchildren? We answer it by, at the very end of a run and **while the agents are still
//! alive**:
//!
//! 1. capturing the *owned set* — every pid in each live `agent_root`'s subtree, with the bytes we
//!    accounted to it;
//! 2. tearing the session down with the launcher's own [`stop`](crate::launcher::LaunchedSession::stop)
//!    (no MemMux special-casing — each launcher does its normal per-launcher thing); and
//! 3. polling for a grace window, counting how many owned pids survive.
//!
//! The differentiator is honest: a launcher that only kills its root pids leaks the grandchildren
//! it spawned (survivors > 0), while a launcher that terminates the whole subtree reclaims it all.

use memmux_core::ids::Pid;
use memmux_metrics::ProcessTree;
use serde::{Deserialize, Serialize};

/// One owned process captured before teardown: its pid and the bytes we accounted to it.
///
/// Captured from a single pre-teardown snapshot so `leaked_bytes` after teardown is the sum of the
/// bytes we *had attributed* to the pids that survived (we cannot re-measure a dead pid).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedProc {
    /// Process id.
    pub pid: Pid,
    /// Accounted bytes for this pid at capture time.
    pub accounted_bytes: u64,
}

/// The cleanup outcome for one launcher × scenario trial (SUM-165 / H2).
///
/// `cleanup_fraction` is `(owned_procs - leaked_procs) / owned_procs`, i.e. the fraction of the
/// owned agent subtree that was gone within the grace window of the launcher's normal teardown.
/// It is `None` when `owned_procs == 0` (nothing owned to reclaim → the measurement is n/a, never
/// a fabricated 100%).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CleanupResult {
    /// Number of pids in the owned agent subtree captured before teardown.
    pub owned_procs: usize,
    /// Number of owned pids still present after the grace window (the leak).
    pub leaked_procs: usize,
    /// Sum of accounted bytes across the leaked (surviving) pids.
    pub leaked_bytes: u64,
    /// Fraction of the owned subtree reclaimed; `None` when nothing was owned.
    pub cleanup_fraction: Option<f64>,
}

impl CleanupResult {
    /// Build a result from the owned set and the set of pids that survived the grace window.
    ///
    /// `leaked_bytes` sums the *capture-time* accounted bytes of each owned pid whose pid is in
    /// `survivors` (see [`survivors_of`] for how survivors are determined from a live tree).
    pub fn from_owned_and_survivors(owned: &[OwnedProc], survivors: &[Pid]) -> Self {
        let owned_procs = owned.len();
        let leaked_procs = survivors.len();
        let leaked_bytes = owned
            .iter()
            .filter(|o| survivors.contains(&o.pid))
            .map(|o| o.accounted_bytes)
            .sum();
        let cleanup_fraction = cleanup_fraction(owned_procs, leaked_procs);
        Self {
            owned_procs,
            leaked_procs,
            leaked_bytes,
            cleanup_fraction,
        }
    }
}

/// Compute the reclaimed fraction `(owned - leaked) / owned`, or `None` when `owned == 0`.
///
/// `leaked` is clamped to `owned` so the fraction never goes negative even if a caller passed an
/// inconsistent survivor count.
pub fn cleanup_fraction(owned_procs: usize, leaked_procs: usize) -> Option<f64> {
    if owned_procs == 0 {
        return None;
    }
    let leaked = leaked_procs.min(owned_procs);
    Some((owned_procs - leaked) as f64 / owned_procs as f64)
}

/// Which of the `owned` pids are still present in a (post-teardown) process `tree`.
///
/// A pid is a survivor iff the tree still contains a sample for it. The Linux sampler already
/// skips zombies, and on macOS a dead pid simply is not returned, so "present in the tree" is the
/// right liveness signal. Pure and side-effect-free so the polling loop stays trivial to reason
/// about and unit-test.
pub fn survivors_of(owned: &[OwnedProc], tree: &ProcessTree) -> Vec<Pid> {
    owned
        .iter()
        .map(|o| o.pid)
        .filter(|pid| tree.get(*pid).is_some())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use memmux_metrics::ProcessSample;

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

    #[test]
    fn cleanup_fraction_math() {
        // owned=0 → None (nothing to reclaim, never a fabricated 100%).
        assert_eq!(cleanup_fraction(0, 0), None);
        // owned=10, leaked=0 → fully reclaimed.
        assert_eq!(cleanup_fraction(10, 0), Some(1.0));
        // owned=4, leaked=1 → 0.75.
        assert_eq!(cleanup_fraction(4, 1), Some(0.75));
        // Over-count is clamped, not negative.
        assert_eq!(cleanup_fraction(2, 5), Some(0.0));
    }

    #[test]
    fn survivors_are_pids_still_in_the_tree() {
        // Owned four pids; after teardown the tree still holds pids 2 and 4 (leaked), while 1 and 3
        // are gone (reclaimed).
        let owned = vec![
            OwnedProc {
                pid: 1,
                accounted_bytes: 100,
            },
            OwnedProc {
                pid: 2,
                accounted_bytes: 200,
            },
            OwnedProc {
                pid: 3,
                accounted_bytes: 300,
            },
            OwnedProc {
                pid: 4,
                accounted_bytes: 400,
            },
        ];
        let tree = ProcessTree::from_samples(vec![sample(2, 1, 20), sample(4, 1, 40)]);
        let mut survivors = survivors_of(&owned, &tree);
        survivors.sort_unstable();
        assert_eq!(survivors, vec![2, 4]);

        let result = CleanupResult::from_owned_and_survivors(&owned, &survivors);
        assert_eq!(result.owned_procs, 4);
        assert_eq!(result.leaked_procs, 2);
        // Leaked bytes are the capture-time accounted bytes of the survivors (200 + 400).
        assert_eq!(result.leaked_bytes, 600);
        assert_eq!(result.cleanup_fraction, Some(0.5));
    }

    #[test]
    fn no_survivors_is_full_reclaim() {
        let owned = vec![OwnedProc {
            pid: 7,
            accounted_bytes: 700,
        }];
        let empty = ProcessTree::default();
        let survivors = survivors_of(&owned, &empty);
        assert!(survivors.is_empty());
        let result = CleanupResult::from_owned_and_survivors(&owned, &survivors);
        assert_eq!(result.leaked_procs, 0);
        assert_eq!(result.leaked_bytes, 0);
        assert_eq!(result.cleanup_fraction, Some(1.0));
    }

    #[test]
    fn nothing_owned_is_not_measured() {
        let result = CleanupResult::from_owned_and_survivors(&[], &[]);
        assert_eq!(result.owned_procs, 0);
        assert_eq!(result.cleanup_fraction, None);
    }
}
