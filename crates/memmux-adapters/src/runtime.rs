//! A running provider instance: a PTY session with bounded capture and live screen state.
//!
//! This is the concrete `launch` behaviour (§12.1) — it ties an adapter to `memmux-pty`, pumping
//! output into the bounded capture buffer and vt100 screen so the daemon/TUI can observe it.

use crate::adapter::{EventWindow, LaunchSpec, ProviderAdapter};
use memmux_core::{Provider, TaskState};
use memmux_pty::{CaptureBuffer, CaptureConfig, PtySession, PtySpec, Screen, StoredLine};

/// Cap on buffered raw output for an attached client (bytes) — bounds memory if a client stalls.
const ATTACH_BUF_CAP: usize = 256 * 1024;

/// A launched provider session under MemMux management.
pub struct RuntimeInstance {
    provider: Provider,
    session: PtySession,
    capture: CaptureBuffer,
    screen: Screen,
    last_activity_ms: u64,
    /// When a client is attached, `pump` tees raw PTY bytes here for a true terminal passthrough
    /// (SUM-125). Empty/unused when no one is attached.
    attached: bool,
    attach_buf: Vec<u8>,
}

impl RuntimeInstance {
    /// Launch `adapter` per `spec`, spawning the provider in a PTY.
    pub fn launch(
        adapter: &dyn ProviderAdapter,
        spec: &LaunchSpec,
        now_ms: u64,
    ) -> anyhow::Result<Self> {
        Self::spawn_pty(adapter.provider(), adapter.command(spec), now_ms)
    }

    /// Launch from an explicit [`PtySpec`] (e.g. a provider-native resume command built by
    /// [`ProviderAdapter::resume_command`]). Lets the daemon choose the launch vs resume
    /// invocation while reusing the same capture/screen wiring.
    pub fn spawn_pty(provider: Provider, pty_spec: PtySpec, now_ms: u64) -> anyhow::Result<Self> {
        let (rows, cols) = (pty_spec.rows, pty_spec.cols);
        let session = PtySession::spawn(&pty_spec)?;
        Ok(Self {
            provider,
            session,
            capture: CaptureBuffer::new(CaptureConfig::default()),
            screen: Screen::new(rows, cols, 1000),
            last_activity_ms: now_ms,
            attached: false,
            attach_buf: Vec::new(),
        })
    }

    /// Provider this instance serves.
    pub fn provider(&self) -> Provider {
        self.provider
    }

    /// The provider process's OS pid, if available (drives RSS attribution for recycling).
    pub fn pid(&self) -> Option<u32> {
        self.session.pid()
    }

    /// Drain available output into the capture buffer and screen. Returns bytes ingested. When a
    /// client is attached, also tees the raw bytes for a terminal passthrough (SUM-125).
    pub fn pump(&mut self, now_ms: u64) -> usize {
        let mut total = 0;
        while let Some(chunk) = self.session.try_read_output() {
            self.screen.process(&chunk);
            self.capture
                .ingest(&String::from_utf8_lossy(&chunk), now_ms);
            if self.attached {
                self.attach_buf.extend_from_slice(&chunk);
                if self.attach_buf.len() > ATTACH_BUF_CAP {
                    let overflow = self.attach_buf.len() - ATTACH_BUF_CAP;
                    self.attach_buf.drain(..overflow);
                }
            }
            total += chunk.len();
            self.last_activity_ms = now_ms;
        }
        total
    }

    /// Begin/end teeing raw output for an attached client. Toggling clears any buffered bytes so a
    /// fresh attach starts clean (it relies on a resize-triggered repaint for the initial paint).
    pub fn set_attached(&mut self, attached: bool) {
        self.attached = attached;
        self.attach_buf.clear();
    }

    /// Take the raw output buffered since the last drain (empty if nothing new / not attached).
    pub fn drain_attach(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.attach_buf)
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

    /// The current cursor position `(row, col)`.
    pub fn cursor(&self) -> (u16, u16) {
        self.screen.cursor()
    }

    /// Take lines evicted from the resident buffer (the daemon persists these to the history
    /// chunk store so scrollback can be paged back).
    pub fn take_evicted(&mut self) -> Vec<StoredLine> {
        self.capture.drain_evicted()
    }

    /// Resize the terminal (session + screen grid).
    pub fn resize(&mut self, rows: u16, cols: u16) -> anyhow::Result<()> {
        self.session.resize(rows, cols)?;
        self.screen.resize(rows, cols);
        Ok(())
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
