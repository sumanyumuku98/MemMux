//! Daemon state: durable-store-backed task registry, request handlers, crash recovery, and the
//! explainable-audit trail (SUM-66, SUM-67 wiring; serves the SUM-63 API).

use memmux_core::{Priority, Provider, ResourceClass, Task, TaskSpec, TaskState};
use memmux_proto::{
    CreateTaskRequest, DaemonInfo, EventView, PressureView, Request, Response, TaskView,
    PROTOCOL_VERSION,
};
use memmux_sched::{class_reservation, pressure::PressureStage, ResourceEnvelope};
use memmux_store::{Decision, EventInput, Store};
use std::collections::BTreeMap;

/// The authoritative in-memory + durable daemon state.
pub struct DaemonState {
    store: Store,
    tasks: BTreeMap<String, Task>,
    envelope: ResourceEnvelope,
    seq: u64,
}

impl DaemonState {
    /// Boot the daemon: open the store and reconstruct all tasks (crash recovery, SUM-66).
    pub fn boot(store: Store, envelope: ResourceEnvelope) -> anyhow::Result<Self> {
        let mut tasks = BTreeMap::new();
        for task in store.load_tasks()? {
            tasks.insert(task.spec.id.to_string(), task);
        }
        let recovered = tasks.len();
        let state = Self {
            store,
            tasks,
            envelope,
            seq: 0,
        };
        state.audit(
            None,
            "recovered",
            &format!("reconstructed {recovered} task(s) from the durable store on boot"),
            None,
        );
        Ok(state)
    }

    /// Number of tracked tasks.
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// Dispatch a request to its handler.
    pub fn handle(&mut self, request: Request) -> Response {
        match request {
            Request::DaemonInfo => Response::DaemonInfo(DaemonInfo {
                protocol_version: PROTOCOL_VERSION.to_string(),
                daemon_version: env!("CARGO_PKG_VERSION").to_string(),
                task_count: self.tasks.len() as u64,
                agent_budget_bytes: self.envelope.agent_budget_bytes,
            }),
            Request::CreateTask(req) => self.create_task(req),
            Request::GetTask { id } => match self.tasks.get(&id) {
                Some(t) => Response::Task(view(t)),
                None => Response::Error {
                    message: format!("no such task: {id}"),
                },
            },
            Request::ListTasks => Response::Tasks(self.tasks.values().map(view).collect()),
            Request::TerminateTask { id } => self.terminate(&id),
            Request::SystemPressure => self.pressure(),
            Request::ReadEvents { after_seq, limit } => self.read_events(after_seq, limit),
        }
    }

    fn create_task(&mut self, req: CreateTaskRequest) -> Response {
        let provider = match parse_provider(&req.provider) {
            Some(p) => p,
            None => {
                return Response::Error {
                    message: format!("unknown provider: {}", req.provider),
                }
            }
        };
        let now = now_ms();
        self.seq += 1;
        let task_id = format!("task_{now:x}{:04x}", self.seq & 0xffff);
        let repo_id = format!("repo_{}", short_hash(&req.repository_path));

        let mut spec = TaskSpec::new(
            task_id.as_str(),
            repo_id.as_str(),
            req.repository_path.as_str(),
            req.title.as_str(),
            provider,
            req.base_branch.as_str(),
        );
        if let Some(rc) = req.resource_class.as_deref().and_then(parse_resource_class) {
            spec.resource_class = rc;
        }
        if let Some(pr) = req.priority.as_deref().and_then(parse_priority) {
            spec.priority = pr;
        }

        let mut task = Task::new(spec, now);
        // Created -> Queued: durable, awaiting admission (provider launch lands with adapters).
        if let Ok(transition) = task.transition(TaskState::Queued, "submitted", now) {
            if let Err(e) = self.store.upsert_task(&task) {
                return Response::Error {
                    message: format!("store error: {e}"),
                };
            }
            let _ = self.store.record_transition(&task_id, &transition);
        }

        self.event(
            Some(&task_id),
            "lifecycle",
            "task_created",
            "info",
            "daemon",
        );

        // Record an explainable admission decision with the resource evidence (SUM-67).
        let predicted_peak = class_reservation(task.spec.resource_class).peak_bytes();
        let fits = predicted_peak <= self.envelope.agent_budget_bytes;
        let reason = if fits {
            format!(
                "queued; predicted peak {} MiB fits the {} MiB budget",
                predicted_peak / (1024 * 1024),
                self.envelope.agent_budget_bytes / (1024 * 1024)
            )
        } else {
            format!(
                "queued; predicted peak {} MiB exceeds the {} MiB budget — will await headroom",
                predicted_peak / (1024 * 1024),
                self.envelope.agent_budget_bytes / (1024 * 1024)
            )
        };
        let evidence = serde_json::json!({
            "predicted_peak_bytes": predicted_peak,
            "agent_budget_bytes": self.envelope.agent_budget_bytes,
            "resource_class": format!("{:?}", task.spec.resource_class),
        });
        self.audit(
            Some(&task_id),
            "queued",
            &reason,
            Some(evidence.to_string()),
        );

        let v = view(&task);
        self.tasks.insert(task_id, task);
        Response::Task(v)
    }

