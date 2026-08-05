//! macOS process sampler via `libproc` (SUM-28).
//!
//! Uses `proc_listpids` to enumerate the process table, `proc_pidinfo(PROC_PIDTASKALLINFO)`
//! for parent pid / name / resident size, and `proc_pid_rusage(RUSAGE_INFO_V2)` for
//! `ri_phys_footprint` — Apple's best per-process physical-memory figure and the macOS analog
//! of Linux PSS for attribution purposes.

use crate::sample::{now_unix_ms, ProcessSample, ProcessSampler, Snapshot};
use std::mem;
use std::time::Instant;

// Flavor / selector constants (defined locally to avoid depending on their presence in `libc`).
const PROC_ALL_PIDS: u32 = 1;
const PROC_PIDTASKALLINFO: libc::c_int = 2;

/// Samples the process tree using the macOS `libproc` API.
#[derive(Debug, Default)]
pub struct MacosSampler;

impl MacosSampler {
    /// Create a new sampler.
    pub fn new() -> Self {
        Self
    }

    /// Enumerate all pids on the system.
    fn list_pids() -> std::io::Result<Vec<libc::pid_t>> {
        // First call with a null buffer to learn how many bytes are needed.
        let needed = unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
        if needed <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        let count = needed as usize / mem::size_of::<libc::pid_t>();
        // Over-allocate slightly to tolerate races where new processes appear.
        let mut pids: Vec<libc::pid_t> = vec![0; count + 16];
        let byte_cap = (pids.len() * mem::size_of::<libc::pid_t>()) as libc::c_int;
        let filled = unsafe {
            libc::proc_listpids(
                PROC_ALL_PIDS,
                0,
                pids.as_mut_ptr() as *mut libc::c_void,
                byte_cap,
            )
        };
        if filled <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        let filled_count = filled as usize / mem::size_of::<libc::pid_t>();
        pids.truncate(filled_count);
        pids.retain(|&p| p > 0);
        Ok(pids)
    }

    fn sample_one(pid: libc::pid_t) -> Option<ProcessSample> {
        let mut info: libc::proc_taskallinfo = unsafe { mem::zeroed() };
        let size = mem::size_of::<libc::proc_taskallinfo>() as libc::c_int;
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                PROC_PIDTASKALLINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if written != size {
            // Process vanished or access denied; skip it.
            return None;
        }

        let name = c_array_to_string(&info.pbsd.pbi_name)
            .filter(|s| !s.is_empty())
            .or_else(|| c_array_to_string(&info.pbsd.pbi_comm))
            .unwrap_or_default();

        let phys_footprint_bytes = phys_footprint(pid);

        Some(ProcessSample {
            pid: pid as memmux_core::ids::Pid,
            ppid: info.pbsd.pbi_ppid as memmux_core::ids::Pid,
            name,
            rss_bytes: info.ptinfo.pti_resident_size,
            pss_bytes: None,
            phys_footprint_bytes,
        })
    }
}

impl ProcessSampler for MacosSampler {
    fn snapshot(&self) -> std::io::Result<Snapshot> {
        let start = Instant::now();
        let pids = Self::list_pids()?;
        let mut samples = Vec::with_capacity(pids.len());
        for pid in pids {
            if let Some(sample) = Self::sample_one(pid) {
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
        "macos-libproc"
    }
}

/// Query `ri_phys_footprint` for a pid via `proc_pid_rusage(RUSAGE_INFO_V2)`.
fn phys_footprint(pid: libc::pid_t) -> Option<u64> {
    let mut rusage: libc::rusage_info_v2 = unsafe { mem::zeroed() };
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V2,
            &mut rusage as *mut libc::rusage_info_v2 as *mut libc::rusage_info_t,
        )
    };
    if rc == 0 {
        Some(rusage.ri_phys_footprint)
    } else {
        None
    }
}

/// Convert a NUL-terminated C `char` array into an owned `String` (lossy, stops at NUL).
fn c_array_to_string(bytes: &[libc::c_char]) -> Option<String> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    if end == 0 {
        return None;
    }
    let slice: Vec<u8> = bytes[..end].iter().map(|&b| b as u8).collect();
    Some(String::from_utf8_lossy(&slice).into_owned())
}
