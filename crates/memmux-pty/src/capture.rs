//! The bounded-capture pipeline (§8.2).
//!
//! Raw provider output bytes go in; a bounded, collapse-aware, quota-governed line buffer comes
//! out — plus a stream of evicted lines (for the chunk store), oversized-line artifacts (for
//! disk), and governance events (for the audit log). This is the single place that guarantees
//! "no memory growth proportional to historical terminal output" (§4.2).

use crate::governance::{GovernanceConfig, GovernanceDecision, OutputGovernor};
use crate::line::StoredLine;
use crate::ring::{BoundedBuffer, RingCaps};
use crate::truncate::{truncate_line, TruncateOutcome};

pub use crate::governance::GovernanceBreach;

/// Configuration for the whole capture pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureConfig {
    /// Resident buffer caps.
    pub ring: RingCaps,
    /// Output governance limits.
    pub governance: GovernanceConfig,
    /// Per-line size cap before truncation to a disk artifact.
    pub max_line_bytes: usize,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            ring: RingCaps::default(),
            governance: GovernanceConfig::default(),
            max_line_bytes: 64 * 1024, // 64 KiB (§8.4)
        }
    }
}

/// A full oversized line to be written to a disk artifact by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingArtifact {
    /// The reference stored in the buffer's `Truncated` entry.
    pub artifact_ref: String,
    /// The full original line content.
    pub content: String,
}

/// The bounded terminal-capture buffer for one task.
#[derive(Debug)]
pub struct CaptureBuffer {
    cfg: CaptureConfig,
    buffer: BoundedBuffer,
    governor: OutputGovernor,
    pending: String,
    artifacts: Vec<PendingArtifact>,
    events: Vec<GovernanceBreach>,
    artifact_seq: u64,
}

impl CaptureBuffer {
    /// Create a capture buffer.
    pub fn new(cfg: CaptureConfig) -> Self {
        Self {
            cfg,
            buffer: BoundedBuffer::new(cfg.ring),
            governor: OutputGovernor::new(cfg.governance),
            pending: String::new(),
            artifacts: Vec::new(),
            events: Vec::new(),
            artifact_seq: 0,
        }
    }

    /// Feed a chunk of provider output. Splits into lines on `\n`, `\r`, and `\r\n`, buffering
    /// any trailing partial line until more output arrives. A partial line that grows past the
    /// per-line cap is force-flushed so `pending` can never grow unbounded.
    pub fn ingest(&mut self, data: &str, now_ms: u64) {
        self.pending.push_str(data);
        loop {
            if let Some(idx) = self.pending.find(['\n', '\r']) {
                let line: String = self.pending[..idx].to_string();
                // Consume the terminator (and a paired \n after \r).
                let mut cut = idx + 1;
                let bytes = self.pending.as_bytes();
                if bytes[idx] == b'\r' && bytes.get(idx + 1) == Some(&b'\n') {
                    cut += 1;
                }
                self.pending.drain(..cut);
                self.ingest_line(&line, now_ms);
            } else if self.pending.len() > self.cfg.max_line_bytes {
                // No terminator but the partial is already oversized: flush it now.
                let line = std::mem::take(&mut self.pending);
                self.ingest_line(&line, now_ms);
                break;
            } else {
                break;
            }
        }
    }

