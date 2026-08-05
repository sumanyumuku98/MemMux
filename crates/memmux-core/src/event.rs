//! Event model (§15.1 / §15.2).
//!
//! Events are the audit and observability backbone. Payloads are intentionally *not* inlined
//! here — the spec mandates a `bounded_payload_ref` so a single event can never balloon the
//! event store. The concrete payload store lands with the daemon (Phase 1, SUM-64/65).

use crate::ids::{RepositoryId, RuntimeInstanceId, TaskId};
use serde::{Deserialize, Serialize};

/// Severity of an event, ordered from least to most urgent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventSeverity {
    /// Fine-grained diagnostic detail.
    Debug,
    /// Normal operational event.
    Info,
    /// Something noteworthy that is not yet an error.
    Warn,
    /// An error occurred.
    Error,
    /// Critical condition requiring immediate attention.
    Critical,
}

/// High-level event category (§15.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventCategory {
    /// Task lifecycle transitions.
    Lifecycle,
    /// Resource sampling and reservation changes.
    Resource,
    /// Process spawn/exit/escape/reclaim events.
    Process,
    /// Git worktree and integration events.
    Git,
    /// Tool/MCP lease and request events.
    Tool,
    /// Memory-optimization interventions.
    Optimization,
    /// Security-relevant events.
    Security,
}

/// Concrete event types grouped by category (§15.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    // Lifecycle
    /// A task record was created.
    TaskCreated,
    /// A task was admitted for execution.
    Admitted,
    /// A runtime instance started.
    Started,
    /// A task went idle.
    Idle,
    /// A task was checkpointed.
    Checkpointed,
    /// A task was hibernated.
    Hibernated,
    /// A task was resumed.
    Resumed,
    /// A provider runtime was recycled.
    Recycled,
    /// A task was terminated.
    Terminated,

    // Resource
    /// A resource sample was recorded.
    Sample,
    /// A reservation changed.
    ReservationChanged,
    /// The pressure ladder stage changed.
    PressureStageChanged,
    /// A configured limit was exceeded.
    LimitExceeded,

    // Process
    /// A descendant process was spawned.
    ProcessSpawned,
    /// A descendant process exited.
    ProcessExited,
    /// A process escaped task ownership.
    ProcessEscaped,
    /// A descendant process was reclaimed.
    ProcessReclaimed,
    /// A descendant was reclassified.
    ProcessReclassified,

    // Git
    /// A worktree was created.
    WorktreeCreated,
    /// The dirty manifest of a worktree changed.
    DirtyChanged,
    /// A conflict-risk signal was raised.
    ConflictRisk,
    /// A task's changes were merged.
    Merged,
    /// A task was archived.
    Archived,

    // Tool
    /// A tool lease was opened.
    LeaseOpened,
    /// A tool request started.
    RequestStarted,
    /// A tool response was truncated to a bounded summary.
    ResponseTruncated,
    /// A tool lease expired.
    LeaseExpired,

    // Optimization
    /// A terminal buffer was trimmed.
    BufferTrimmed,
    /// A transcript was compacted.
    TranscriptCompacted,
    /// A service was pooled.
    ServicePooled,
    /// Worker concurrency was reduced.
    WorkerReduced,
    /// Bytes were reclaimed by an optimization.
    BytesReclaimed,

    // Security
    /// A capability was denied.
    CapabilityDenied,
    /// A secret was redacted before persistence.
    SecretRedacted,
    /// A plugin violated its capability grant.
    PluginViolation,
}

impl EventType {
    /// The category this event type belongs to.
    pub fn category(self) -> EventCategory {
        use EventType::*;
        match self {
            TaskCreated | Admitted | Started | Idle | Checkpointed | Hibernated | Resumed
            | Recycled | Terminated => EventCategory::Lifecycle,
            Sample | ReservationChanged | PressureStageChanged | LimitExceeded => {
                EventCategory::Resource
            }
            ProcessSpawned | ProcessExited | ProcessEscaped | ProcessReclaimed
            | ProcessReclassified => EventCategory::Process,
            WorktreeCreated | DirtyChanged | ConflictRisk | Merged | Archived => EventCategory::Git,
            LeaseOpened | RequestStarted | ResponseTruncated | LeaseExpired => EventCategory::Tool,
            BufferTrimmed | TranscriptCompacted | ServicePooled | WorkerReduced
            | BytesReclaimed => EventCategory::Optimization,
            CapabilityDenied | SecretRedacted | PluginViolation => EventCategory::Security,
        }
    }
}

/// A single bounded event (§15.1).
///
/// The heavy payload lives out of line behind `bounded_payload_ref`; this record stays small
/// enough that the event store never grows proportional to output volume.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Globally unique event id.
    pub event_id: String,
    /// Monotonic per-task sequence number (drives incremental transcript cursors).
    pub sequence: u64,
    /// Milliseconds since the Unix epoch.
    pub timestamp_ms: u64,
    /// Owning repository, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
    /// Owning task, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// Owning runtime instance, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_instance_id: Option<RuntimeInstanceId>,
    /// Concrete event type.
    pub event_type: EventType,
    /// Severity.
    pub severity: EventSeverity,
    /// Free-form source label (component that emitted the event).
    pub source: String,
    /// Reference to the bounded payload artifact, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounded_payload_ref: Option<String>,
    /// Id of the event that caused this one, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    /// Correlation id grouping related events, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_types_map_to_expected_categories() {
        assert_eq!(EventType::TaskCreated.category(), EventCategory::Lifecycle);
        assert_eq!(EventType::Sample.category(), EventCategory::Resource);
        assert_eq!(EventType::ProcessEscaped.category(), EventCategory::Process);
        assert_eq!(
            EventType::BytesReclaimed.category(),
            EventCategory::Optimization
        );
        assert_eq!(
            EventType::CapabilityDenied.category(),
            EventCategory::Security
        );
    }

    #[test]
    fn severity_orders_debug_to_critical() {
        assert!(EventSeverity::Debug < EventSeverity::Info);
        assert!(EventSeverity::Warn < EventSeverity::Error);
        assert!(EventSeverity::Error < EventSeverity::Critical);
    }

    #[test]
    fn event_round_trips_and_omits_none_fields() {
        let ev = Event {
            event_id: "ev_1".into(),
            sequence: 42,
            timestamp_ms: 1_722_800_000_000,
            repository_id: None,
            task_id: Some(TaskId::new("task_1")),
            runtime_instance_id: None,
            event_type: EventType::Started,
            severity: EventSeverity::Info,
            source: "lifecycle".into(),
            bounded_payload_ref: None,
            causation_id: None,
            correlation_id: None,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(!json.contains("repository_id"));
        assert!(json.contains("\"task_id\":\"task_1\""));
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, back);
    }
}
