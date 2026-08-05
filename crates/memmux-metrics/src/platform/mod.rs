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
