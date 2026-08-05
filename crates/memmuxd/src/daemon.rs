//! Daemon state: durable-store-backed task registry, request handlers, crash recovery, and the
//! explainable-audit trail (SUM-66, SUM-67 wiring; serves the SUM-63 API).

use memmux_adapters::capabilities::ResumeFidelity;
use memmux_adapters::{adapter_for, CapabilityGrant, LaunchSpec, ProviderAdapter, RuntimeInstance};
use memmux_core::{Priority, Provider, ResourceClass, Task, TaskSpec, TaskState};
use memmux_lifecycle::{
    assess, ActivitySnapshot, Checkpoint, Reclamation, RecyclePolicy, RecycleRecord, ResumeMode,
    ResumeOutcome, SafePoint,
};
use memmux_proto::{
    CreateTaskRequest, DaemonInfo, EventView, HistoryPage, PressureView, Request, Response,
    ScreenView, TaskView, PROTOCOL_VERSION,
};
use memmux_pty::ChunkStore;
use memmux_sched::{class_reservation, pressure::PressureStage, ResourceEnvelope};
use memmux_store::{CheckpointRef, Decision, EventInput, Store};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

/// Lines per history chunk file.
const HISTORY_CHUNK_LINES: usize = 500;

/// How often the pump checks running providers against their RSS recycle threshold (SUM-94).
const RECYCLE_CHECK_INTERVAL_MS: u64 = 5_000;

/// Grace period for verifying a provider's process subtree is gone during recycle (SUM-95).
const TREE_VERIFY_GRACE: Duration = Duration::from_millis(300);

/// The authoritative in-memory + durable daemon state.
pub struct DaemonState {
    store: Store,
    tasks: BTreeMap<String, Task>,
    envelope: ResourceEnvelope,
    root: PathBuf,
    /// Live provider processes, keyed by task id.
    runtimes: HashMap<String, RuntimeInstance>,
    /// Per-task scrollback history (evicted capture lines).
    histories: HashMap<String, ChunkStore>,
    /// Last time the pump checked running providers against their recycle threshold.
    last_recycle_check_ms: u64,
    seq: u64,
}

