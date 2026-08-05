//! The async UDS server: accepts client connections, dispatches framed requests, streams
//! attach sessions, and drives the periodic pump that reads provider output.
//!
//! The socket lives under a `0700` directory and is itself `0600`, so only the owning user can
//! talk to the daemon (least-privilege local socket — SUM-63/SUM-78).

use crate::daemon::{now_ms, DaemonState};
use crate::frame::{read_frame, write_frame};
use memmux_proto::{AttachClientMsg, AttachServerMsg, Request, Response};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};

/// How often the daemon pumps provider output into capture + screen.
const PUMP_INTERVAL: Duration = Duration::from_millis(100);
/// How often an attached client receives a screen frame.
const ATTACH_FRAME_INTERVAL: Duration = Duration::from_millis(50);

/// Bind the socket and serve requests until the listener errors.
pub async fn serve(socket: &Path, state: Arc<Mutex<DaemonState>>) -> anyhow::Result<()> {
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    if socket.exists() {
        std::fs::remove_file(socket)?;
    }
    let listener = UnixListener::bind(socket)?;
    let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600));
    tracing::info!(socket = %socket.display(), "memmuxd listening");

    // Background pump: read provider output into each task's capture buffer + screen.
    let pump_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PUMP_INTERVAL);
        loop {
            tick.tick().await;
            if let Ok(mut guard) = pump_state.lock() {
                guard.pump(now_ms());
            }
        }
    });

    loop {
        let (stream, _addr) = listener.accept().await?;
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream, state).await {
                tracing::debug!(error = %e, "connection ended");
            }
        });
    }
}

async fn handle_conn(mut stream: UnixStream, state: Arc<Mutex<DaemonState>>) -> anyhow::Result<()> {
    while let Some(body) = read_frame(&mut stream).await? {
        match serde_json::from_slice::<Request>(&body) {
            // Attach switches this connection into a bidirectional stream (SUM-86).
            Ok(Request::Attach { id }) => return attach_loop(stream, state, id).await,
            Ok(request) => {
                let response = {
                    let mut guard = state.lock().expect("daemon state poisoned");
                    guard.handle(request)
                };
                write_frame(&mut stream, &serde_json::to_vec(&response)?).await?;
            }
            Err(e) => {
                let response = Response::Error {
                    message: format!("bad request: {e}"),
                };
                write_frame(&mut stream, &serde_json::to_vec(&response)?).await?;
            }
        }
    }
    Ok(())
}

/// Bidirectional attach: stream screen frames to the client while forwarding its input to the
/// task's PTY, until the client detaches, disconnects, or the process exits (SUM-86).
async fn attach_loop(
    stream: UnixStream,
    state: Arc<Mutex<DaemonState>>,
    id: String,
) -> anyhow::Result<()> {
    let (mut rd, mut wr) = stream.into_split();
    let mut tick = tokio::time::interval(ATTACH_FRAME_INTERVAL);

    loop {
        tokio::select! {
            biased;
            frame = read_frame(&mut rd) => {
                match frame? {
                    None => break, // client disconnected
                    Some(bytes) => match serde_json::from_slice::<AttachClientMsg>(&bytes) {
                        Ok(AttachClientMsg::Input { data }) => {
                            state.lock().expect("state poisoned").write_stdin(&id, &data);
                        }
                        Ok(AttachClientMsg::Resize { rows, cols }) => {
                            state.lock().expect("state poisoned").resize(&id, rows, cols);
                        }
                        Ok(AttachClientMsg::Detach) => break,
                        Err(_) => {} // ignore malformed frames
                    },
                }
            }
            _ = tick.tick() => {
                let (screen, running) = {
                    let guard = state.lock().expect("state poisoned");
                    (guard.screen_view(&id), guard.is_running(&id))
                };
                if let Some(sv) = screen {
                    let msg = AttachServerMsg::Screen(sv);
                    write_frame(&mut wr, &serde_json::to_vec(&msg)?).await?;
                }
                if !running {
                    let msg = AttachServerMsg::Exited;
                    write_frame(&mut wr, &serde_json::to_vec(&msg)?).await?;
                    break;
                }
            }
        }
    }
    Ok(())
}
