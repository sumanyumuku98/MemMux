//! Fallback sampler for platforms MemMux does not yet support (Windows arrives with Job
//! Objects in a later milestone, per §4.2 Portability).

use crate::sample::{ProcessSampler, Snapshot};

/// A sampler that always reports that the current platform is unsupported.
#[derive(Debug, Default)]
pub struct UnsupportedSampler;

impl ProcessSampler for UnsupportedSampler {
    fn snapshot(&self) -> std::io::Result<Snapshot> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "process sampling is only implemented for Linux and macOS",
        ))
    }

    fn platform(&self) -> &'static str {
        "unsupported"
    }
}
