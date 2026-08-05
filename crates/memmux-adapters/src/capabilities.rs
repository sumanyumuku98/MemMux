//! Provider capability negotiation (§12.2).
//!
//! Every adapter declares, honestly, what it can and cannot do. The runtime surfaces these so
//! users always know whether e.g. a resume is native, reconstructed, or impossible.

use serde::{Deserialize, Serialize};

/// How faithfully a provider can resume a checkpointed session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeFidelity {
    /// Resumes via the provider's own session mechanism.
    Native,
    /// A new session is started with a reconstructed context summary.
    Reconstructed,
    /// Cannot resume; only repository/task state is preserved.
    Unsupported,
}

/// How a provider emits output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// Structured event stream.
    StructuredStream,
    /// Plain terminal text.
    TerminalText,
    /// A mix of both.
    Hybrid,
}

/// Whether a safe-point-to-recycle signal is available.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafePointSignal {
    /// The provider signals safe points natively.
    Native,
    /// Safe points are inferred from output.
    Inferred,
    /// No safe-point signal is available.
    Unavailable,
}

/// How visible a provider's tool calls are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolVisibility {
    /// Structured tool-call events.
    Structured,
    /// Derived from child processes.
    ProcessDerived,
    /// Opaque.
    Opaque,
}

/// How visible a provider's subagents are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentVisibility {
    /// Native subagent visibility.
    Native,
    /// Derived from child processes.
    ProcessDerived,
    /// Not supported.
    Unsupported,
}

/// How context compaction is achieved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompaction {
    /// A provider command compacts context.
    ProviderCommand,
    /// The runtime summarizes context.
    RuntimeSummary,
    /// Not supported.
    Unsupported,
}

/// How permissions are enforced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permissions {
    /// The provider enforces its own permissions.
    ProviderNative,
    /// The launch wrapper enforces permissions.
    WrapperEnforced,
    /// Both.
    Both,
}

/// The full capability matrix a provider negotiates (§12.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    /// Resume fidelity.
    pub resume: ResumeFidelity,
    /// Output mode.
    pub output: OutputMode,
    /// Safe-point signalling.
    pub safe_point: SafePointSignal,
    /// Tool-call visibility.
    pub tools: ToolVisibility,
    /// Subagent visibility.
    pub subagents: SubagentVisibility,
    /// Context-compaction mechanism.
    pub context_compaction: ContextCompaction,
    /// Permission enforcement.
    pub permissions: Permissions,
}

impl ProviderCapabilities {
    /// The most conservative capability set — safe defaults for an unknown provider.
    pub fn conservative() -> Self {
        Self {
            resume: ResumeFidelity::Unsupported,
            output: OutputMode::TerminalText,
            safe_point: SafePointSignal::Unavailable,
            tools: ToolVisibility::ProcessDerived,
            subagents: SubagentVisibility::Unsupported,
            context_compaction: ContextCompaction::Unsupported,
            permissions: Permissions::WrapperEnforced,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conservative_defaults_are_honest() {
        let c = ProviderCapabilities::conservative();
        assert_eq!(c.resume, ResumeFidelity::Unsupported);
        assert_eq!(c.permissions, Permissions::WrapperEnforced);
    }

    #[test]
    fn capabilities_round_trip() {
        let c = ProviderCapabilities::conservative();
        let json = serde_json::to_string(&c).unwrap();
        let back: ProviderCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }
}
