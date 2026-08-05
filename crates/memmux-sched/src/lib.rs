//! # memmux-sched
//!
//! The memory budget and scheduler (§7). Pure, I/O-free logic so it can be exhaustively tested
//! and reused by the daemon:
//!
//! * [`envelope`] — the global agent budget from host memory minus reserves (SUM-44).
//! * [`reservation`] — per-task reservation model (SUM-45).
//! * [`classes`] — resource-class priors and EWMA peak prediction (SUM-46).
//! * [`mod@score`] — scheduling score and admission queue (SUM-47).
//! * [`pressure`] — the pressure ladder (SUM-48).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod classes;
pub mod envelope;
pub mod pressure;
pub mod reservation;
pub mod score;

pub use classes::{class_reservation, EwmaPredictor};
pub use envelope::{Reserves, ResourceEnvelope};
pub use pressure::{PressureAction, PressureStage};
pub use reservation::Reservation;
pub use score::{plan_admission, score, AdmissionPlan, Candidate, ScoreInputs, ScoreWeights};

/// Bytes in one gibibyte.
pub const GIB: u64 = 1024 * 1024 * 1024;
/// Bytes in one mebibyte.
pub const MIB: u64 = 1024 * 1024;
