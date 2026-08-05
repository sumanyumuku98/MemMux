//! # memmux-adapters
//!
//! The provider adapter runtime (§12). Each adapter describes how to launch a specific
//! coding-agent CLI, negotiates its capabilities honestly, classifies task sub-state from
//! output, and estimates resources.
//!
//! * [`capabilities`] — the capability matrix providers negotiate (SUM-69 / §12.2).
//! * [`adapter`] — the [`ProviderAdapter`] contract (SUM-69 / §12.1).
//! * [`providers`] — generic / Claude Code / Codex / Gemini CLI / OpenCode adapters
//!   (SUM-70, SUM-71, SUM-72, SUM-73, SUM-74).
//! * [`isolation`] — least-privilege capability grants + capability-scoped secret resolution
//!   (SUM-75, SUM-79 / §12.3).
//! * [`runtime`] — a launched PTY-backed provider instance.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod adapter;
pub mod capabilities;
pub mod isolation;
pub mod providers;
pub mod runtime;

pub use adapter::{EventWindow, LaunchSpec, ProviderAdapter, ResourceEstimate};
pub use capabilities::ProviderCapabilities;
pub use isolation::CapabilityGrant;
pub use providers::{
    adapter_for, ClaudeCodeAdapter, CodexAdapter, GeminiCliAdapter, GenericTerminalAdapter,
    OpenCodeAdapter,
};
pub use runtime::RuntimeInstance;
