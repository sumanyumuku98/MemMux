//! The async UDS server: accepts client connections and dispatches framed requests.
//!
//! The socket lives under a `0700` directory and is itself `0600`, so only the owning user can
//! talk to the daemon (least-privilege local socket — SUM-63/SUM-78).

use crate::daemon::DaemonState;
use crate::frame::{read_frame, write_frame};
use memmux_proto::{Request, Response};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::net::{UnixListener, UnixStream};

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
        let response = match serde_json::from_slice::<Request>(&body) {
            Ok(request) => {
                // Handlers are synchronous and fast; the lock is never held across an await.
                let mut guard = state.lock().expect("daemon state poisoned");
                guard.handle(request)
            }
            Err(e) => Response::Error {
                message: format!("bad request: {e}"),
            },
        };
        let out = serde_json::to_vec(&response)?;
        write_frame(&mut stream, &out).await?;
    }
    Ok(())
}
