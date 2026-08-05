//! Per-process samples and the [`ProcessSampler`] trait.

use memmux_core::ids::Pid;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// A single point-in-time measurement of one process.
///
/// Memory is reported as resident set size plus, where the platform supports it, a
/// proportional metric that avoids double-counting shared pages:
/// * `pss_bytes` — Linux Proportional Set Size (from `smaps_rollup`).
/// * `phys_footprint_bytes` — macOS `ri_phys_footprint` (from `proc_pid_rusage`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessSample {
    /// Process id.
    pub pid: Pid,
    /// Parent process id.
    pub ppid: Pid,
    /// Executable / command name.
    pub name: String,
    /// Resident set size, in bytes.
    pub rss_bytes: u64,
    /// Proportional set size (Linux), in bytes.
    pub pss_bytes: Option<u64>,
    /// Physical footprint (macOS), in bytes.
    pub phys_footprint_bytes: Option<u64>,
}

impl ProcessSample {
    /// The best available per-process memory figure for accounting.
    ///
    /// Prefers a proportional metric (PSS, then `phys_footprint`) and falls back to RSS. This
    /// is the value the attribution engine sums, so shared pages are not counted N times.
    pub fn accounted_bytes(&self) -> u64 {
        self.pss_bytes
            .or(self.phys_footprint_bytes)
            .unwrap_or(self.rss_bytes)
    }
}

/// A complete snapshot of the process tree at one instant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Wall-clock time the snapshot was taken (milliseconds since the Unix epoch).
    pub taken_at_unix_ms: u64,
    /// How long collecting the snapshot took (used to police the ≤2% CPU overhead gate).
    pub sample_duration: Duration,
    /// All sampled processes.
    pub samples: Vec<ProcessSample>,
}

impl Snapshot {
    /// Total accounted bytes across every sampled process.
    pub fn total_accounted_bytes(&self) -> u64 {
        self.samples
            .iter()
            .map(ProcessSample::accounted_bytes)
            .sum()
    }
}

/// Samples the operating-system process tree.
///
/// Implementations are platform-specific; obtain the right one with [`default_sampler`].
///
/// [`default_sampler`]: crate::default_sampler
pub trait ProcessSampler: Send + Sync {
    /// Capture the full process tree.
    fn snapshot(&self) -> std::io::Result<Snapshot>;

    /// A short label identifying the sampler backend (e.g. `"linux-proc"`).
    fn platform(&self) -> &'static str;
}

/// Current wall-clock time in milliseconds since the Unix epoch (saturating on error).
pub(crate) fn now_unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(rss: u64, pss: Option<u64>, phys: Option<u64>) -> ProcessSample {
        ProcessSample {
            pid: 1,
            ppid: 0,
            name: "x".into(),
            rss_bytes: rss,
            pss_bytes: pss,
            phys_footprint_bytes: phys,
        }
    }

    #[test]
    fn accounted_bytes_prefers_pss_then_phys_then_rss() {
        assert_eq!(sample(100, Some(40), Some(60)).accounted_bytes(), 40);
        assert_eq!(sample(100, None, Some(60)).accounted_bytes(), 60);
        assert_eq!(sample(100, None, None).accounted_bytes(), 100);
    }

    #[test]
    fn snapshot_totals_accounted_bytes() {
        let snap = Snapshot {
            taken_at_unix_ms: 0,
            sample_duration: Duration::from_millis(1),
            samples: vec![sample(100, Some(40), None), sample(200, None, None)],
        };
        assert_eq!(snap.total_accounted_bytes(), 240);
    }
}
