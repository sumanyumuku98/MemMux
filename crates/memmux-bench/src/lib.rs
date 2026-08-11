//! # memmux-bench
//!
//! The MemMux competitive benchmark harness (Phase 0, §18). It answers the benchmark questions
//! — how much memory is attributable to the runtime, whether memory stays bounded with output
//! volume and session age, and what the sampling overhead is — by driving a deterministic stub
//! agent through scenarios under a set of launchers and sampling it with `memmux-metrics`.
//!
//! Modules:
//! * [`stub`] — deterministic stub agent (SUM-32).
//! * [`scenario`] — burst / soak / idle / leak / hold scenarios (SUM-36, SUM-165).
//! * [`launcher`] — baseline / MemMux / competitor launchers (SUM-34, SUM-35).
//! * [`sampler`] — time-series sampling to JSONL + overhead accounting (SUM-33, SUM-31).
//! * [`cleanup`] — cleanup / leak-on-teardown measurement (SUM-165 / H2).
//! * [`escape`] — escaped-process visibility measurement (SUM-166 / H3).
//! * [`h4`] — bounded-footprint-under-overcommit + no-lost-work measurement (SUM-167/168 / H4).
//! * [`report`] — Markdown + sparkline report generator (SUM-37).
//! * [`matrix`] — §18.2 test-matrix enumeration (SUM-39).
//! * [`gates`] — §18.5 launch-gate checks (SUM-40).
//! * [`stats`] — multi-trial descriptive statistics + 95% CI (SUM-162).
//! * [`plot`] — dependency-free SVG line-chart figures (SUM-163).
//! * [`sweep`] — N-sweep agent-count list parsing (SUM-169 / P3).
//! * [`host`] — host-spec capture for the one-command reproducer (SUM-171 / P3).
//! * [`run`] — live orchestration tying it together.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cleanup;
pub mod escape;
pub mod gates;
pub mod h4;
pub mod host;
pub mod launcher;
pub mod matrix;
pub mod plot;
pub mod report;
pub mod run;
pub mod sampler;
pub mod scenario;
pub mod stats;
pub mod stub;
pub mod sweep;

pub use scenario::Scenario;
pub use stub::SessionRecording;