    /// Flush any buffered partial line as a complete line (e.g. on process exit).
    pub fn flush(&mut self, now_ms: u64) {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.ingest_line(&line, now_ms);
        }
    }

    fn ingest_line(&mut self, line: &str, now_ms: u64) {
        let outcome = self.governor.on_line(line.len() as u64, now_ms);
        if let Some(event) = outcome.event {
            self.events.push(event);
        }
        match outcome.decision {
            GovernanceDecision::SampledOut | GovernanceDecision::QuotaExceeded => {
                // Not persisted; the breach event (if any) already records why.
            }
            GovernanceDecision::Store => match truncate_line(line, self.cfg.max_line_bytes) {
                TruncateOutcome::Kept(text) => self.buffer.push(StoredLine::Text { text }),
                TruncateOutcome::Truncated {
                    head,
                    full_bytes,
                    full,
                } => {
                    let artifact_ref = format!("art_{}", self.artifact_seq);
                    self.artifact_seq += 1;
                    self.artifacts.push(PendingArtifact {
                        artifact_ref: artifact_ref.clone(),
                        content: full,
                    });
                    self.buffer.push(StoredLine::Truncated {
                        head,
                        full_bytes,
                        artifact_ref,
                    });
                }
            },
        }
    }

    /// Take lines evicted from the resident buffer (hand to the chunk store).
    pub fn drain_evicted(&mut self) -> Vec<StoredLine> {
        self.buffer.drain_evicted()
    }

    /// Take oversized-line artifacts pending disk persistence.
    pub fn drain_artifacts(&mut self) -> Vec<PendingArtifact> {
        std::mem::take(&mut self.artifacts)
    }

    /// Take governance breach events pending audit.
    pub fn drain_events(&mut self) -> Vec<GovernanceBreach> {
        std::mem::take(&mut self.events)
    }

    /// Resident byte estimate of the live buffer.
    pub fn resident_bytes(&self) -> usize {
        self.buffer.resident_bytes()
    }

    /// Resident entry count.
    pub fn line_count(&self) -> usize {
        self.buffer.line_count()
    }

    /// Rendered terminal rows the resident entries represent.
    pub fn rendered_rows(&self) -> u64 {
        self.buffer.rendered_rows()
    }

    /// Iterate resident lines, oldest to newest.
    pub fn lines(&self) -> impl Iterator<Item = &StoredLine> {
        self.buffer.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_mixed_terminators() {
        let mut c = CaptureBuffer::new(CaptureConfig::default());
        c.ingest("a\nb\r\nc\rd", 0);
        // a, b, c complete; d is still pending.
        assert_eq!(c.line_count(), 3);
        c.flush(0);
        assert_eq!(c.line_count(), 4);
    }

    #[test]
    fn partial_lines_span_ingest_calls() {
        let mut c = CaptureBuffer::new(CaptureConfig::default());
        c.ingest("hel", 0);
        c.ingest("lo\n", 0);
        assert_eq!(c.line_count(), 1);
        assert_eq!(c.lines().next().unwrap().render(), "hello");
    }

    #[test]
    fn giant_line_is_truncated_and_produces_an_artifact() {
        let cfg = CaptureConfig {
            max_line_bytes: 100,
            ..Default::default()
        };
        let mut c = CaptureBuffer::new(cfg);
        let big = "Z".repeat(10_000);
        c.ingest(&format!("{big}\n"), 0);
        let artifacts = c.drain_artifacts();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].content.len(), 10_000);
        match c.lines().next().unwrap() {
            StoredLine::Truncated { full_bytes, .. } => assert_eq!(*full_bytes, 10_000),
            other => panic!("expected truncated, got {other:?}"),
        };
    }

    #[test]
    fn unterminated_giant_line_does_not_grow_pending_unbounded() {
        let cfg = CaptureConfig {
            max_line_bytes: 1000,
            ..Default::default()
        };
        let mut c = CaptureBuffer::new(cfg);
        // 1 MiB with no newline at all.
        c.ingest(&"x".repeat(1_000_000), 0);
        // It was force-flushed and truncated, so at least one artifact exists and the buffer
        // holds a bounded truncated entry.
        assert!(!c.drain_artifacts().is_empty());
        assert!(c.resident_bytes() < 2000);
    }

    #[test]
    fn repeated_output_collapses_through_the_pipeline() {
        let mut c = CaptureBuffer::new(CaptureConfig::default());
        for _ in 0..500 {
            c.ingest("downloading...\n", 0);
        }
        assert_eq!(c.line_count(), 1);
        assert_eq!(c.rendered_rows(), 500);
    }
}
