//! # memmux (TUI)
//!
//! The dense operational terminal UI for the MemMux daemon (§5.1, Appendix A). Built on the Elm
//! architecture so the interaction core ([`app`]) and rendering ([`render`]) are pure and
//! unit-testable; the terminal runtime lives in the binary.
//!
//! * [`app`] — model, keys, and the pure `update` (SUM-81, SUM-87).
//! * [`render`] — dashboard / tasks / queue / timeline / form / help views (SUM-82–89).
//! * [`client`] — the blocking UDS client that feeds live data in.

#![warn(missing_docs)]

pub mod app;
pub mod render;
pub mod theme;

/// Self-update: `memmux update` + the "update available" hint (SUM-129).
pub mod update;

#[cfg(unix)]
pub mod client;

/// Daemon auto-start / reuse for single-command startup (SUM-118).
#[cfg(unix)]
pub mod supervisor;