impl DaemonState {
    /// Boot the daemon: open the store and reconstruct all tasks (crash recovery, SUM-66).
    pub fn boot(store: Store, envelope: ResourceEnvelope, root: PathBuf) -> anyhow::Result<Self> {
        let mut tasks = BTreeMap::new();
        for task in store.load_tasks()? {
            tasks.insert(task.spec.id.to_string(), task);
        }
        let recovered = tasks.len();
        let state = Self {
            store,
            tasks,
            envelope,
            root,
            runtimes: HashMap::new(),
            histories: HashMap::new(),
            last_recycle_check_ms: 0,
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
            Request::HibernateTask { id } => self.hibernate(&id),
            Request::ResumeTask { id } => self.resume(&id),
            Request::RecycleTask { id } => self.recycle(&id),
            Request::SystemPressure => self.pressure(),
            Request::ReadEvents { after_seq, limit } => self.read_events(after_seq, limit),
            Request::StartTask { id } => self.start_task(&id),
            Request::GetScreen { id } => match self.screen_view(&id) {
                Some(sv) => Response::Screen(sv),
                None => Response::Error {
                    message: format!("task not running: {id}"),
                },
            },
            Request::ReadHistory { id, cursor, limit } => self.read_history(&id, cursor, limit),
            // Attach is intercepted by the server (it needs the raw connection); reaching here
            // means it was mis-dispatched.
            Request::Attach { .. } => Response::Error {
                message: "attach must be handled as a stream".to_string(),
            },
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
        spec.command = req.command.clone();

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
        if !self.tasks.contains_key(id) {
            return Response::Error {
                message: format!("no such task: {id}"),
            };
        }

        // Kill and reap the live provider process, if any, preserving its history.
        if let Some(mut rt) = self.runtimes.remove(id) {
            let _ = rt.stop();
            if let Some(hist) = self.histories.get_mut(id) {
                let _ = hist.append(rt.take_evicted());
                let _ = hist.flush();
            }
        }

        let task = self.tasks.get_mut(id).expect("checked above");
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
        // Per-task provider RSS sampling into the pressure figure is a Phase-3 concern; until
        // then managed usage is reported as zero rather than fabricating a number.
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

    /// Admit and launch a task's provider in a PTY (Queued → Admitting → Starting → Active).
    fn start_task(&mut self, id: &str) -> Response {
        let now = now_ms();
        if self.runtimes.contains_key(id) {
            return match self.tasks.get(id) {
                Some(t) => Response::Task(view(t)),
                None => Response::Error {
                    message: format!("no such task: {id}"),
                },
            };
        }
        let Some(task) = self.tasks.get_mut(id) else {
            return Response::Error {
                message: format!("no such task: {id}"),
            };
        };
        if task.state != TaskState::Queued {
            return Response::Error {
                message: format!("task {id} is not startable from {}", task.state),
            };
        }

        let (adapter, spec) = launch_spec_for(task);

        for (to, reason) in [
            (TaskState::Admitting, "admitted: fits budget"),
            (TaskState::Starting, "launching provider"),
        ] {
            if let Ok(t) = task.transition(to, reason, now) {
                let _ = self.store.record_transition(id, &t);
            }
        }

        match RuntimeInstance::launch(adapter.as_ref(), &spec, now) {
            Ok(runtime) => {
                if let Ok(t) = task.transition(TaskState::Active, "provider running", now) {
                    let _ = self.store.record_transition(id, &t);
                }
                let _ = self.store.upsert_task(task);
                let v = view(task);
                self.install_runtime(id, runtime);
                self.event(Some(id), "lifecycle", "started", "info", "daemon");
                self.audit(Some(id), "started", "provider launched in a PTY", None);
                Response::Task(v)
            }
            Err(e) => {
                if let Ok(t) = task.transition(TaskState::Failed, "launch failed", now) {
                    let _ = self.store.record_transition(id, &t);
                }
                let _ = self.store.upsert_task(task);
                self.audit(
                    Some(id),
                    "failed",
                    &format!("provider launch failed: {e}"),
                    None,
                );
                Response::Error {
                    message: format!("launch failed: {e}"),
                }
            }
        }
    }

    /// Install a live runtime and ensure its history store exists (reused across resume so the
    /// transcript cursor stays continuous).
    fn install_runtime(&mut self, id: &str, rt: RuntimeInstance) {
        self.runtimes.insert(id.to_string(), rt);
        if !self.histories.contains_key(id) {
            let hist_dir = self.root.join("tasks").join(id).join("history");
            if let Ok(store) = ChunkStore::new(&hist_dir, HISTORY_CHUNK_LINES) {
                self.histories.insert(id.to_string(), store);
            }
        }
    }

    /// Stop a task's live provider, flushing its scrollback and (best-effort) verifying its whole
    /// process subtree is gone (SUM-95). Returns a human-readable cleanup note.
    fn shutdown_runtime(&mut self, id: &str) -> String {
        let Some(mut rt) = self.runtimes.remove(id) else {
            return "no live provider".into();
        };
        let pid = rt.pid();
        let _ = rt.stop();
        if let Some(hist) = self.histories.get_mut(id) {
            let _ = hist.append(rt.take_evicted());
            let _ = hist.flush();
        }
        match pid {
            Some(pid) => verify_subtree_gone(pid).1,
            None => "no pid to verify".into(),
        }
    }

    /// The activity snapshot used for safe-point detection. Sub-state comes from the task's last
    /// classified state; the daemon is conservative (no continuous mid-write signal, so `writing`
    /// is false and `assess` relies on the state machine).
    fn activity(&self, task: &Task) -> ActivitySnapshot {
        ActivitySnapshot {
            sub_state: task.state,
            tool_running: task.state == TaskState::ToolRunning,
            writing: false,
        }
    }

    /// Capture a checkpoint of a live task (§13.1 / SUM-90): repo state, transcript cursor, RSS
    /// baseline, and the provider's declared secret references.
    fn capture_checkpoint(&self, id: &str, task: &Task, now: u64) -> Checkpoint {
        let repo = std::path::Path::new(&task.spec.repository_path);
        let git_head = git_head(repo).unwrap_or_default();
        let git_patch_hash = git_patch_hash(repo);
        let cursor = self.histories.get(id).map(|h| h.total_lines()).unwrap_or(0);
        let rss = self
            .runtimes
            .get(id)
            .and_then(|rt| rt.pid())
            .map(sample_subtree_rss)
            .unwrap_or(0);
        let secret_refs = adapter_for(task.spec.provider).secret_refs();
        // Session-ref capture is provider-specific (a claude session id, etc.) and lands with
        // deeper per-provider integration; until then we checkpoint without one, so resume uses
        // the reconstructed path rather than claiming a native resume we can't yet drive.
        Checkpoint::new(
            id,
            git_head,
            git_patch_hash,
            cursor,
            None,
            rss,
            secret_refs,
            now,
        )
    }

    /// Persist a checkpoint as a disk artifact plus a durable reference in the store (SUM-90).
    fn persist_checkpoint(&self, cp: &Checkpoint) -> anyhow::Result<()> {
        let dir = self.root.join("tasks").join(&cp.task_id);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("checkpoint.json");
        std::fs::write(&path, serde_json::to_vec_pretty(cp)?)?;
        self.store.save_checkpoint_ref(&CheckpointRef {
            task_id: cp.task_id.clone(),
            created_at_ms: cp.created_at_ms,
            artifact_path: path.display().to_string(),
            integrity: cp.integrity.clone(),
        })?;
        Ok(())
    }

    /// Load and integrity-check a task's checkpoint before a resume (SUM-90 acceptance).
    fn load_checkpoint(&self, id: &str) -> anyhow::Result<Checkpoint> {
        let cref = self
            .store
            .load_checkpoint_ref(id)?
            .ok_or_else(|| anyhow::anyhow!("no checkpoint recorded for {id}"))?;
        let bytes = std::fs::read(&cref.artifact_path)?;
        let cp: Checkpoint = serde_json::from_slice(&bytes)?;
        if !cp.verify() || cp.integrity != cref.integrity {
            anyhow::bail!("checkpoint integrity check failed for {id}");
        }
        Ok(cp)
    }

    /// Freeze an idle task to a checkpoint and stop its provider (§13 / SUM-90..92).
    fn hibernate(&mut self, id: &str) -> Response {
        let now = now_ms();
        if !self.runtimes.contains_key(id) {
            return Response::Error {
                message: format!("task {id} has no live provider to hibernate"),
            };
        }
        let Some(task) = self.tasks.get(id) else {
            return Response::Error {
                message: format!("no such task: {id}"),
            };
        };
        // Conservative safe-point gate — never force a freeze mid-tool-call (SUM-91).
        if let SafePoint::Wait(reason) = assess(&self.activity(task)) {
            self.audit(Some(id), "hibernate_deferred", &reason, None);
            return Response::Error {
                message: format!("not at a safe point: {reason}"),
            };
        }

        let task = task.clone();
        let cp = self.capture_checkpoint(id, &task, now);
        if let Err(e) = self.persist_checkpoint(&cp) {
            return Response::Error {
                message: format!("checkpoint failed: {e}"),
            };
        }
        let cleanup = self.shutdown_runtime(id);

        let task = self.tasks.get_mut(id).expect("checked above");
        for (to, reason) in [
            (TaskState::Checkpointing, "capturing checkpoint"),
            (TaskState::Hibernated, "provider stopped; task frozen"),
        ] {
            if let Ok(t) = task.transition(to, reason, now) {
                let _ = self.store.record_transition(id, &t);
            }
        }
        let _ = self.store.upsert_task(task);
        let v = view(task);
        self.event(Some(id), "lifecycle", "hibernated", "info", "daemon");
        self.audit(
            Some(id),
            "hibernated",
            &format!("checkpoint {} captured; {cleanup}", cp.git_patch_hash),
            Some(
                serde_json::json!({
                    "git_head": cp.git_head,
                    "git_patch_hash": cp.git_patch_hash,
                    "transcript_cursor": cp.transcript_cursor,
                    "rss_baseline_bytes": cp.resource_baseline_bytes,
                })
                .to_string(),
            ),
        );
        Response::Task(v)
    }

    /// Restore a hibernated task from its checkpoint (SUM-92 native / SUM-93 reconstructed).
    fn resume(&mut self, id: &str) -> Response {
        let now = now_ms();
        let Some(task) = self.tasks.get(id) else {
            return Response::Error {
                message: format!("no such task: {id}"),
            };
        };
        if task.state != TaskState::Hibernated {
            return Response::Error {
                message: format!("task {id} is not hibernated (state {})", task.state),
            };
        }
        let cp = match self.load_checkpoint(id) {
            Ok(cp) => cp,
            Err(e) => {
                return Response::Error {
                    message: format!("cannot resume {id}: {e}"),
                }
            }
        };
        let task = task.clone();
        let outcome = match self.relaunch_from_checkpoint(id, &task, &cp, now) {
            Ok(o) => o,
            Err(e) => {
                // Resume failed entirely: leave the task hibernated with its checkpoint intact so
                // no work is lost, and report the failure (SUM-93 reconstructed-fallback exhausted).
                self.audit(
                    Some(id),
                    "resume_failed",
                    &format!("resume failed, checkpoint retained: {e}"),
                    None,
                );
                return Response::Error {
                    message: format!("resume failed (task remains hibernated): {e}"),
                };
            }
        };

        let task = self.tasks.get_mut(id).expect("checked above");
        for (to, reason) in [
            (TaskState::Resuming, "restoring from checkpoint"),
            (TaskState::Active, "provider resumed"),
        ] {
            if let Ok(t) = task.transition(to, reason, now) {
                let _ = self.store.record_transition(id, &t);
            }
        }
        let _ = self.store.upsert_task(task);
        let v = view(task);
        // The checkpoint has been consumed; the task is live again.
        let _ = self.store.delete_checkpoint_ref(id);
        self.emit_resume_event(id, &outcome);
        Response::Task(v)
    }

    /// Recycle a running provider: checkpoint, restart at a safe point, resume, and ledger the
    /// reclaimed memory (§8.7 / SUM-94..97).
    fn recycle(&mut self, id: &str) -> Response {
        let now = now_ms();
        if !self.runtimes.contains_key(id) {
            return Response::Error {
                message: format!("task {id} has no live provider to recycle"),
            };
        }
        let Some(task) = self.tasks.get(id) else {
            return Response::Error {
                message: format!("no such task: {id}"),
            };
        };
        if let SafePoint::Wait(reason) = assess(&self.activity(task)) {
            self.audit(Some(id), "recycle_deferred", &reason, None);
            return Response::Error {
                message: format!("not at a safe point: {reason}"),
            };
        }
        let task = task.clone();
        let rss_before = self
            .runtimes
            .get(id)
            .and_then(|rt| rt.pid())
            .map(sample_subtree_rss)
            .unwrap_or(0);

        let cp = self.capture_checkpoint(id, &task, now);
        if let Err(e) = self.persist_checkpoint(&cp) {
            return Response::Error {
                message: format!("recycle checkpoint failed: {e}"),
            };
        }
        let cleanup = self.shutdown_runtime(id);

        // Active -> Recycling while the provider is down.
        if let Some(t) = self.tasks.get_mut(id) {
            if let Ok(tr) = t.transition(TaskState::Recycling, "recycling provider", now) {
                let _ = self.store.record_transition(id, &tr);
            }
            let _ = self.store.upsert_task(t);
        }

        let outcome = match self.relaunch_from_checkpoint(id, &task, &cp, now) {
            Ok(o) => o,
            Err(e) => {
                // Rollback: resume failed, so fail the task but retain the checkpoint (no lost
                // work — the user can resume later) — SUM-96.
                if let Some(t) = self.tasks.get_mut(id) {
                    if let Ok(tr) = t.transition(TaskState::Failed, "recycle resume failed", now) {
                        let _ = self.store.record_transition(id, &tr);
                    }
                    let _ = self.store.upsert_task(t);
                }
                self.audit(
                    Some(id),
                    "recycle_rolled_back",
                    &format!("resume after recycle failed, checkpoint retained: {e}"),
                    None,
                );
                return Response::Error {
                    message: format!("recycle failed, checkpoint retained: {e}"),
                };
            }
        };
        let rss_after = self
            .runtimes
            .get(id)
            .and_then(|rt| rt.pid())
            .map(sample_subtree_rss)
            .unwrap_or(0);

        // Recycling -> (Resuming) -> Active.
        let task_ref = self.tasks.get_mut(id).expect("checked above");
        for (to, reason) in [
            (TaskState::Resuming, "restoring from checkpoint"),
            (TaskState::Active, "provider recycled"),
        ] {
            if let Ok(t) = task_ref.transition(to, reason, now) {
                let _ = self.store.record_transition(id, &t);
            }
        }
        let _ = self.store.upsert_task(task_ref);
        let v = view(task_ref);
        let _ = self.store.delete_checkpoint_ref(id);

        // Ledger the recycle (Appendix B `runtime_recycled` shape — SUM-97).
        let reclamation = Reclamation {
            rss_before,
            rss_after,
        };
        let record = RecycleRecord::new(
            reclamation,
            outcome.mode,
            outcome.latency_ms,
            cp.git_patch_hash.clone(),
        );
        let payload = serde_json::to_string(&record).ok();
        let _ = self.store.append_event(&EventInput {
            task_id: Some(id.to_string()),
            ts_ms: now_ms(),
            category: "lifecycle".to_string(),
            event_type: "runtime_recycled".to_string(),
            severity: "info".to_string(),
            source: "daemon".to_string(),
            payload_json: payload,
        });
        self.audit(
            Some(id),
            "recycled",
            &format!(
                "{}; resume {}; {cleanup}",
                reclamation.summary(),
                outcome.mode.label()
            ),
            serde_json::to_string(&record).ok(),
        );
        Response::Task(v)
    }

    /// Relaunch a task from its checkpoint, choosing native resume vs a reconstructed session and
    /// validating the result. Installs the new runtime on success.
    fn relaunch_from_checkpoint(
        &mut self,
        id: &str,
        task: &Task,
        cp: &Checkpoint,
        now: u64,
    ) -> anyhow::Result<ResumeOutcome> {
        let (adapter, spec) = launch_spec_for(task);
        let cap = adapter.capabilities().resume;
        let mode = plan_resume(cap, cp.has_native_session());

        let started = now_ms();
        let launched = match mode {
            ResumeMode::Native => {
                let session = cp.provider_session_ref.as_deref().unwrap_or_default();
                match adapter.resume_command(&spec, session) {
                    Some(pty) => RuntimeInstance::spawn_pty(adapter.provider(), pty, now),
                    None => RuntimeInstance::launch(adapter.as_ref(), &spec, now),
                }
            }
            ResumeMode::Reconstructed | ResumeMode::ColdStart => {
                RuntimeInstance::launch(adapter.as_ref(), &spec, now)
            }
        };

        let mut rt = match launched {
            Ok(rt) => rt,
            Err(e) if mode == ResumeMode::Native => {
                // Native path failed to spawn — fall back to a reconstructed session (SUM-93).
                let fallback = RuntimeInstance::launch(adapter.as_ref(), &spec, now)?;
                let latency = now_ms().saturating_sub(started);
                self.install_runtime(id, fallback);
                return Ok(ResumeOutcome::reconstructed(
                    latency,
                    format!("native resume failed: {e}"),
                ));
            }
            Err(e) => return Err(e),
        };

        // Post-resume validation (SUM-93 / SUM-96): the provider must actually be running.
        if !rt.is_running() {
            anyhow::bail!("resumed provider exited immediately");
        }
        let latency = now_ms().saturating_sub(started);
        self.install_runtime(id, rt);
        Ok(match mode {
            ResumeMode::Native => ResumeOutcome::native(latency),
            ResumeMode::Reconstructed => {
                ResumeOutcome::reconstructed(latency, "no native session handle")
            }
            ResumeMode::ColdStart => {
                ResumeOutcome::cold_start(latency, "provider does not support resume")
            }
        })
    }

    fn emit_resume_event(&self, id: &str, outcome: &ResumeOutcome) {
        let _ = self.store.append_event(&EventInput {
            task_id: Some(id.to_string()),
            ts_ms: now_ms(),
            category: "lifecycle".to_string(),
            event_type: "resumed".to_string(),
            severity: "info".to_string(),
            source: "daemon".to_string(),
            payload_json: serde_json::to_string(outcome).ok(),
        });
        self.audit(
            Some(id),
            "resumed",
            &format!(
                "resumed via {} in {} ms (validated={})",
                outcome.mode.label(),
                outcome.latency_ms,
                outcome.validated
            ),
            serde_json::to_string(outcome).ok(),
        );
    }

    /// Pump all live runtimes: drain output into capture + screen, spill evicted scrollback to
    /// the history store, and retire exited tasks. Called on a timer by the server.
    pub fn pump(&mut self, now: u64) {
        let mut exited = Vec::new();
        for (id, rt) in self.runtimes.iter_mut() {
            rt.pump(now);
            if let Some(hist) = self.histories.get_mut(id) {
                let _ = hist.append(rt.take_evicted());
            }
            if !rt.is_running() {
                exited.push(id.clone());
            }
        }
        for id in exited {
            if let Some(mut rt) = self.runtimes.remove(&id) {
                // Final drain + flush before dropping the runtime.
                if let Some(hist) = self.histories.get_mut(&id) {
                    let _ = hist.append(rt.take_evicted());
                    let _ = hist.flush();
                }
            }
            if let Some(task) = self.tasks.get_mut(&id) {
                if !task.state.is_terminal() && task.state != TaskState::Terminating {
                    if let Ok(t) = task.transition(TaskState::Terminating, "provider exited", now) {
                        let _ = self.store.record_transition(&id, &t);
                    }
                    if let Ok(t) = task.transition(TaskState::Terminated, "provider exited", now) {
                        let _ = self.store.record_transition(&id, &t);
                    }
                    let _ = self.store.upsert_task(task);
                }
            }
            self.event(Some(&id), "process", "process_exited", "info", "daemon");
        }

        self.check_recycle_triggers(now);
    }

    /// Throttled RSS-threshold recycle trigger (SUM-94): every `RECYCLE_CHECK_INTERVAL_MS`, sample
    /// each live provider's subtree RSS and recycle those over their per-provider threshold — only
    /// at a safe point, with a decision event recording the reason and measured RSS.
    fn check_recycle_triggers(&mut self, now: u64) {
        if now.saturating_sub(self.last_recycle_check_ms) < RECYCLE_CHECK_INTERVAL_MS {
            return;
        }
        self.last_recycle_check_ms = now;

        // Collect (id, rss, reason) for over-threshold providers without holding a runtime borrow.
        let mut to_recycle: Vec<(String, u64, String)> = Vec::new();
        for (id, rt) in self.runtimes.iter() {
            let Some(task) = self.tasks.get(id) else {
                continue;
            };
            let Some(pid) = rt.pid() else { continue };
            let rss = sample_subtree_rss(pid);
            let policy = RecyclePolicy::for_provider(task.spec.provider);
            if let Some(reason) = policy.should_recycle(rss) {
                to_recycle.push((id.clone(), rss, reason));
            }
        }
        for (id, rss, reason) in to_recycle {
            self.audit(
                Some(&id),
                "recycle_triggered",
                &reason,
                Some(serde_json::json!({ "rss_bytes": rss }).to_string()),
            );
            // recycle() re-checks the safe point and rolls back on failure.
            let _ = self.recycle(&id);
        }
    }

    /// Current screen grid of a running task.
    pub fn screen_view(&self, id: &str) -> Option<ScreenView> {
        self.runtimes.get(id).map(|rt| {
            let (row, col) = rt.cursor();
            ScreenView {
                rows: rt.screen_rows(),
                cursor_row: row,
                cursor_col: col,
                running: true,
            }
        })
    }

    /// Whether a task's provider process is live.
    pub fn is_running(&self, id: &str) -> bool {
        self.runtimes.contains_key(id)
    }

    /// Forward bytes to a running task's stdin (attach input).
    pub fn write_stdin(&mut self, id: &str, data: &[u8]) {
        if let Some(rt) = self.runtimes.get_mut(id) {
            let _ = rt.write_stdin(data);
        }
    }

    /// Resize a running task's terminal (attach resize).
    pub fn resize(&mut self, id: &str, rows: u16, cols: u16) {
        if let Some(rt) = self.runtimes.get_mut(id) {
            let _ = rt.resize(rows, cols);
        }
    }

    fn read_history(&self, id: &str, cursor: u64, limit: u32) -> Response {
        match self.histories.get(id) {
            Some(store) => match store.read_history(cursor, limit as usize) {
                Ok(lines) => Response::History(HistoryPage {
                    lines: lines.iter().map(|l| l.render()).collect(),
                    next_cursor: cursor + lines.len() as u64,
                    total: store.total_lines(),
                }),
                Err(e) => Response::Error {
                    message: format!("history error: {e}"),
                },
            },
            None => Response::History(HistoryPage {
                lines: Vec::new(),
                next_cursor: cursor,
                total: 0,
            }),
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

/// Build the adapter and launch spec for a task, resolving only the secrets its capability grant
/// permits (SUM-79). Free function so it borrows nothing of `DaemonState`.
fn launch_spec_for(task: &Task) -> (Box<dyn ProviderAdapter>, LaunchSpec) {
    let adapter = adapter_for(task.spec.provider);
    let mut spec = LaunchSpec::in_dir(&task.spec.repository_path);
    spec.command = task.spec.command.clone();
    let refs = adapter.secret_refs();
    let names: Vec<String> = refs.iter().map(|r| r.name.clone()).collect();
    let grant = CapabilityGrant::least_privilege(&task.spec.repository_path).with_secrets(names);
    spec.env = grant.resolve_env(&refs);
    (adapter, spec)
}

/// Decide the resume mode from the provider's negotiated fidelity and whether a native session
/// handle was checkpointed (SUM-92 / SUM-93). Pure and unit-tested.
fn plan_resume(cap: ResumeFidelity, has_native_session: bool) -> ResumeMode {
    match cap {
        ResumeFidelity::Native if has_native_session => ResumeMode::Native,
        // Native-capable but no session handle, or explicitly reconstructed: rebuild context.
        ResumeFidelity::Native | ResumeFidelity::Reconstructed => ResumeMode::Reconstructed,
        ResumeFidelity::Unsupported => ResumeMode::ColdStart,
    }
}

/// `HEAD` commit sha of a git repository, if it is one.
fn git_head(repo: &std::path::Path) -> Option<String> {
    memmux_worktree::gitcmd::git_ok(repo, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Hash of the working-tree patch (`git diff HEAD`); empty repos/patches hash deterministically.
fn git_patch_hash(repo: &std::path::Path) -> String {
    let diff = memmux_worktree::gitcmd::git_ok(repo, &["diff", "HEAD"]).unwrap_or_default();
    memmux_lifecycle::content_hash(diff.as_bytes())
}

/// Resident bytes of the process subtree rooted at `pid` (0 if the sampler can't see it).
fn sample_subtree_rss(pid: u32) -> u64 {
    let sampler = memmux_metrics::default_sampler();
    match sampler.snapshot() {
        Ok(snap) => {
            memmux_metrics::ProcessTree::from_samples(snap.samples).subtree_rss_bytes(pid as i32)
        }
        Err(_) => 0,
    }
}

/// Force-clean the process subtree rooted at `pid` and report whether it is fully gone (SUM-95).
fn verify_subtree_gone(pid: u32) -> (bool, String) {
    #[cfg(unix)]
    {
        let sampler = memmux_metrics::default_sampler();
        match memmux_metrics::terminate_subtree(sampler.as_ref(), pid as i32, TREE_VERIFY_GRACE) {
            Ok(report) => (
                report.fully_cleaned(),
                format!(
                    "process-tree cleanup {:.0}%{}",
                    report.cleanup_fraction() * 100.0,
                    if report.used_sigkill {
                        " (sigkill)"
                    } else {
                        ""
                    }
                ),
            ),
            Err(e) => (false, format!("process-tree verify error: {e}")),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        (true, "process-tree verification is unix-only".into())
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
        DaemonState::boot(
            store,
            envelope,
            std::env::temp_dir().join("memmuxd-unit-test"),
        )
        .unwrap()
    }

    fn create(state: &mut DaemonState, title: &str) -> TaskView {
        match state.handle(Request::CreateTask(CreateTaskRequest {
            title: title.into(),
            repository_path: "/src/product".into(),
            provider: "claude-code".into(),
            base_branch: "main".into(),
            resource_class: None,
            priority: None,
            command: None,
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
            command: None,
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
            let mut s = DaemonState::boot(Store::open(&db).unwrap(), env, dir.clone()).unwrap();
            create(&mut s, "persisted task").id
        };
        // Reboot: a fresh state on the same store must recover the task.
        let s2 = DaemonState::boot(Store::open(&db).unwrap(), env, dir.clone()).unwrap();
        assert_eq!(s2.task_count(), 1);
        assert!(s2.tasks.contains_key(&id));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn plan_resume_picks_native_only_with_a_session_handle() {
        // Native-capable provider with a checkpointed session -> native resume.
        assert_eq!(
            plan_resume(ResumeFidelity::Native, true),
            ResumeMode::Native
        );
        // Native-capable but no session handle -> reconstructed, not a false native claim.
        assert_eq!(
            plan_resume(ResumeFidelity::Native, false),
            ResumeMode::Reconstructed
        );
        // Reconstructed-only providers always reconstruct.
        assert_eq!(
            plan_resume(ResumeFidelity::Reconstructed, true),
            ResumeMode::Reconstructed
        );
        // No resume support -> cold start.
        assert_eq!(
            plan_resume(ResumeFidelity::Unsupported, false),
            ResumeMode::ColdStart
        );
    }

    #[test]
    fn hibernate_requires_a_live_provider() {
        let mut s = state();
        let t = create(&mut s, "idle task"); // QUEUED, never started
        match s.handle(Request::HibernateTask { id: t.id }) {
            Response::Error { message } => assert!(message.contains("no live provider")),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn resume_requires_a_hibernated_task() {
        let mut s = state();
        let t = create(&mut s, "queued task");
        match s.handle(Request::ResumeTask { id: t.id }) {
            Response::Error { message } => assert!(message.contains("not hibernated")),
            other => panic!("expected error, got {other:?}"),
        }
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
