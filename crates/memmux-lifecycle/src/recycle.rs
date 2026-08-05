//! Recycling policy and the reclaimed-bytes ledger (§8.7 / SUM-94, SUM-97).
//!
//! Recycling restarts a provider process that has grown past an RSS threshold, resuming it from a
//! checkpoint so no work is lost. This module holds the *decision* (should we recycle?) and the
//! *ledger* (how much did we actually reclaim?) — both pure, so the daemon only supplies numbers.

use crate::resume::ResumeMode;
use memmux_core::Provider;
use serde::{Deserialize, Serialize};

use crate::{GIB, MIB};

/// Per-provider RSS threshold that triggers recycling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecyclePolicy {
    /// Recycle once a provider's resident set crosses this many bytes.
    pub rss_threshold_bytes: u64,
}

impl RecyclePolicy {
    /// A policy with an explicit threshold.
    pub fn new(rss_threshold_bytes: u64) -> Self {
        Self {
            rss_threshold_bytes,
        }
    }

    /// Conservative per-provider defaults (§8.7). Heavier agents get more headroom before the
    /// runtime pays the recycle cost.
    pub fn for_provider(provider: Provider) -> Self {
        let threshold = match provider {
            Provider::ClaudeCode => 4 * GIB,
            Provider::Codex => 3 * GIB,
            Provider::GeminiCli => 3 * GIB,
            Provider::OpenCode => 2 * GIB,
            Provider::Generic => 2 * GIB,
        };
        Self::new(threshold)
    }

    /// If `rss_bytes` is at or over the threshold, return the reason to recycle; else `None`.
    pub fn should_recycle(&self, rss_bytes: u64) -> Option<String> {
        (rss_bytes >= self.rss_threshold_bytes).then(|| {
            format!(
                "RSS {} MiB reached the {} MiB recycle threshold",
                rss_bytes / MIB,
                self.rss_threshold_bytes / MIB
            )
        })
    }
}

/// Before/after RSS around a recycle, yielding the reclaimed amount.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reclamation {
    /// RSS (bytes) just before the provider was shut down.
    pub rss_before: u64,
    /// RSS (bytes) of the freshly resumed provider.
    pub rss_after: u64,
}

impl Reclamation {
    /// Bytes reclaimed (signed: negative if the new instance is larger).
    pub fn reclaimed_bytes(&self) -> i64 {
        self.rss_before as i64 - self.rss_after as i64
    }

    /// Whether the recycle actually freed measurable memory.
    pub fn is_measurable(&self) -> bool {
        self.reclaimed_bytes() > 0
    }

    /// A human-readable summary — explicitly says so when nothing was reclaimed (SUM-97).
    pub fn summary(&self) -> String {
        let delta = self.reclaimed_bytes();
        if delta > 0 {
            format!("reclaimed {} MiB", delta as u64 / MIB)
        } else if delta == 0 {
            "no measurable reclamation".into()
        } else {
            format!(
                "no measurable reclamation (new instance is {} MiB larger)",
                (-delta) as u64 / MIB
            )
        }
    }
}

/// The `runtime_recycled` ledger record (Appendix B event shape / SUM-97). Serialized into the
/// event payload and surfaced in the TUI timeline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecycleRecord {
    /// RSS before shutdown (bytes).
    pub rss_before: u64,
    /// RSS after resume (bytes).
    pub rss_after: u64,
    /// Signed reclaimed bytes.
    pub reclaimed_bytes: i64,
    /// How the task was resumed.
    pub resume_mode: ResumeMode,
    /// Resume latency (ms).
    pub resume_latency_ms: u64,
    /// The checkpoint's git patch hash (ties the ledger entry to a repo state).
    pub git_patch_hash: String,
}

impl RecycleRecord {
    /// Assemble a ledger record from a reclamation measurement and resume outcome.
    pub fn new(
        reclamation: Reclamation,
        resume_mode: ResumeMode,
        resume_latency_ms: u64,
        git_patch_hash: impl Into<String>,
    ) -> Self {
        Self {
            rss_before: reclamation.rss_before,
            rss_after: reclamation.rss_after,
            reclaimed_bytes: reclamation.reclaimed_bytes(),
            resume_mode,
            resume_latency_ms,
            git_patch_hash: git_patch_hash.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_defaults_are_ordered_by_weight() {
        let claude = RecyclePolicy::for_provider(Provider::ClaudeCode).rss_threshold_bytes;
        let generic = RecyclePolicy::for_provider(Provider::Generic).rss_threshold_bytes;
        assert!(claude > generic);
    }

    #[test]
    fn should_recycle_fires_at_or_above_threshold() {
        let p = RecyclePolicy::new(2 * GIB);
        assert!(p.should_recycle(2 * GIB).is_some());
        assert!(p.should_recycle(2 * GIB + 1).is_some());
        assert!(p.should_recycle(2 * GIB - 1).is_none());
    }

    #[test]
    fn reclamation_reports_freed_memory() {
        let r = Reclamation {
            rss_before: 3 * GIB,
            rss_after: GIB,
        };
        assert_eq!(r.reclaimed_bytes(), 2 * GIB as i64);
        assert!(r.is_measurable());
        assert!(r.summary().contains("reclaimed"));
    }

    #[test]
    fn reclamation_reports_no_measurable_reclamation() {
        let same = Reclamation {
            rss_before: GIB,
            rss_after: GIB,
        };
        assert!(!same.is_measurable());
        assert_eq!(same.summary(), "no measurable reclamation");

        let bigger = Reclamation {
            rss_before: GIB,
            rss_after: 2 * GIB,
        };
        assert!(!bigger.is_measurable());
        assert!(bigger.summary().contains("no measurable reclamation"));
        assert!(bigger.summary().contains("larger"));
    }

    #[test]
    fn ledger_record_captures_all_fields() {
        let rec = RecycleRecord::new(
            Reclamation {
                rss_before: 3 * GIB,
                rss_after: GIB,
            },
            ResumeMode::Native,
            250,
            "abcd1234",
        );
        assert_eq!(rec.reclaimed_bytes, 2 * GIB as i64);
        assert_eq!(rec.resume_mode, ResumeMode::Native);
        assert_eq!(rec.git_patch_hash, "abcd1234");
        let json = serde_json::to_string(&rec).unwrap();
        assert!(json.contains("\"resume_mode\":\"native\""));
    }
}
