//! # memmux-proto
//!
//! Versioned protocol contract between MemMux clients (TUI, VS Code bridge, CLI, SDK) and the
//! daemon. Phase 0 only pins the protocol version and a couple of shared request/response
//! shapes; the gRPC-over-UDS service definition and generated stubs land in Phase 1
//! (SUM-63 / SUM-117 wire-protocol stability).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};

/// Semantic version of the wire protocol. Bumped when request/response shapes change.
pub const PROTOCOL_VERSION: &str = "0.1.0";

/// Response to a client handshake, letting a client verify daemon/protocol compatibility.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handshake {
    /// Protocol version the daemon speaks.
    pub protocol_version: String,
    /// Daemon build version.
    pub daemon_version: String,
}

impl Default for Handshake {
    fn default() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_reports_protocol_version() {
        let h = Handshake::default();
        assert_eq!(h.protocol_version, PROTOCOL_VERSION);
    }
}
