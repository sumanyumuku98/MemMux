//! # memmuxd (library surface)
//!
//! The MemMux control-plane daemon internals, exposed so the `memmuxd` binary and integration
//! tests share one implementation:
//!
//! * [`daemon`] — durable-store-backed state, request handlers, crash recovery, audit.
//! * [`server`] — the async Unix-domain-socket server.
//! * [`client`] — a blocking client for the UDS API.
//! * [`frame`] — the length-prefixed frame codec.

#![warn(missing_docs)]

pub mod client;
pub mod daemon;
pub mod frame;
pub mod server;
