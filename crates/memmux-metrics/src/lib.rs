//! # memmux-metrics
//!
//! Process accounting and task attribution — the Phase 0 core of MemMux.
//!
//! Responsibilities (§14.2, §18.5 "Attribution" gate):
//!
//! * Sample the OS process tree with per-process memory (RSS, and the platform's best
//!   proportional metric — PSS on Linux, `phys_footprint` on macOS).
//! * Reconstruct parent/child relationships into a [`ProcessTree`].
//! * Reconcile the observed tree against declared task/shared-service roots to classify every
//!   process as **owned**, **shared**, **escaped**, or **unknown** ([`attribute`]).
//!
//! Platform-specific sampling lives behind the [`ProcessSampler`] trait; the pure `/proc`
//! parsers are compiled and unit-tested on every host so correctness is not gated on running
//! Linux.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod attribution;
pub mod sample;
pub mod sweep;
pub mod terminate;
pub mod tree;

mod platform;

pub use attribution::{attribute, Attribution, AttributionReport, Owner, RootSpec, ServiceId};
pub use platform::default_sampler;
pub use sample::{ProcessSample, ProcessSampler, Snapshot};
pub use sweep::{reconcile, reconcile_tree, FlaggedProcess, ReconciliationReport};
pub use terminate::{terminate_subtree, termination_targets, TerminationReport};
pub use tree::ProcessTree;
