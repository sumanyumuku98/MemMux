//! Blocking client for the daemon's UDS API (used by CLI subcommands and tests).

use anyhow::Context;
use memmux_proto::{Request, Response};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// A blocking client that connects to the daemon socket per call.
#[derive(Clone, Debug)]
pub struct Client {
    socket: PathBuf,
}

impl Client {
    /// Create a client targeting the given socket path.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// Send one request and read one response.
    pub fn call(&self, request: &Request) -> anyhow::Result<Response> {
        let mut stream = UnixStream::connect(&self.socket)
            .with_context(|| format!("connecting to daemon at {}", self.socket.display()))?;
        let body = serde_json::to_vec(request)?;
        write_frame(&mut stream, &body)?;
        let resp = read_frame(&mut stream)?
            .ok_or_else(|| anyhow::anyhow!("daemon closed the connection without responding"))?;
        Ok(serde_json::from_slice(&resp)?)
    }

    /// The socket path.
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

fn write_frame(stream: &mut UnixStream, body: &[u8]) -> std::io::Result<()> {
    stream.write_all(&(body.len() as u32).to_be_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn read_frame(stream: &mut UnixStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    Ok(Some(body))
}
