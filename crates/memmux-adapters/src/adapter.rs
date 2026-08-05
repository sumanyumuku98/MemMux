//! The `ProviderAdapter` contract (§12.1).
//!
//! The spec sketches an async trait; MemMux's PTY layer is thread-based (see `memmux-pty`), so
//! the adapter contract here is synchronous and therefore `dyn`-compatible — a registry of
//! `Box<dyn ProviderAdapter>` needs no `async_trait`. Checkpoint/resume (Phase 2) are expressed
//! through the negotiated [`ProviderCapabilities`] rather than trait methods for now.

use crate::capabilities::ProviderCapabilities;
use memmux_core::{Provider, ResourceClass, TaskState};
use memmux_lifecycle::SecretRef;
use memmux_pty::PtySpec;
use memmux_sched::{class_reservation, Reservation};
use std::path::PathBuf;

/// How to launch a provider session.
#[derive(Clone, Debug, Default)]
pub struct LaunchSpec {
    /// Override the provider's default program+args (used by the generic adapter).
    pub command: Option<Vec<String>>,
    /// Extra arguments appended to the provider's default command.
    pub extra_args: Vec<String>,
    /// Working directory (typically the task's worktree).
    pub cwd: Option<PathBuf>,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
    /// Initial terminal rows.
    pub rows: u16,
    /// Initial terminal columns.
    pub cols: u16,
}

impl LaunchSpec {
    /// A launch spec for the given working directory at the default 24×80 size.
    pub fn in_dir(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: Some(cwd.into()),
            rows: 24,
            cols: 80,
            ..Default::default()
        }
    }
}

/// A bounded window of recent output lines used to classify task sub-state (§12.1 `classify`).
#[derive(Clone, Debug, Default)]
pub struct EventWindow {
    /// Recent output lines (most recent last).
    pub recent_lines: Vec<String>,
    /// Milliseconds since the epoch of the last observed activity.
    pub last_activity_ms: u64,
    /// Whether a task-critical child command is currently running.
    pub tool_running: bool,
}

/// A resource estimate for a task (§12.1 `estimate`). Backed by the scheduler's reservation.
pub type ResourceEstimate = Reservation;

/// A provider adapter: how to launch, classify, and estimate a specific coding-agent CLI.
pub trait ProviderAdapter: Send + Sync {
    /// Which provider this adapter serves.
    fn provider(&self) -> Provider;

    /// The negotiated capability matrix (§12.2).
    fn capabilities(&self) -> ProviderCapabilities;

    /// Build the PTY command for a launch (§12.1 `launch`).
    fn command(&self, spec: &LaunchSpec) -> PtySpec;

    /// Build the PTY command that resumes a prior session via the provider's *native* resume
    /// mechanism, given the session handle recorded in a checkpoint (§13.2 / SUM-92).
    ///
    /// Returns `None` when the provider has no native resume (the runtime then falls back to a
    /// reconstructed session — SUM-93). The default is `None`; only adapters whose
    /// [`capabilities`](Self::capabilities) report [`ResumeFidelity::Native`] should override it.
    ///
    /// [`ResumeFidelity::Native`]: crate::capabilities::ResumeFidelity::Native
    fn resume_command(&self, _spec: &LaunchSpec, _session_ref: &str) -> Option<PtySpec> {
        None
    }

    /// The secret *references* this provider needs at launch (SUM-79). These are names/sources,
    /// never values; the isolation layer resolves only those a task's grant allows. The default
    /// is none (e.g. the generic adapter needs no secrets).
    fn secret_refs(&self) -> Vec<SecretRef> {
        Vec::new()
    }

    /// Classify the current task sub-state from a window of recent output (§12.1 `classify`).
    ///
    /// The default heuristic treats output matching any of [`Self::waiting_markers`] as
    /// `WAITING_USER`, an active tool as `TOOL_RUNNING`, silence past `idle_after_ms` as `IDLE`,
    /// else `ACTIVE`.
    fn classify(&self, window: &EventWindow, now_ms: u64, idle_after_ms: u64) -> TaskState {
        if window.tool_running {
            return TaskState::ToolRunning;
        }
        let markers = self.waiting_markers();
        let awaiting = window
            .recent_lines
            .iter()
            .rev()
            .take(5)
            .any(|line| markers.iter().any(|m| line.contains(m)));
        if awaiting {
            return TaskState::WaitingUser;
        }
        if now_ms.saturating_sub(window.last_activity_ms) >= idle_after_ms {
            return TaskState::Idle;
        }
        TaskState::Active
    }

    /// Output substrings that indicate the provider is awaiting user input.
    fn waiting_markers(&self) -> &[&str] {
        &[]
    }

    /// Estimate the resource reservation for a task of the given class (§12.1 `estimate`).
    fn estimate(&self, class: ResourceClass) -> ResourceEstimate {
        class_reservation(class)
    }
}
