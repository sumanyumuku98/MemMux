//! Resume modes and outcomes (§13.2 / SUM-92, SUM-93).
//!
//! When a hibernated or recycled task comes back, the runtime prefers the provider's *native*
//! resume (fastest, highest fidelity). If the provider can't resume natively — or a native
//! resume fails validation — it falls back to a *reconstructed* session (a fresh provider with a
//! context summary). If even that is impossible, only repository/task state is restored
//! ([`ResumeMode::ColdStart`]). Which path is taken is always surfaced to the user.

use serde::{Deserialize, Serialize};

/// How a task was brought back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeMode {
    /// Restored via the provider's own session mechanism (e.g. `claude --resume`).
    Native,
    /// A new session was started with a reconstructed context summary.
    Reconstructed,
    /// No session could be restored; only repository/task state was preserved.
    ColdStart,
}

impl ResumeMode {
    /// A short human label for TUI/event surfaces.
    pub fn label(self) -> &'static str {
        match self {
            ResumeMode::Native => "native",
            ResumeMode::Reconstructed => "reconstructed",
            ResumeMode::ColdStart => "cold-start",
        }
    }

    /// Whether this mode preserved the provider's conversational context.
    pub fn preserved_context(self) -> bool {
        !matches!(self, ResumeMode::ColdStart)
    }
}

/// The result of a resume attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeOutcome {
    /// Which path actually restored the task.
    pub mode: ResumeMode,
    /// End-to-end resume latency in milliseconds (reported as an event, SUM-92).
    pub latency_ms: u64,
    /// Whether post-resume validation confirmed fidelity (SUM-93).
    pub validated: bool,
    /// Human-readable detail (e.g. why a native resume fell back to reconstructed).
    pub detail: String,
}

impl ResumeOutcome {
    /// A successful native resume.
    pub fn native(latency_ms: u64) -> Self {
        Self {
            mode: ResumeMode::Native,
            latency_ms,
            validated: true,
            detail: "restored via provider-native session".into(),
        }
    }

    /// A reconstructed fallback, carrying the reason the native path was not used.
    pub fn reconstructed(latency_ms: u64, reason: impl Into<String>) -> Self {
        Self {
            mode: ResumeMode::Reconstructed,
            latency_ms,
            validated: true,
            detail: format!("reconstructed session: {}", reason.into()),
        }
    }

    /// A cold start — nothing but repo/task state survived.
    pub fn cold_start(latency_ms: u64, reason: impl Into<String>) -> Self {
        Self {
            mode: ResumeMode::ColdStart,
            latency_ms,
            validated: false,
            detail: format!("cold start: {}", reason.into()),
        }
    }

    /// Whether the resume both restored context and passed validation.
    pub fn is_high_fidelity(&self) -> bool {
        self.validated && self.mode.preserved_context()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_is_high_fidelity() {
        let o = ResumeOutcome::native(120);
        assert!(o.is_high_fidelity());
        assert_eq!(o.mode.label(), "native");
        assert_eq!(o.latency_ms, 120);
    }

    #[test]
    fn reconstructed_preserves_context_and_records_reason() {
        let o = ResumeOutcome::reconstructed(300, "provider has no session id");
        assert!(o.mode.preserved_context());
        assert!(o.is_high_fidelity());
        assert!(o.detail.contains("no session id"));
    }

    #[test]
    fn cold_start_is_low_fidelity() {
        let o = ResumeOutcome::cold_start(50, "resume unsupported");
        assert!(!o.mode.preserved_context());
        assert!(!o.is_high_fidelity());
    }

    #[test]
    fn outcome_round_trips() {
        let o = ResumeOutcome::native(10);
        let back: ResumeOutcome =
            serde_json::from_str(&serde_json::to_string(&o).unwrap()).unwrap();
        assert_eq!(o, back);
    }
}
