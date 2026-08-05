//! # memmux-bench
//!
//! The MemMux competitive benchmark harness (Phase 0, §18). It answers the benchmark questions
//! — how much memory is attributable to the runtime, whether memory stays bounded with output
//! volume and session age, and what the sampling overhead is — by driving a deterministic stub
//! agent through scenarios under a set of launchers and sampling it with `memmux-metrics`.
//!
//! Modules:
//! * [`stub`] — deterministic stub agent (SUM-32).
//! * [`scenario`] — burst / soak / idle / leak scenarios (SUM-36).
//! * [`launcher`] — baseline / MemMux / competitor launchers (SUM-34, SUM-35).
//! * [`sampler`] — time-series sampling to JSONL + overhead accounting (SUM-33, SUM-31).
//! * [`report`] — Markdown + sparkline report generator (SUM-37).
//! * [`matrix`] — §18.2 test-matrix enumeration (SUM-39).
//! * [`gates`] — §18.5 launch-gate checks (SUM-40).
//! * [`run`] — live orchestration tying it together.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod gates;
pub mod launcher;
pub mod matrix;
pub mod report;
pub mod run;
pub mod sampler;
pub mod scenario;
pub mod stub;

pub use scenario::Scenario;
pub use stub::SessionRecording;
