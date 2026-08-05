//! End-to-end test of the daemon UDS API: a real client round-trips create/list over the
//! socket against a running server (SUM-63).

#![cfg(unix)]

use memmux_proto::{AttachClientMsg, AttachServerMsg, CreateTaskRequest, Request, Response};
use memmux_sched::{ResourceEnvelope, GIB};
use memmux_store::Store;
use memmuxd::client::Client;
use memmuxd::daemon::DaemonState;
use memmuxd::server;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn write_frame(s: &mut UnixStream, body: &[u8]) {
    s.write_all(&(body.len() as u32).to_be_bytes()).unwrap();
    s.write_all(body).unwrap();
    s.flush().unwrap();
}

fn read_frame(s: &mut UnixStream) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len).ok()?;
    let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
    s.read_exact(&mut body).ok()?;
    Some(body)
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("memmuxd-rt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Spawn the server on a background runtime and wait for its socket.
fn spawn_server(dir: &std::path::Path) -> (Client, std::path::PathBuf) {
    let socket = dir.join("memmux.sock");
    let store = Store::open(dir.join("state.db")).unwrap();
    let envelope = ResourceEnvelope::with_default_reserves(32 * GIB);
    let state = Arc::new(Mutex::new(
        DaemonState::boot(store, envelope, dir.to_path_buf()).unwrap(),
    ));
    let sp = socket.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _ = rt.block_on(server::serve(&sp, state));
    });
    for _ in 0..300 {
        if socket.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(socket.exists(), "daemon socket never appeared");
    (Client::new(&socket), socket)
}

