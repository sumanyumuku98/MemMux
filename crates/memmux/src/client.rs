//! Blocking client to the daemon's UDS API, assembling [`Data`] for the TUI.

use crate::app::Data;
use anyhow::Context;
use memmux_proto::{HistoryPage, Request, Response, ScreenView};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// A blocking client that opens a connection per call.
#[derive(Clone, Debug)]
pub struct Client {
    socket: PathBuf,
}

impl Client {
    /// Target the given socket path.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// The socket path.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Send one request, read one response.
    pub fn call(&self, request: &Request) -> anyhow::Result<Response> {
        let mut stream = UnixStream::connect(&self.socket)
            .with_context(|| format!("connecting to daemon at {}", self.socket.display()))?;
        let body = serde_json::to_vec(request)?;
        stream.write_all(&(body.len() as u32).to_be_bytes())?;
        stream.write_all(&body)?;
        stream.flush()?;

        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf)?;
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut resp = vec![0u8; len];
        stream.read_exact(&mut resp)?;
        Ok(serde_json::from_slice(&resp)?)
    }

    /// Fetch a full data snapshot for the dashboard.
    pub fn fetch(&self) -> anyhow::Result<Data> {
        let mut data = Data::default();
        if let Response::Tasks(t) = self.call(&Request::ListTasks)? {
            data.tasks = t;
        }
        if let Response::Pressure(p) = self.call(&Request::SystemPressure)? {
            data.pressure = Some(p);
        }
        if let Response::DaemonInfo(d) = self.call(&Request::DaemonInfo)? {
            data.daemon = Some(d);
        }
        if let Response::Events(e) = self.call(&Request::ReadEvents {
            after_seq: 0,
            limit: 500,
        })? {
            data.events = e;
        }
        if let Response::Workspaces(w) = self.call(&Request::ListWorkspaces)? {
            data.workspaces = w;
        }
        Ok(data)
    }

    /// Register a folder as a workspace (SUM-124).
    pub fn add_workspace(&self, path: &str) -> anyhow::Result<()> {
        self.call(&Request::AddWorkspace {
            path: path.to_string(),
        })?;
        Ok(())
    }

    /// Create a task and return its id (SUM-130, used by quick-launch to chain create→start→attach).
    pub fn create(&self, req: memmux_proto::CreateTaskRequest) -> anyhow::Result<String> {
        match self.call(&Request::CreateTask(req))? {
            Response::Task(t) => Ok(t.id),
            Response::Error { message } => Err(anyhow::anyhow!(message)),
            other => Err(anyhow::anyhow!("unexpected create response: {other:?}")),
        }
    }

    /// Admit and launch a task's provider. Errors when the daemon refuses (e.g. not startable).
    pub fn start(&self, id: &str) -> anyhow::Result<()> {
        match self.call(&Request::StartTask { id: id.to_string() })? {
            Response::Error { message } => Err(anyhow::anyhow!(message)),
            _ => Ok(()),
        }
    }

    /// Restart a dead/failed task: re-queue and relaunch (SUM-130).
    pub fn restart(&self, id: &str) -> anyhow::Result<()> {
        match self.call(&Request::RestartTask { id: id.to_string() })? {
            Response::Error { message } => Err(anyhow::anyhow!(message)),
            _ => Ok(()),
        }
    }

    /// Terminate a running/queued task (SUM-130).
    pub fn terminate(&self, id: &str) -> anyhow::Result<()> {
        match self.call(&Request::TerminateTask { id: id.to_string() })? {
            Response::Error { message } => Err(anyhow::anyhow!(message)),
            _ => Ok(()),
        }
    }

    /// Forget a terminal task, removing it from the registry (SUM-130).
    pub fn forget(&self, id: &str) -> anyhow::Result<()> {
        match self.call(&Request::ForgetTask { id: id.to_string() })? {
            Response::Error { message } => Err(anyhow::anyhow!(message)),
            _ => Ok(()),
        }
    }

    /// Fetch the current screen grid for a running task, if any.
    pub fn get_screen(&self, id: &str) -> anyhow::Result<Option<ScreenView>> {
        match self.call(&Request::GetScreen { id: id.to_string() })? {
            Response::Screen(s) => Ok(Some(s)),
            _ => Ok(None),
        }
    }

    /// Page a task's scrollback history.
    pub fn read_history(&self, id: &str, cursor: u64, limit: u32) -> anyhow::Result<HistoryPage> {
        match self.call(&Request::ReadHistory {
            id: id.to_string(),
            cursor,
            limit,
        })? {
            Response::History(h) => Ok(h),
            _ => Ok(HistoryPage {
                lines: Vec::new(),
                next_cursor: cursor,
                total: 0,
            }),
        }
    }
}
