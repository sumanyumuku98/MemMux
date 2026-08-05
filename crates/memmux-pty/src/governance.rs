//! Output governance quotas (SUM-55 / §8.4).
//!
//! Caps how much output a single task may persist and how fast it may stream. High-rate output
//! is *sampled* rather than fully stored, and the total artifact size is bounded. Breaches are
//! surfaced as events (edge-triggered) so nothing is ever silently dropped.

use serde::{Deserialize, Serialize};

/// Configurable per-task output governance limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceConfig {
    /// Maximum total bytes of persisted log artifacts for the task.
    pub max_log_artifact_bytes: u64,
    /// Lines-per-second above which output is sampled instead of fully stored.
    pub sample_above_lines_per_sec: u64,
    /// Keep 1 of every `sample_ratio` lines when sampling (>= 1).
    pub sample_ratio: u64,
}

impl Default for GovernanceConfig {
    fn default() -> Self {
        Self {
            max_log_artifact_bytes: 5 * 1024 * 1024 * 1024, // 5 GiB (Appendix C)
            sample_above_lines_per_sec: 5_000,
            sample_ratio: 20,
        }
    }
}

/// What the governor decided for a line of output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GovernanceDecision {
    /// Persist the line normally.
    Store,
    /// Drop the line from persistence (sampled out due to high rate) — still counted.
    SampledOut,
    /// The artifact quota is exhausted; the line is not persisted.
    QuotaExceeded,
}

/// An edge-triggered governance breach worth surfacing as an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "breach", rename_all = "snake_case")]
pub enum GovernanceBreach {
    /// Output rate crossed the sampling threshold; sampling engaged.
    RateSampling {
        /// Observed lines per second at the breach.
        lines_per_sec: u64,
    },
    /// The artifact quota was reached; further output is not persisted.
    ArtifactQuota {
        /// The configured limit.
        limit_bytes: u64,
    },
}

/// The full outcome of governing one line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GovernanceOutcome {
    /// The persistence decision.
    pub decision: GovernanceDecision,
    /// An event to emit, if this line triggered a breach transition.
    pub event: Option<GovernanceBreach>,
}

/// Tracks a task's output rate and artifact quota.
#[derive(Debug)]
pub struct OutputGovernor {
    cfg: GovernanceConfig,
    window_start_ms: u64,
    lines_this_window: u64,
    lines_since_sample: u64,
    artifact_bytes: u64,
    rate_breach_active: bool,
    quota_breached: bool,
}

impl OutputGovernor {
    /// Create a governor with the given config.
    pub fn new(cfg: GovernanceConfig) -> Self {
        Self {
            cfg,
            window_start_ms: 0,
            lines_this_window: 0,
            lines_since_sample: 0,
            artifact_bytes: 0,
            rate_breach_active: false,
            quota_breached: false,
        }
    }

    /// Govern one output line of `bytes` bytes observed at `now_ms`.
    pub fn on_line(&mut self, bytes: u64, now_ms: u64) -> GovernanceOutcome {
        // Slide the 1-second rate window.
        if now_ms.saturating_sub(self.window_start_ms) >= 1000 {
            self.window_start_ms = now_ms;
            self.lines_this_window = 0;
            self.rate_breach_active = false;
        }
        self.lines_this_window += 1;

        // Quota check first: once exhausted, nothing more is persisted.
        if self.artifact_bytes.saturating_add(bytes) > self.cfg.max_log_artifact_bytes {
            let event = (!self.quota_breached).then(|| {
                self.quota_breached = true;
                GovernanceBreach::ArtifactQuota {
                    limit_bytes: self.cfg.max_log_artifact_bytes,
                }
            });
            return GovernanceOutcome {
                decision: GovernanceDecision::QuotaExceeded,
                event,
            };
        }

        // Rate check: above the threshold we sample 1-in-N.
        let over_rate = self.lines_this_window > self.cfg.sample_above_lines_per_sec;
        if over_rate {
            let event = (!self.rate_breach_active).then(|| {
                self.rate_breach_active = true;
                GovernanceBreach::RateSampling {
                    lines_per_sec: self.lines_this_window,
                }
            });
            self.lines_since_sample += 1;
            let ratio = self.cfg.sample_ratio.max(1);
            if self.lines_since_sample % ratio == 0 {
                self.artifact_bytes += bytes;
                return GovernanceOutcome {
                    decision: GovernanceDecision::Store,
                    event,
                };
            }
            return GovernanceOutcome {
                decision: GovernanceDecision::SampledOut,
                event,
            };
        }

        self.artifact_bytes += bytes;
        GovernanceOutcome {
            decision: GovernanceDecision::Store,
            event: None,
        }
    }

    /// Total bytes persisted to artifacts so far.
    pub fn artifact_bytes(&self) -> u64 {
        self.artifact_bytes
    }

    /// Whether the artifact quota has been breached.
    pub fn quota_breached(&self) -> bool {
        self.quota_breached
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_rate_stores_everything() {
        let mut g = OutputGovernor::new(GovernanceConfig::default());
        for i in 0..100 {
            let out = g.on_line(80, i);
            assert_eq!(out.decision, GovernanceDecision::Store);
            assert!(out.event.is_none());
        }
        assert_eq!(g.artifact_bytes(), 100 * 80);
    }

    #[test]
    fn high_rate_engages_sampling_with_one_breach_event() {
        let cfg = GovernanceConfig {
            max_log_artifact_bytes: u64::MAX,
            sample_above_lines_per_sec: 10,
            sample_ratio: 5,
        };
        let mut g = OutputGovernor::new(cfg);
        let mut stored = 0;
        let mut sampled_out = 0;
        let mut breach_events = 0;
        // 100 lines all within the same second (now_ms = 0).
        for _ in 0..100 {
            let out = g.on_line(10, 0);
            match out.decision {
                GovernanceDecision::Store => stored += 1,
                GovernanceDecision::SampledOut => sampled_out += 1,
                GovernanceDecision::QuotaExceeded => unreachable!(),
            }
            if out.event.is_some() {
                breach_events += 1;
            }
        }
        // First 10 stored; the rest sampled at 1-in-5.
        assert!(stored < 100 && sampled_out > 0);
        assert_eq!(
            breach_events, 1,
            "rate breach should be edge-triggered once"
        );
    }

    #[test]
    fn artifact_quota_blocks_further_persistence_and_fires_once() {
        let cfg = GovernanceConfig {
            max_log_artifact_bytes: 1000,
            sample_above_lines_per_sec: u64::MAX,
            sample_ratio: 1,
        };
        let mut g = OutputGovernor::new(cfg);
        let mut quota_events = 0;
        for i in 0..50 {
            let out = g.on_line(100, i);
            if out.event == Some(GovernanceBreach::ArtifactQuota { limit_bytes: 1000 }) {
                quota_events += 1;
            }
        }
        assert!(g.quota_breached());
        assert!(g.artifact_bytes() <= 1000);
        assert_eq!(quota_events, 1, "quota breach should surface exactly once");
    }

    #[test]
    fn rate_window_resets_after_a_second() {
        let cfg = GovernanceConfig {
            max_log_artifact_bytes: u64::MAX,
            sample_above_lines_per_sec: 3,
            sample_ratio: 2,
        };
        let mut g = OutputGovernor::new(cfg);
        for _ in 0..5 {
            g.on_line(10, 0);
        }
        // New second -> window resets, first few lines stored again.
        let out = g.on_line(10, 1000);
        assert_eq!(out.decision, GovernanceDecision::Store);
    }
}
