//! End-to-end test of the daemon UDS API: a real client round-trips create/list over the
//! socket against a running server (SUM-63).

#![cfg(unix)]

use memmux_proto::{CreateTaskRequest, Request, Response};
use memmux_sched::{ResourceEnvelope, GIB};
use memmux_store::Store;
use memmuxd::client::Client;
use memmuxd::daemon::DaemonState;
use memmuxd::server;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("memmuxd-rt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn client_round_trips_create_and_list_over_the_socket() {
    let dir = tmp();
    let socket = dir.join("memmux.sock");
    let store = Store::open(dir.join("state.db")).unwrap();
    let envelope = ResourceEnvelope::with_default_reserves(32 * GIB);
    let state = Arc::new(Mutex::new(DaemonState::boot(store, envelope).unwrap()));

    // Run the server on a background current-thread runtime.
    let sp = socket.clone();
    let st = Arc::clone(&state);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _ = rt.block_on(server::serve(&sp, st));
    });

    // Wait for the socket to appear.
    for _ in 0..300 {
        if socket.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(socket.exists(), "daemon socket never appeared");

    let client = Client::new(&socket);

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
