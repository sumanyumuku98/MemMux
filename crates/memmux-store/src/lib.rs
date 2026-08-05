//! # memmux-store
//!
//! The durable metadata store for the MemMux daemon (SUM-64, SUM-65, SUM-67 / §15).
//!
//! SQLite in WAL mode holds tasks, state transitions, an append-only event log with monotonic
//! sequence numbers (for incremental cursor readers), scheduler/lifecycle decisions with their
//! reasons + evidence, and resource samples. Large logs stay in chunk files elsewhere
//! (`memmux-pty`), never as database blobs.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use anyhow::Context;
use memmux_core::runtime::StateTransition;
use memmux_core::{Task, TaskSpec, TaskState};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tasks (
    id            TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL,
    spec_json     TEXT NOT NULL,
    state_json    TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS transitions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id    TEXT NOT NULL,
    from_state TEXT NOT NULL,
    to_state   TEXT NOT NULL,
    at_ms      INTEGER NOT NULL,
    reason     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_transitions_task ON transitions(task_id, id);
CREATE TABLE IF NOT EXISTS events (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id      TEXT,
    ts_ms        INTEGER NOT NULL,
    category     TEXT NOT NULL,
    event_type   TEXT NOT NULL,
    severity     TEXT NOT NULL,
    source       TEXT NOT NULL,
    payload_json TEXT
);
CREATE TABLE IF NOT EXISTS decisions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id       TEXT,
    ts_ms         INTEGER NOT NULL,
    action        TEXT NOT NULL,
    reason        TEXT NOT NULL,
    evidence_json TEXT
);
CREATE TABLE IF NOT EXISTS resource_samples (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms           INTEGER NOT NULL,
    task_id         TEXT,
    rss_bytes       INTEGER NOT NULL,
    accounted_bytes INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS checkpoints (
    task_id       TEXT PRIMARY KEY,
    created_at_ms INTEGER NOT NULL,
    artifact_path TEXT NOT NULL,
    integrity     TEXT NOT NULL
);
"#;

/// An event to append to the log (§15.2). The sequence number is assigned by the store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventInput {
    /// Owning task id, if any.
    pub task_id: Option<String>,
    /// Milliseconds since the Unix epoch.
    pub ts_ms: u64,
    /// Event category (e.g. `lifecycle`).
    pub category: String,
    /// Concrete event type (e.g. `runtime_recycled`).
    pub event_type: String,
    /// Severity.
    pub severity: String,
    /// Emitting component.
    pub source: String,
    /// Bounded JSON payload reference/summary.
    pub payload_json: Option<String>,
}

/// A stored event with its assigned monotonic sequence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEvent {
    /// Monotonic sequence number (cursor key).
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
    /// Source component.
    pub source: String,
    /// Payload reference/summary.
    pub payload_json: Option<String>,
}

/// An explainable control decision (SUM-67): reason + resource evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    /// Affected task id, if any.
    pub task_id: Option<String>,
    /// Timestamp (ms since epoch).
    pub ts_ms: u64,
    /// The action taken (e.g. `queued`, `recycled`, `terminated`).
    pub action: String,
    /// User-visible reason.
    pub reason: String,
    /// JSON snapshot of the metrics behind the decision.
    pub evidence_json: Option<String>,
}

/// A durable *reference* to a checkpoint (SUM-90). The full checkpoint JSON is an artifact on
/// disk; the store keeps only this pointer plus the integrity hash so a corrupt or missing
/// artifact is detectable before a resume is attempted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRef {
    /// Owning task id (one live checkpoint per task).
    pub task_id: String,
    /// When the checkpoint was captured (ms since epoch).
    pub created_at_ms: u64,
    /// Path to the checkpoint JSON artifact on disk.
    pub artifact_path: String,
    /// Integrity digest of the checkpoint contents (matches `Checkpoint::integrity`).
    pub integrity: String,
}

