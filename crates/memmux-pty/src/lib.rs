//! # memmux-pty
//!
//! Bounded terminal capture and (in a follow-up) PTY session management (§8.2, §8.4).
//!
//! The capture pipeline guarantees the product's central memory property: resident memory is a
//! function of the configured caps, never of how much output has scrolled past.
//!
//! * [`line`] — the compact stored-line representation.
//! * [`ring`] — bounded ring buffer with line + byte caps and repeated-line collapse
//!   (SUM-50, SUM-53).
//! * [`truncate`] — giant-line truncation to a disk artifact (SUM-54).
//! * [`governance`] — per-task output-rate and artifact-size quotas (SUM-55).
//! * [`capture`] — the pipeline tying them together.
//! * [`chunkstore`] — zstd-compressed history with paged reads (SUM-52).
//! * [`screen`] — vt100 screen state with checkpoints (SUM-51).
//! * [`session`] — daemon-owned PTY session (SUM-49).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod capture;
pub mod chunkstore;
pub mod governance;
pub mod line;
pub mod ring;
pub mod screen;
pub mod session;
pub mod truncate;

pub use capture::{CaptureBuffer, CaptureConfig, PendingArtifact};
pub use chunkstore::{ChunkMeta, ChunkStore};
pub use governance::{GovernanceBreach, GovernanceConfig, OutputGovernor};
pub use line::StoredLine;
pub use ring::{BoundedBuffer, RingCaps};
pub use screen::Screen;
pub use session::{PtySession, PtySpec};
pub use truncate::{truncate_line, TruncateOutcome};
