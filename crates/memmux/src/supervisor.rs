//! Daemon lifecycle supervision for single-command startup (SUM-118).
//!
//! `memmux` is a client; it needs `memmuxd` running. Rather than make the user start the daemon
//! by hand, the TUI reuses a running daemon if one is up, and otherwise spawns `memmuxd serve`
//! itself (detached, under the same managed root) and waits for it to accept connections. The
//! auto-started daemon is a background service and is **left running** when the TUI exits.

use crate::client::Client;
use memmux_proto::Request;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long to wait for a freshly-spawned daemon to accept connections.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
/// How often to re-probe the socket while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Whether a daemon is already listening and answering on the client's socket.
pub fn daemon_is_up(client: &Client) -> bool {
    client.call(&Request::DaemonInfo).is_ok()
}

/// Resolve the `memmuxd` binary to spawn: `$MEMMUXD_BIN` if set, else a sibling of the current
/// executable (so an installed `memmux`/`memmuxd` pair Just Works), else bare `memmuxd` on `PATH`.
pub fn memmuxd_path() -> PathBuf {
    if let Some(p) = std::env::var_os("MEMMUXD_BIN") {
        return PathBuf::from(p);
    }
    let name = if cfg!(windows) {
        "memmuxd.exe"
    } else {
        "memmuxd"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(sib) = exe.parent().map(|d| d.join(name)) {
            if sib.exists() {
                return sib;
            }
        }
    }
    PathBuf::from(name)
}

/// Ensure a daemon is reachable on the client's socket.
///
/// Returns `Ok(false)` if an already-running daemon was reused, `Ok(true)` if a new one was
/// spawned and became reachable, or an `Err` with a clear message if it could not be started
/// (binary missing, permission error, or it never came up). Never spawns a second daemon when
/// one is already answering.
pub fn ensure_daemon(root: &Path, client: &Client, memmuxd_bin: &Path) -> anyhow::Result<bool> {
    if daemon_is_up(client) {
        return Ok(false);
    }

    std::fs::create_dir_all(root).ok();
    let mut cmd = Command::new(memmuxd_bin);
    cmd.arg("serve")
        .arg("--root")
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        // Put the daemon in its own process group so a Ctrl-C / exit in the TUI's terminal does
        // not tear it down — it outlives the UI.
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().map_err(|e| {
        anyhow::anyhow!(
            "could not start the daemon '{}': {e} — install memmuxd (it ships alongside memmux) \
             or set MEMMUXD_BIN",
            memmuxd_bin.display()
        )
    })?;

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        if daemon_is_up(client) {
            return Ok(true);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    anyhow::bail!(
        "daemon was started but did not become reachable at {} within {}s",
        client.socket().display(),
        STARTUP_TIMEOUT.as_secs()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memmuxd_path_honors_env_override() {
        std::env::set_var("MEMMUXD_BIN", "/custom/memmuxd");
        assert_eq!(memmuxd_path(), PathBuf::from("/custom/memmuxd"));
        std::env::remove_var("MEMMUXD_BIN");
    }
}