/// The durable store.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) a WAL-mode store at `path`.
    pub fn open(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let conn = Connection::open(path).context("open sqlite store")?;
        Self::init(conn)
    }

    /// Open an in-memory store (tests).
    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        // WAL gives safe concurrent read-while-write and crash resilience (SUM-64).
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA).context("apply schema")?;
        Ok(Self { conn })
    }

    /// Insert or update a task row.
    pub fn upsert_task(&self, task: &Task) -> anyhow::Result<()> {
        let spec_json = serde_json::to_string(&task.spec)?;
        let state_json = serde_json::to_string(&task.state)?;
        self.conn.execute(
            "INSERT INTO tasks (id, repository_id, spec_json, state_json, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                spec_json=excluded.spec_json,
                state_json=excluded.state_json,
                updated_at_ms=excluded.updated_at_ms",
            rusqlite::params![
                task.spec.id.as_str(),
                task.spec.repository.as_str(),
                spec_json,
                state_json,
                task.created_at_ms as i64,
                task.updated_at_ms as i64,
            ],
        )?;
        Ok(())
    }

    /// Append a state transition to a task's history.
    pub fn record_transition(&self, task_id: &str, t: &StateTransition) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO transitions (task_id, from_state, to_state, at_ms, reason)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                task_id,
                serde_json::to_string(&t.from)?,
                serde_json::to_string(&t.to)?,
                t.at_ms as i64,
                t.reason,
            ],
        )?;
        Ok(())
    }

    /// Load one task (with its transition history) by id.
    pub fn load_task(&self, id: &str) -> anyhow::Result<Option<Task>> {
        let row = self
            .conn
            .query_row(
                "SELECT spec_json, state_json, created_at_ms, updated_at_ms FROM tasks WHERE id=?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((spec_json, state_json, created, updated)) = row else {
            return Ok(None);
        };
        let spec: TaskSpec = serde_json::from_str(&spec_json)?;
        let state: TaskState = serde_json::from_str(&state_json)?;
        let history = self.load_history(id)?;
        Ok(Some(Task {
            spec,
            state,
            created_at_ms: created as u64,
            updated_at_ms: updated as u64,
            history,
        }))
    }

    fn load_history(&self, task_id: &str) -> anyhow::Result<Vec<StateTransition>> {
        let mut stmt = self.conn.prepare(
            "SELECT from_state, to_state, at_ms, reason FROM transitions WHERE task_id=?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([task_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut history = Vec::new();
        for row in rows {
            let (from, to, at_ms, reason) = row?;
            history.push(StateTransition {
                from: serde_json::from_str(&from)?,
                to: serde_json::from_str(&to)?,
                at_ms: at_ms as u64,
                reason,
            });
        }
        Ok(history)
    }

    /// Load every task — the basis of crash recovery (SUM-66).
    pub fn load_tasks(&self) -> anyhow::Result<Vec<Task>> {
        let ids: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM tasks ORDER BY created_at_ms")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut tasks = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(task) = self.load_task(&id)? {
                tasks.push(task);
            }
        }
        Ok(tasks)
    }

    /// Append an event, returning its assigned sequence number.
    pub fn append_event(&self, ev: &EventInput) -> anyhow::Result<u64> {
        self.conn.execute(
            "INSERT INTO events (task_id, ts_ms, category, event_type, severity, source, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                ev.task_id,
                ev.ts_ms as i64,
                ev.category,
                ev.event_type,
                ev.severity,
                ev.source,
                ev.payload_json,
            ],
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    /// Read up to `limit` events with sequence greater than `after_seq` (cursor semantics).
    pub fn read_events_after(
        &self,
        after_seq: u64,
        limit: usize,
    ) -> anyhow::Result<Vec<StoredEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, task_id, ts_ms, category, event_type, severity, source, payload_json
             FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![after_seq as i64, limit as i64],
            Self::map_event,
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The most recent `limit` events (newest first) — used by the support bundle.
    pub fn recent_events(&self, limit: usize) -> anyhow::Result<Vec<StoredEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, task_id, ts_ms, category, event_type, severity, source, payload_json
             FROM events ORDER BY seq DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], Self::map_event)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn map_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<StoredEvent> {
        Ok(StoredEvent {
            seq: r.get::<_, i64>(0)? as u64,
            task_id: r.get(1)?,
            ts_ms: r.get::<_, i64>(2)? as u64,
            category: r.get(3)?,
            event_type: r.get(4)?,
            severity: r.get(5)?,
            source: r.get(6)?,
            payload_json: r.get(7)?,
        })
    }

    /// Record an explainable control decision (SUM-67).
    pub fn record_decision(&self, d: &Decision) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO decisions (task_id, ts_ms, action, reason, evidence_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                d.task_id,
                d.ts_ms as i64,
                d.action,
                d.reason,
                d.evidence_json
            ],
        )?;
        Ok(())
    }

    /// Record a resource sample aggregate.
    pub fn record_sample(
        &self,
        ts_ms: u64,
        task_id: Option<&str>,
        rss_bytes: u64,
        accounted_bytes: u64,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO resource_samples (ts_ms, task_id, rss_bytes, accounted_bytes) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![ts_ms as i64, task_id, rss_bytes as i64, accounted_bytes as i64],
        )?;
        Ok(())
    }

    /// Count of tasks (diagnostics).
    pub fn task_count(&self) -> anyhow::Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get::<_, i64>(0))? as u64)
    }

    /// Insert or replace the checkpoint reference for a task (SUM-90).
    pub fn save_checkpoint_ref(&self, r: &CheckpointRef) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO checkpoints (task_id, created_at_ms, artifact_path, integrity)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(task_id) DO UPDATE SET
                created_at_ms=excluded.created_at_ms,
                artifact_path=excluded.artifact_path,
                integrity=excluded.integrity",
            rusqlite::params![
                r.task_id,
                r.created_at_ms as i64,
                r.artifact_path,
                r.integrity
            ],
        )?;
        Ok(())
    }

    /// Load a task's checkpoint reference, if one exists.
    pub fn load_checkpoint_ref(&self, task_id: &str) -> anyhow::Result<Option<CheckpointRef>> {
        Ok(self
            .conn
            .query_row(
                "SELECT task_id, created_at_ms, artifact_path, integrity
                 FROM checkpoints WHERE task_id = ?1",
                [task_id],
                |row| {
                    Ok(CheckpointRef {
                        task_id: row.get(0)?,
                        created_at_ms: row.get::<_, i64>(1)? as u64,
                        artifact_path: row.get(2)?,
                        integrity: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// Drop a task's checkpoint reference (after a successful resume or termination).
    pub fn delete_checkpoint_ref(&self, task_id: &str) -> anyhow::Result<()> {
        self.conn
            .execute("DELETE FROM checkpoints WHERE task_id = ?1", [task_id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memmux_core::Provider;

    fn task(id: &str) -> Task {
        Task::new(
            TaskSpec::new(
                id,
                "repo_1",
                "/src",
                "Do work",
                Provider::ClaudeCode,
                "main",
            ),
            1000,
        )
    }

    #[test]
    fn upsert_and_load_task_round_trips() {
        let store = Store::open_in_memory().unwrap();
        let mut t = task("task_1");
        t.transition(TaskState::Queued, "submitted", 1100).unwrap();
        store.upsert_task(&t).unwrap();
        store
            .record_transition(t.id().as_str(), &t.history[0])
            .unwrap();

        let loaded = store.load_task("task_1").unwrap().unwrap();
        assert_eq!(loaded.state, TaskState::Queued);
        assert_eq!(loaded.spec.title, "Do work");
        assert_eq!(loaded.history.len(), 1);
        assert_eq!(loaded.history[0].to, TaskState::Queued);
    }

    #[test]
    fn checkpoint_ref_saves_loads_and_deletes() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.load_checkpoint_ref("task_1").unwrap().is_none());

        let r = CheckpointRef {
            task_id: "task_1".into(),
            created_at_ms: 1234,
            artifact_path: "/root/tasks/task_1/checkpoint.json".into(),
            integrity: "deadbeefdeadbeef".into(),
        };
        store.save_checkpoint_ref(&r).unwrap();
        assert_eq!(store.load_checkpoint_ref("task_1").unwrap().unwrap(), r);

        // Upsert replaces in place (one live checkpoint per task).
        let r2 = CheckpointRef {
            created_at_ms: 5678,
            integrity: "cafef00dcafef00d".into(),
            ..r.clone()
        };
        store.save_checkpoint_ref(&r2).unwrap();
        assert_eq!(store.load_checkpoint_ref("task_1").unwrap().unwrap(), r2);

        store.delete_checkpoint_ref("task_1").unwrap();
        assert!(store.load_checkpoint_ref("task_1").unwrap().is_none());
    }

    #[test]
    fn load_tasks_returns_all_for_recovery() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_task(&task("task_1")).unwrap();
        store.upsert_task(&task("task_2")).unwrap();
        assert_eq!(store.task_count().unwrap(), 2);
        let tasks = store.load_tasks().unwrap();
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn events_have_monotonic_cursor_semantics() {
        let store = Store::open_in_memory().unwrap();
        let mk = |t: &str| EventInput {
            task_id: Some("task_1".into()),
            ts_ms: 1,
            category: "lifecycle".into(),
            event_type: t.into(),
            severity: "info".into(),
            source: "test".into(),
            payload_json: None,
        };
        let s1 = store.append_event(&mk("started")).unwrap();
        let s2 = store.append_event(&mk("idle")).unwrap();
        assert!(s2 > s1);

        let after_first = store.read_events_after(s1, 10).unwrap();
        assert_eq!(after_first.len(), 1);
        assert_eq!(after_first[0].event_type, "idle");
        assert_eq!(after_first[0].seq, s2);

        assert_eq!(store.read_events_after(0, 10).unwrap().len(), 2);
        assert!(store.read_events_after(s2, 10).unwrap().is_empty());
    }

    #[test]
    fn decisions_are_recorded_with_reason_and_evidence() {
        let store = Store::open_in_memory().unwrap();
        store
            .record_decision(&Decision {
                task_id: Some("task_1".into()),
                ts_ms: 5,
                action: "queued".into(),
                reason: "insufficient headroom".into(),
                evidence_json: Some(r#"{"needed_mib":8192,"free_mib":2048}"#.into()),
            })
            .unwrap();
        // Recorded rows are queryable back (smoke via recent_events is separate; here just count).
        let n: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM decisions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn wal_store_persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("memmux-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        {
            let store = Store::open(&path).unwrap();
            store.upsert_task(&task("task_persist")).unwrap();
        }
        // Reopen (simulating a daemon restart) and confirm the task survived.
        let store = Store::open(&path).unwrap();
        assert!(store.load_task("task_persist").unwrap().is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
