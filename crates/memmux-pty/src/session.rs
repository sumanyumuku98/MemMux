//! Daemon-owned PTY session (SUM-49 / §5, §8.2).
//!
//! Spawns a provider process inside a pseudo-terminal owned by the daemon. Output is read on a
//! background thread and delivered over a channel, so capture continues whether or not a UI is
//! attached. Supports window resize and stdin forwarding, and tears the process down on drop.

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How to launch a PTY session.
#[derive(Clone, Debug)]
pub struct PtySpec {
    /// Program to run.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Working directory, if any.
    pub cwd: Option<PathBuf>,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
    /// Initial terminal rows.
    pub rows: u16,
    /// Initial terminal columns.
    pub cols: u16,
}

impl PtySpec {
    /// A simple spec running `program` with `args` at the default 24×80 size.
    pub fn command(program: impl Into<String>, args: impl IntoIterator<Item = String>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().collect(),
            cwd: None,
            env: Vec::new(),
            rows: 24,
            cols: 80,
        }
    }
}

/// A running PTY session owned by the daemon.
pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    output_rx: Receiver<Vec<u8>>,
    reader: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySession").finish_non_exhaustive()
    }
}

impl PtySession {
    /// Spawn the process described by `spec` inside a new PTY.
    pub fn spawn(spec: &PtySpec) -> anyhow::Result<Self> {
        let pty = native_pty_system();
        let pair = pty.openpty(PtySize {
            rows: spec.rows.max(1),
            cols: spec.cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            cmd.cwd(cwd);
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        let child = pair.slave.spawn_command(cmd)?;
        // Drop the slave handle in the parent so EOF is delivered when the child exits.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let (tx, rx) = channel::<Vec<u8>>();
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match std::io::Read::read(&mut reader, &mut buf) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break; // receiver dropped
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            master: pair.master,
            child,
            writer,
            output_rx: rx,
            reader: Some(reader),
        })
    }

    /// Resize the terminal window.
    pub fn resize(&self, rows: u16, cols: u16) -> anyhow::Result<()> {
        self.master.resize(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// Forward bytes to the process's stdin.
    pub fn write_stdin(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()
    }

    /// Non-blocking read of the next available output chunk.
    pub fn try_read_output(&self) -> Option<Vec<u8>> {
        self.output_rx.try_recv().ok()
    }

    /// Whether the child process is still running.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Terminate the child process.
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()
    }

    /// Wait for the child to exit and return its exit code.
    pub fn wait(&mut self) -> anyhow::Result<u32> {
        let status = self.child.wait()?;
        Ok(status.exit_code())
    }

    /// Collect all output until the process exits or `timeout` elapses (test/utility helper).
    pub fn read_output_until_exit(&mut self, timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        loop {
            while let Some(chunk) = self.try_read_output() {
                out.extend_from_slice(&chunk);
            }
            if !self.is_running() {
                // Drain any remaining buffered output.
                while let Some(chunk) = self.try_read_output() {
                    out.extend_from_slice(&chunk);
                }
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        out
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Own what you launch (§2.4): ensure the child is not left running.
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}