#[test]
fn client_round_trips_create_and_list_over_the_socket() {
    let dir = tmp("roundtrip");
    let (client, socket) = spawn_server(&dir);

    // Least-privilege local socket (SUM-78): 0600 socket under a 0700 directory.
    use std::os::unix::fs::PermissionsExt;
    let sock_mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
    assert_eq!(sock_mode, 0o600, "socket mode was {sock_mode:o}");
    let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "socket dir mode was {dir_mode:o}");

    // Handshake.
    match client.call(&Request::DaemonInfo).unwrap() {
        Response::DaemonInfo(i) => assert!(i.agent_budget_bytes > 0),
        other => panic!("expected DaemonInfo, got {other:?}"),
    }

    // Create a task.
    let created = match client
        .call(&Request::CreateTask(CreateTaskRequest {
            title: "Round-trip task".into(),
            repository_path: "/src/product".into(),
            provider: "codex".into(),
            base_branch: "main".into(),
            resource_class: None,
            priority: None,
            command: None,
        }))
        .unwrap()
    {
        Response::Task(t) => t,
        other => panic!("expected Task, got {other:?}"),
    };
    assert_eq!(created.state, "QUEUED");
    assert_eq!(created.provider, "codex");

    // List reflects it.
    match client.call(&Request::ListTasks).unwrap() {
        Response::Tasks(ts) => {
            assert_eq!(ts.len(), 1);
            assert_eq!(ts[0].id, created.id);
        }
        other => panic!("expected Tasks, got {other:?}"),
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn start_streams_screen_and_pages_history() {
    let dir = tmp("stream");
    let (client, _socket) = spawn_server(&dir);

    // A generic task that prints enough lines to overflow the resident ring into history.
    let created = match client
        .call(&Request::CreateTask(CreateTaskRequest {
            title: "streamer".into(),
            repository_path: "/tmp".into(),
            provider: "generic".into(),
            base_branch: "main".into(),
            resource_class: None,
            priority: None,
            command: Some(vec![
                "sh".into(),
                "-c".into(),
                "for i in $(seq 1 6000); do echo line-$i; done; sleep 5".into(),
            ]),
        }))
        .unwrap()
    {
        Response::Task(t) => t,
        other => panic!("expected Task, got {other:?}"),
    };

    // Launch the provider.
    match client
        .call(&Request::StartTask {
            id: created.id.clone(),
        })
        .unwrap()
    {
        Response::Task(t) => assert_eq!(t.state, "ACTIVE"),
        other => panic!("expected Task, got {other:?}"),
    }

    // The screen shows recent output.
    let mut saw_screen = false;
    for _ in 0..200 {
        if let Response::Screen(s) = client
            .call(&Request::GetScreen {
                id: created.id.clone(),
            })
            .unwrap()
        {
            if s.rows.iter().any(|r| r.contains("line-")) {
                saw_screen = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(saw_screen, "screen never showed provider output");

    // Older output overflowed the resident buffer into paged history (SUM-85).
    let mut history_ok = false;
    for _ in 0..200 {
        if let Response::History(h) = client
            .call(&Request::ReadHistory {
                id: created.id.clone(),
                cursor: 0,
                limit: 20,
            })
            .unwrap()
        {
            if h.total > 0
                && h.lines
                    .first()
                    .map(|l| l.contains("line-1"))
                    .unwrap_or(false)
            {
                history_ok = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(history_ok, "history never paged the earliest lines");

    // Terminating kills the provider process.
    match client
        .call(&Request::TerminateTask {
            id: created.id.clone(),
        })
        .unwrap()
    {
        Response::Task(t) => assert_eq!(t.state, "TERMINATED"),
        other => panic!("expected Task, got {other:?}"),
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// Start a long-running generic task and return its id once ACTIVE.
fn start_long_running(client: &Client, title: &str) -> String {
    let created = match client
        .call(&Request::CreateTask(CreateTaskRequest {
            title: title.into(),
            repository_path: "/tmp".into(),
            provider: "generic".into(),
            base_branch: "main".into(),
            resource_class: None,
            priority: None,
            command: Some(vec![
                "sh".into(),
                "-c".into(),
                "while true; do echo tick; sleep 1; done".into(),
            ]),
        }))
        .unwrap()
    {
        Response::Task(t) => t,
        other => panic!("expected Task, got {other:?}"),
    };
    match client
        .call(&Request::StartTask {
            id: created.id.clone(),
        })
        .unwrap()
    {
        Response::Task(t) => assert_eq!(t.state, "ACTIVE"),
        other => panic!("expected Task, got {other:?}"),
    }
    created.id
}

#[test]
fn hibernate_checkpoints_and_resume_restores_the_task() {
    let dir = tmp("hibernate");
    let (client, _socket) = spawn_server(&dir);
    let id = start_long_running(&client, "hibernator");

    // Hibernate: the provider stops and the task freezes.
    match client
        .call(&Request::HibernateTask { id: id.clone() })
        .unwrap()
    {
        Response::Task(t) => assert_eq!(t.state, "HIBERNATED"),
        other => panic!("expected Task, got {other:?}"),
    }

    // A checkpoint artifact was persisted on disk (SUM-90) and it verifies its own integrity.
    let cp_path = dir.join("tasks").join(&id).join("checkpoint.json");
    assert!(
        cp_path.exists(),
        "checkpoint artifact missing at {cp_path:?}"
    );
    let cp: serde_json::Value = serde_json::from_slice(&std::fs::read(&cp_path).unwrap()).unwrap();
    assert!(cp.get("integrity").and_then(|v| v.as_str()).is_some());
    assert_eq!(cp["task_id"], serde_json::json!(id));

    // The provider is no longer resident while hibernated.
    match client.call(&Request::GetScreen { id: id.clone() }).unwrap() {
        Response::Error { .. } => {}
        other => panic!("expected no live screen while hibernated, got {other:?}"),
    }

    // Resume: the task comes back to ACTIVE with a live provider.
    match client
        .call(&Request::ResumeTask { id: id.clone() })
        .unwrap()
    {
        Response::Task(t) => assert_eq!(t.state, "ACTIVE"),
        other => panic!("expected Task, got {other:?}"),
    }

    // The resumed provider produces output again.
    let mut alive = false;
    for _ in 0..100 {
        if let Response::Screen(s) = client.call(&Request::GetScreen { id: id.clone() }).unwrap() {
            if s.running {
                alive = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(alive, "resumed provider never came back to life");

    client.call(&Request::TerminateTask { id }).ok();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn recycle_restarts_provider_and_ledgers_reclamation() {
    let dir = tmp("recycle");
    let (client, _socket) = spawn_server(&dir);
    let id = start_long_running(&client, "recyclable");

    // Recycle: checkpoint, restart at a safe point, resume.
    match client
        .call(&Request::RecycleTask { id: id.clone() })
        .unwrap()
    {
        Response::Task(t) => assert_eq!(t.state, "ACTIVE"),
        other => panic!("expected Task, got {other:?}"),
    }

    // A `runtime_recycled` ledger event was emitted (SUM-97).
    let mut ledgered = false;
    if let Response::Events(evs) = client
        .call(&Request::ReadEvents {
            after_seq: 0,
            limit: 200,
        })
        .unwrap()
    {
        ledgered = evs.iter().any(|e| e.event_type == "runtime_recycled");
    }
    assert!(ledgered, "no runtime_recycled event was ledgered");

    // The provider is live again after the recycle.
    let mut alive = false;
    for _ in 0..100 {
        if let Response::Screen(s) = client.call(&Request::GetScreen { id: id.clone() }).unwrap() {
            if s.running {
                alive = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(alive, "recycled provider is not running");

    client.call(&Request::TerminateTask { id }).ok();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn attach_streams_screen_and_accepts_input() {
    let dir = tmp("attach");
    let (client, socket) = spawn_server(&dir);

    // A generic task that echoes its stdin (so we can prove input flows through attach).
    let created = match client
        .call(&Request::CreateTask(CreateTaskRequest {
            title: "cat".into(),
            repository_path: "/tmp".into(),
            provider: "generic".into(),
            base_branch: "main".into(),
            resource_class: None,
            priority: None,
            command: Some(vec!["cat".into()]),
        }))
        .unwrap()
    {
        Response::Task(t) => t,
        other => panic!("expected Task, got {other:?}"),
    };
    match client
        .call(&Request::StartTask {
            id: created.id.clone(),
        })
        .unwrap()
    {
        Response::Task(t) => assert_eq!(t.state, "ACTIVE"),
        other => panic!("expected Task, got {other:?}"),
    }

    // Open a raw connection and enter attach mode.
    let mut s = UnixStream::connect(&socket).unwrap();
    write_frame(
        &mut s,
        &serde_json::to_vec(&Request::Attach {
            id: created.id.clone(),
        })
        .unwrap(),
    );

    // Send input; `cat` echoes it back into the screen.
    write_frame(
        &mut s,
        &serde_json::to_vec(&AttachClientMsg::Input {
            data: b"echo-through-attach\n".to_vec(),
        })
        .unwrap(),
    );

    // Read screen frames until the echoed input shows up.
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut saw = false;
    for _ in 0..200 {
        match read_frame(&mut s) {
            Some(bytes) => {
                if let Ok(AttachServerMsg::Screen(sv)) =
                    serde_json::from_slice::<AttachServerMsg>(&bytes)
                {
                    if sv.rows.iter().any(|r| r.contains("echo-through-attach")) {
                        saw = true;
                        break;
                    }
                }
            }
            None => break,
        }
    }
    assert!(saw, "attach never reflected the injected input");

    // Detach leaves the task running.
    write_frame(
        &mut s,
        &serde_json::to_vec(&AttachClientMsg::Detach).unwrap(),
    );
    drop(s);
    match client
        .call(&Request::GetTask {
            id: created.id.clone(),
        })
        .unwrap()
    {
        Response::Task(t) => assert_eq!(t.state, "ACTIVE", "task should survive detach"),
        other => panic!("expected Task, got {other:?}"),
    }

    client.call(&Request::TerminateTask { id: created.id }).ok();
    std::fs::remove_dir_all(&dir).ok();
}
