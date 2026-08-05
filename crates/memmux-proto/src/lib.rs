//! # memmux-proto
//!
//! The versioned client/daemon protocol contract (§16.1).
//!
//! The transport is a length-prefixed (`u32` big-endian length + JSON body) request/response
//! framing over the daemon's Unix domain socket. gRPC/Connect can replace the transport later
//! without changing these types (see the Phase-6 wire-protocol-stability story); `protoc` is not
//! required, keeping the build self-contained.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};

/// Semantic version of the wire protocol. Bumped when request/response shapes change.
pub const PROTOCOL_VERSION: &str = "0.1.0";

/// A request from a client to the daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Ask for daemon/protocol info.
    DaemonInfo,
    /// Create a new task.
    CreateTask(CreateTaskRequest),
    /// Fetch a single task by id.
    GetTask {
        /// Task id.
        id: String,
    },
    /// List all tasks.
    ListTasks,
    /// Terminate a task.
    TerminateTask {
        /// Task id.
        id: String,
    },
    /// Report current system memory pressure.
    SystemPressure,
    /// Read events after a cursor.
    ReadEvents {
        /// Return events with sequence greater than this.
        after_seq: u64,
        /// Maximum number of events.
        limit: u32,
    },
}

/// Fields needed to create a task (§16.2, trimmed to the Phase-1 surface).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateTaskRequest {
    /// Human-readable title / intent.
    pub title: String,
    /// Absolute path to the repository.
    pub repository_path: String,
    /// Provider slug (`claude-code`, `codex`, `gemini-cli`, `opencode`, `generic`).
    pub provider: String,
    /// Base branch to cut a worktree from.
    pub base_branch: String,
    /// Optional resource-class slug (`small`, `standard`, `browser-heavy`, `build-heavy`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_class: Option<String>,
    /// Optional priority slug (`low`, `normal`, `high`, `urgent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
}

/// A response from the daemon.
///
/// Adjacently tagged (`{"result": …, "data": …}`) so variants wrapping sequences (`Tasks`,
/// `Events`) serialize correctly, which internal tagging cannot do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum Response {
    /// Daemon/protocol information.
    DaemonInfo(DaemonInfo),
    /// A single task.
    Task(TaskView),
    /// A list of tasks.
    Tasks(Vec<TaskView>),
    /// Current pressure.
    Pressure(PressureView),
    /// A page of events.
    Events(Vec<EventView>),
    /// An error with a message.
    Error {
        /// Human-readable error.
        message: String,
    },
}

/// Daemon and protocol identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonInfo {
    /// Protocol version the daemon speaks.
    pub protocol_version: String,
    /// Daemon build version.
    pub daemon_version: String,
    /// Number of tasks the daemon is tracking.
    pub task_count: u64,
    /// The global agent memory budget in bytes.
    pub agent_budget_bytes: u64,
}

/// A client-facing view of a task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskView {
    /// Task id.
    pub id: String,
    /// Title / intent.
    pub title: String,
    /// Provider slug.
    pub provider: String,
    /// Current lifecycle state (SCREAMING_SNAKE_CASE).
    pub state: String,
    /// Owning repository id.
    pub repository: String,
    /// Base branch.
    pub base_branch: String,
    /// Creation time (ms since epoch).
    pub created_at_ms: u64,
    /// Last update time (ms since epoch).
    pub updated_at_ms: u64,
}

/// A snapshot of system memory pressure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PressureView {
    /// Agent memory budget in bytes.
    pub agent_budget_bytes: u64,
    /// Bytes currently attributed to managed tasks.
    pub used_bytes: u64,
    /// Utilization percent (0–100+, rounded).
    pub utilization_pct: u32,
    /// Pressure-ladder stage name.
    pub stage: String,
}

/// A client-facing view of an event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventView {
    /// Monotonic sequence.
    pub seq: u64,
    /// Owning task id, if any.
    pub task_id: Option<String>,
    /// Timestamp (ms since epoch).
    pub ts_ms: u64,
    /// Category.
    pub category: String,
    /// Concrete type.
    pub event_type: String,
    /// Severity.
    pub severity: String,
    /// Emitting component.
    pub source: String,
}

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

impl Request {
    /// Parse a request from a JSON string.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        let req = Request::CreateTask(CreateTaskRequest {
            title: "Refactor auth".into(),
            repository_path: "/src/product".into(),
            provider: "claude-code".into(),
            base_branch: "main".into(),
            resource_class: Some("standard".into()),
            priority: None,
        });
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(Request::from_json(&json).unwrap(), req);
    }

    #[test]
    fn response_round_trips() {
        let resp = Response::Error {
            message: "nope".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(back, resp);
    }

    #[test]
    fn response_with_sequence_variant_round_trips() {
        // Tasks wraps a Vec — this is exactly what internal tagging cannot serialize.
        let resp = Response::Tasks(vec![TaskView {
            id: "task_1".into(),
            title: "t".into(),
            provider: "codex".into(),
            state: "QUEUED".into(),
            repository: "repo_1".into(),
            base_branch: "main".into(),
            created_at_ms: 1,
            updated_at_ms: 2,
        }]);
        let json = serde_json::to_string(&resp).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(back, resp);
    }
}
