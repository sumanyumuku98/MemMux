//! Single-command startup supervision (SUM-118): reuse a running daemon; report a clear error
//! when one cannot be started.

#![cfg(unix)]

use memmux::client::Client;
use memmux::supervisor::{daemon_is_up, ensure_daemon};
use memmux_sched::{ResourceEnvelope, GIB};
use memmux_store::Store;
use memmuxd::daemon::DaemonState;
use memmuxd::server;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("memmux-autostart-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Boot a real daemon in-process on a temp socket (mirrors the daemon round-trip harness).
fn boot_server(dir: &Path) -> PathBuf {
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
    socket
}

#[test]
fn reuses_a_running_daemon_without_spawning() {
    let dir = tmp("reuse");
    let socket = boot_server(&dir);
    let client = Client::new(&socket);

    // A bogus binary path: if ensure_daemon tried to spawn, it would fail — but the daemon is
    // already up, so it must reuse it and never touch the binary.
    let spawned = ensure_daemon(&dir, &client, Path::new("/nonexistent/memmuxd")).unwrap();
    assert!(
        !spawned,
        "expected to reuse the running daemon, not spawn a new one"
    );
    assert!(daemon_is_up(&client));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn reports_a_clear_error_when_the_daemon_cannot_start() {
    let dir = tmp("fail");
    let client = Client::new(dir.join("memmux.sock"));

    let err = ensure_daemon(&dir, &client, Path::new("/nonexistent/memmuxd"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("could not start the daemon"),
        "error should name the failure clearly, got: {err}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
