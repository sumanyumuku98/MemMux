//! # memmux-lifecycle
//!
//! Pure, I/O-free lifecycle logic for the MemMux runtime (§8 recycling, §13 checkpoint /
//! hibernate / native resume). Like [`memmux_sched`](../memmux_sched/index.html), this crate
//! holds only decision logic and data contracts so it can be exhaustively unit-tested and reused
//! by the daemon without dragging in a process, a socket, or a clock.
//!
//! * [`checkpoint`] — what a checkpoint holds and how its integrity is verified (SUM-90), plus
//!   [`checkpoint::SecretRef`] so checkpoints reference secrets rather than embed them (SUM-79).
//! * [`safepoint`] — conservative safe-point detection shared by hibernation and recycling
//!   (SUM-91).
//! * [`recycle`] — the RSS-threshold recycle trigger (SUM-94) and the reclaimed-bytes ledger
//!   (SUM-97).
//! * [`resume`] — resume modes and outcomes for native resume + reconstructed fallback
//!   (SUM-92, SUM-93).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod checkpoint;
pub mod recycle;
pub mod resume;
pub mod safepoint;

pub use checkpoint::{content_hash, Checkpoint, SecretRef, SecretSource};
pub use recycle::{Reclamation, RecyclePolicy, RecycleRecord};
pub use resume::{ResumeMode, ResumeOutcome};
pub use safepoint::{assess, ActivitySnapshot, SafePoint, SafePointWaiter, WaitDecision};

/// Bytes in a mebibyte (used for human-readable summaries).
pub const MIB: u64 = 1024 * 1024;
/// Bytes in a gibibyte.
pub const GIB: u64 = 1024 * MIB;
