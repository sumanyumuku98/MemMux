//! Linux process sampler: `/proc/<pid>/stat`, `status` (VmRSS), and `smaps_rollup` (PSS).
//!
//! SUM-27. Only compiled on Linux; the parsing logic it relies on lives in
//! [`crate::platform::linux_parse`] and is tested on all hosts.

use super::linux_parse::{parse_smaps_rollup_pss, parse_stat, parse_status_kb_field};
use crate::sample::{now_unix_ms, ProcessSample, ProcessSampler, Snapshot};
use std::fs;
use std::time::Instant;

/// Samples the process tree by walking `/proc`.
#[derive(Debug, Default)]
pub struct LinuxSampler;

impl LinuxSampler {
    /// Create a new sampler.
    pub fn new() -> Self {
        Self
    }

    fn sample_one(pid_dir: &str) -> Option<ProcessSample> {
        let base = format!("/proc/{pid_dir}");
        let stat = fs::read_to_string(format!("{base}/stat")).ok()?;
        let info = parse_stat(&stat)?;

        // Skip zombies: they hold no memory and are pending reap, so they are effectively gone
        // for accounting and termination (a killed child appears as `Z` in /proc until its
        // parent reaps it).
        if info.is_zombie() {
            return None;
        }

        // RSS from status (VmRSS) — avoids needing the page size.
        let rss_bytes = fs::read_to_string(format!("{base}/status"))
            .ok()
            .and_then(|s| parse_status_kb_field(&s, "VmRSS:"))
            .unwrap_or(0);

        // PSS from smaps_rollup — may be denied for other users' processes.
        let pss_bytes = fs::read_to_string(format!("{base}/smaps_rollup"))
            .ok()
            .and_then(|s| parse_smaps_rollup_pss(&s));

        Some(ProcessSample {
            pid: info.pid,
            ppid: info.ppid,
            name: info.comm,
            rss_bytes,
            pss_bytes,
            phys_footprint_bytes: None,
        })
    }
}

impl ProcessSampler for LinuxSampler {
    fn snapshot(&self) -> std::io::Result<Snapshot> {
        let start = Instant::now();
        let mut samples = Vec::new();
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Only numeric directory names are process ids.
            if !name.bytes().all(|b| b.is_ascii_digit()) || name.is_empty() {
                continue;
            }
            if let Some(sample) = Self::sample_one(&name) {
                samples.push(sample);
            }
        }
        Ok(Snapshot {
            taken_at_unix_ms: now_unix_ms(),
            sample_duration: start.elapsed(),
            samples,
        })
    }

    fn platform(&self) -> &'static str {
        "linux-proc"
    }
}