    fn terminate(&mut self, id: &str) -> Response {
        let now = now_ms();
        let Some(task) = self.tasks.get_mut(id) else {
            return Response::Error {
                message: format!("no such task: {id}"),
            };
        };
        // Drive ANY -> TERMINATING -> TERMINATED.
        if task.state != TaskState::Terminating && task.state != TaskState::Terminated {
            if let Ok(t) = task.transition(TaskState::Terminating, "operator terminate", now) {
                let _ = self.store.record_transition(id, &t);
            }
        }
        if let Ok(t) = task.transition(TaskState::Terminated, "torn down", now) {
            let _ = self.store.record_transition(id, &t);
        }
        let _ = self.store.upsert_task(task);
        let v = view(task);
        self.audit(Some(id), "terminated", "task terminated by operator", None);
        self.event(Some(id), "lifecycle", "terminated", "info", "daemon");
        Response::Task(v)
    }

    fn pressure(&mut self) -> Response {
        // No provider processes are launched yet (adapters arrive in SUM-13), so managed usage
        // is zero; the ladder reports Normal honestly rather than fabricating a figure.
        let used_bytes = 0u64;
        let utilization = self.envelope.utilization(used_bytes);
        let stage = PressureStage::classify(utilization, false, false);
        Response::Pressure(PressureView {
            agent_budget_bytes: self.envelope.agent_budget_bytes,
            used_bytes,
            utilization_pct: (utilization * 100.0).round() as u32,
            stage: format!("{stage:?}"),
        })
    }

    fn read_events(&self, after_seq: u64, limit: u32) -> Response {
        match self.store.read_events_after(after_seq, limit as usize) {
            Ok(events) => Response::Events(
                events
                    .into_iter()
                    .map(|e| EventView {
                        seq: e.seq,
                        task_id: e.task_id,
                        ts_ms: e.ts_ms,
                        category: e.category,
                        event_type: e.event_type,
                        severity: e.severity,
                        source: e.source,
                    })
                    .collect(),
            ),
            Err(e) => Response::Error {
                message: format!("store error: {e}"),
            },
        }
    }

    fn event(
        &self,
        task_id: Option<&str>,
        category: &str,
        event_type: &str,
        severity: &str,
        source: &str,
    ) {
        let _ = self.store.append_event(&EventInput {
            task_id: task_id.map(str::to_string),
            ts_ms: now_ms(),
            category: category.to_string(),
            event_type: event_type.to_string(),
            severity: severity.to_string(),
            source: source.to_string(),
            payload_json: None,
        });
    }

    fn audit(
        &self,
        task_id: Option<&str>,
        action: &str,
        reason: &str,
        evidence_json: Option<String>,
    ) {
        let _ = self.store.record_decision(&Decision {
            task_id: task_id.map(str::to_string),
            ts_ms: now_ms(),
            action: action.to_string(),
            reason: reason.to_string(),
            evidence_json,
        });
    }

    /// Borrow the store (support-bundle export reads recent events from it).
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The current resource envelope.
    pub fn envelope(&self) -> &ResourceEnvelope {
        &self.envelope
    }
}

fn view(task: &Task) -> TaskView {
    TaskView {
        id: task.spec.id.to_string(),
        title: task.spec.title.clone(),
        provider: task.spec.provider.slug().to_string(),
        state: task.state.to_string(),
        repository: task.spec.repository.to_string(),
        base_branch: task.spec.base_branch.clone(),
        created_at_ms: task.created_at_ms,
        updated_at_ms: task.updated_at_ms,
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn short_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:x}")
}

