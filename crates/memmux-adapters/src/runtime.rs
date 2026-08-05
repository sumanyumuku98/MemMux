//! A running provider instance: a PTY session with bounded capture and live screen state.
//!
//! This is the concrete `launch` behaviour (§12.1) — it ties an adapter to `memmux-pty`, pumping
//! output into the bounded capture buffer and vt100 screen so the daemon/TUI can observe it.

use crate::adapter::{EventWindow, LaunchSpec, ProviderAdapter};
use memmux_core::{Provider, TaskState};
use memmux_pty::{CaptureBuffer, CaptureConfig, PtySession, Screen};

/// A launched provider session under MemMux management.
pub struct RuntimeInstance {
    provider: Provider,
    session: PtySession,
    capture: CaptureBuffer,
    screen: Screen,
    last_activity_ms: u64,
}

impl RuntimeInstance {
    /// Launch `adapter` per `spec`, spawning the provider in a PTY.
    pub fn launch(
        adapter: &dyn ProviderAdapter,
        spec: &LaunchSpec,
        now_ms: u64,
    ) -> anyhow::Result<Self> {
        let pty_spec = adapter.command(spec);
        let (rows, cols) = (pty_spec.rows, pty_spec.cols);
        let session = PtySession::spawn(&pty_spec)?;
        Ok(Self {
            provider: adapter.provider(),
            session,
            capture: CaptureBuffer::new(CaptureConfig::default()),
            screen: Screen::new(rows, cols, 1000),
            last_activity_ms: now_ms,
        })
    }

    /// Provider this instance serves.
    pub fn provider(&self) -> Provider {
        self.provider
    }

    /// Drain available output into the capture buffer and screen. Returns bytes ingested.
    pub fn pump(&mut self, now_ms: u64) -> usize {
        let mut total = 0;
        while let Some(chunk) = self.session.try_read_output() {
            self.screen.process(&chunk);
            self.capture
                .ingest(&String::from_utf8_lossy(&chunk), now_ms);
            total += chunk.len();
            self.last_activity_ms = now_ms;
        }
        total
    }

    /// A classification window built from the current screen and last activity.
    pub fn event_window(&self) -> EventWindow {
        let recent_lines: Vec<String> = self
            .screen
            .rows()
            .into_iter()
            .filter(|l| !l.trim().is_empty())
            .collect();
        EventWindow {
            recent_lines,
            last_activity_ms: self.last_activity_ms,
            tool_running: false,
        }
    }

    /// Classify the current sub-state using the adapter's heuristics.
    pub fn classify(
        &self,
        adapter: &dyn ProviderAdapter,
        now_ms: u64,
        idle_after_ms: u64,
    ) -> TaskState {
        adapter.classify(&self.event_window(), now_ms, idle_after_ms)
    }

    /// Forward bytes to the provider's stdin.
    pub fn write_stdin(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.session.write_stdin(data)
    }

    /// Whether the provider process is still running.
    pub fn is_running(&mut self) -> bool {
        self.session.is_running()
    }

    /// The current screen rows (what the TUI term-pane renders).
    pub fn screen_rows(&self) -> Vec<String> {
        self.screen.rows()
    }

    /// Resident bytes of the capture buffer (bounded).
    pub fn capture_resident_bytes(&self) -> usize {
        self.capture.resident_bytes()
    }

    /// Terminate the provider.
    pub fn stop(&mut self) -> std::io::Result<()> {
        self.session.kill()
    }
}
