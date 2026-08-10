//! Platform-specific process sampling.
//!
//! The pure parsers in [`linux_parse`] are compiled on every target so they can be unit-tested
//! from macOS CI; the OS-touching samplers are gated by `cfg(target_os = …)`.

use crate::sample::ProcessSampler;

// The pure parsers are only wired into the sampler on Linux, but are compiled and tested
// everywhere; silence dead-code warnings on non-Linux hosts (tests still exercise them).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod linux_parse;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;

/// Construct the best process sampler for the current platform.
///
/// * Linux → `/proc` + `smaps_rollup` (PSS).
/// * macOS → `libproc` (`proc_pidinfo` + `proc_pid_rusage` `phys_footprint`).
/// * Other → an unsupported sampler that returns an error on use.
pub fn default_sampler() -> Box<dyn ProcessSampler> {
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxSampler::new())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacosSampler::new())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Box::new(unsupported::UnsupportedSampler)
    }
}

/// Best-effort current swap usage in **bytes**, or `None` if the platform can't report it cheaply.
///
/// The pressure ladder (SUM-48) treats a value rising across ticks as a leading indicator of
/// thrashing, escalating the pressure stage before the system is actually swapping hard. Only
/// Linux (`/proc/meminfo`) is wired today; other platforms return `None`, so the ladder falls back
/// to budget utilization + hard-limit, which already trigger below the swap-safety reserve.
pub fn swap_used_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let content = std::fs::read_to_string("/proc/meminfo").ok()?;
        linux_parse::parse_swap_used_meminfo(&content)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Cumulative CPU time consumed by a process so far, in **seconds** (user + system), or `None` if
/// the platform can't report it or the process is gone.
///
/// This is the primitive behind the benchmark's real sampling-overhead measurement (SUM-164, H5):
/// sampling the manager process's CPU seconds at the start and end of a run yields the wall-clock
/// CPU% the multiplexer's own bookkeeping costs, rather than a per-sample-duration proxy.
///
/// * Linux → `utime`+`stime` from `/proc/<pid>/stat` (clock ticks) divided by
///   `sysconf(_SC_CLK_TCK)`.
/// * macOS → `ri_user_time`+`ri_system_time` from `proc_pid_rusage(RUSAGE_INFO_V2)` (nanoseconds).
/// * Other → `None`.
pub fn process_cpu_seconds(pid: memmux_core::ids::Pid) -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let content = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (utime, stime) = linux_parse::parse_stat_cpu_ticks(&content)?;
        // SAFETY: `sysconf` is a pure query of a system constant with no pointer arguments.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if hz <= 0 {
            return None;
        }
        let ticks = utime.checked_add(stime)?;
        Some(ticks as f64 / hz as f64)
    }
    #[cfg(target_os = "macos")]
    {
        macos::process_cpu_seconds(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}