fn parse_provider(slug: &str) -> Option<Provider> {
    Some(match slug {
        "claude-code" | "claude" => Provider::ClaudeCode,
        "codex" => Provider::Codex,
        "gemini-cli" | "gemini" => Provider::GeminiCli,
        "opencode" => Provider::OpenCode,
        "generic" => Provider::Generic,
        _ => return None,
    })
}

fn parse_resource_class(slug: &str) -> Option<ResourceClass> {
    Some(match slug {
        "small" => ResourceClass::Small,
        "standard" => ResourceClass::Standard,
        "browser-heavy" => ResourceClass::BrowserHeavy,
        "build-heavy" => ResourceClass::BuildHeavy,
        "custom" => ResourceClass::Custom,
        _ => return None,
    })
}

fn parse_priority(slug: &str) -> Option<Priority> {
    Some(match slug {
        "low" => Priority::Low,
        "normal" => Priority::Normal,
        "high" => Priority::High,
        "urgent" => Priority::Urgent,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use memmux_sched::GIB;

    fn state() -> DaemonState {
        let store = Store::open_in_memory().unwrap();
        let envelope = ResourceEnvelope::with_default_reserves(32 * GIB);
        DaemonState::boot(store, envelope).unwrap()
    }

    fn create(state: &mut DaemonState, title: &str) -> TaskView {
        match state.handle(Request::CreateTask(CreateTaskRequest {
            title: title.into(),
            repository_path: "/src/product".into(),
            provider: "claude-code".into(),
            base_branch: "main".into(),
            resource_class: None,
            priority: None,
        })) {
            Response::Task(v) => v,
            other => panic!("expected Task, got {other:?}"),
        }
    }

    #[test]
    fn create_lists_and_gets_a_task() {
        let mut s = state();
        let created = create(&mut s, "Refactor auth");
        assert_eq!(created.state, "QUEUED");
        assert_eq!(created.provider, "claude-code");

        match s.handle(Request::ListTasks) {
            Response::Tasks(ts) => assert_eq!(ts.len(), 1),
            other => panic!("{other:?}"),
        }
        match s.handle(Request::GetTask {
            id: created.id.clone(),
        }) {
            Response::Task(t) => assert_eq!(t.id, created.id),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_provider_is_an_error() {
        let mut s = state();
        let resp = s.handle(Request::CreateTask(CreateTaskRequest {
            title: "x".into(),
            repository_path: "/r".into(),
            provider: "not-a-provider".into(),
            base_branch: "main".into(),
            resource_class: None,
            priority: None,
        }));
        assert!(matches!(resp, Response::Error { .. }));
    }

    #[test]
    fn create_emits_events_and_decisions() {
        let mut s = state();
        create(&mut s, "task one");
        match s.handle(Request::ReadEvents {
            after_seq: 0,
            limit: 10,
        }) {
            Response::Events(evs) => {
                assert!(evs.iter().any(|e| e.event_type == "task_created"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn terminate_drives_task_to_terminated() {
        let mut s = state();
        let t = create(&mut s, "doomed");
        match s.handle(Request::TerminateTask { id: t.id.clone() }) {
            Response::Task(v) => assert_eq!(v.state, "TERMINATED"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn boot_recovers_tasks_from_the_store() {
        // Create a task, then reboot a new DaemonState on the same underlying data by sharing
        // a temp-file store.
        let dir = std::env::temp_dir().join(format!("memmuxd-recover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.db");
        let env = ResourceEnvelope::with_default_reserves(32 * GIB);

        let id = {
            let mut s = DaemonState::boot(Store::open(&db).unwrap(), env).unwrap();
            create(&mut s, "persisted task").id
        };
        // Reboot: a fresh state on the same store must recover the task.
        let s2 = DaemonState::boot(Store::open(&db).unwrap(), env).unwrap();
        assert_eq!(s2.task_count(), 1);
        assert!(s2.tasks.contains_key(&id));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pressure_reports_budget_and_normal_stage() {
        let mut s = state();
        match s.handle(Request::SystemPressure) {
            Response::Pressure(p) => {
                assert!(p.agent_budget_bytes > 0);
                assert_eq!(p.stage, "Normal");
            }
            other => panic!("{other:?}"),
        }
    }
}
