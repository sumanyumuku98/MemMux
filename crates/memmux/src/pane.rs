//! Client-side terminal multiplexer (SUM-132).
//!
//! Each open agent pane holds a live [`Request::Attach`] connection to the daemon. A reader thread
//! feeds the raw PTY output bytes — which carry the agent's own colours — into a per-pane
//! `vt100::Parser`. The render loop snapshots each parser into a [`StyledGrid`] and draws it into a
//! ratatui rect, so several agents render as colored panes at once with the sidebar still visible.
//! This reuses the existing Attach protocol wholesale; no daemon changes.

use crate::app::{GridCell, StyledGrid};
use memmux_proto::{AttachClientMsg, AttachServerMsg, Request};
use ratatui::style::{Color, Modifier};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A live pane: an Attach connection + the vt100 parser its output feeds, plus a reader thread.
pub struct PaneSession {
    stream: UnixStream,
    parser: Arc<Mutex<vt100::Parser>>,
    alive: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    rows: u16,
    cols: u16,
}

impl PaneSession {
    /// Open an Attach session for `id` sized `rows`×`cols` and start streaming its output into a
    /// vt100 parser. `dirty` is pinged whenever new output arrives so the render loop can wake.
    pub fn open(
        socket: &Path,
        id: &str,
        rows: u16,
        cols: u16,
        dirty: Sender<()>,
    ) -> std::io::Result<Self> {
        let mut stream = UnixStream::connect(socket)?;
        send_frame(
            &mut stream,
            &frame_enc(&Request::Attach { id: id.to_string() }),
        )?;
        send_frame(
            &mut stream,
            &frame_enc(&AttachClientMsg::Resize { rows, cols }),
        )?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows.max(1), cols.max(1), 0)));
        let alive = Arc::new(AtomicBool::new(true));

        let mut reader_stream = stream.try_clone()?;
        let (rparser, ralive) = (Arc::clone(&parser), Arc::clone(&alive));
        let reader = std::thread::spawn(move || {
            while let Ok(Some(bytes)) = recv_frame(&mut reader_stream) {
                match serde_json::from_slice::<AttachServerMsg>(&bytes) {
                    Ok(AttachServerMsg::Output { data }) => {
                        if let Ok(mut p) = rparser.lock() {
                            p.process(&data);
                        }
                        let _ = dirty.send(());
                    }
                    Ok(AttachServerMsg::Exited) => {
                        ralive.store(false, Ordering::Relaxed);
                        let _ = dirty.send(());
                        break;
                    }
                    // Other server frames (e.g. a plain-text Screen) aren't used by the pane parser.
                    _ => {}
                }
            }
            ralive.store(false, Ordering::Relaxed);
            let _ = dirty.send(());
        });

        Ok(Self {
            stream,
            parser,
            alive,
            reader: Some(reader),
            rows,
            cols,
        })
    }

    /// Forward key bytes to the agent's stdin.
    pub fn send_input(&mut self, bytes: &[u8]) {
        let _ = send_frame(
            &mut self.stream,
            &frame_enc(&AttachClientMsg::Input {
                data: bytes.to_vec(),
            }),
        );
    }

    /// Resize the agent's PTY (and the local parser) to a new pane size.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        if (rows, cols) == (self.rows, self.cols) || rows == 0 || cols == 0 {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
        let _ = send_frame(
            &mut self.stream,
            &frame_enc(&AttachClientMsg::Resize { rows, cols }),
        );
    }

    /// Snapshot the current screen into a [`StyledGrid`] for rendering.
    pub fn snapshot(&self) -> StyledGrid {
        let alive = self.alive.load(Ordering::Relaxed);
        let Ok(parser) = self.parser.lock() else {
            return StyledGrid {
                alive,
                ..Default::default()
            };
        };
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let mut grid_rows = Vec::with_capacity(rows as usize);
        for r in 0..rows {
            let mut row = Vec::with_capacity(cols as usize);
            for c in 0..cols {
                row.push(match screen.cell(r, c) {
                    Some(cell) => {
                        let ch = cell.contents().chars().next().unwrap_or(' ');
                        GridCell {
                            ch: if ch == '\0' { ' ' } else { ch },
                            fg: conv_color(cell.fgcolor(), Color::Reset),
                            bg: conv_color(cell.bgcolor(), Color::Reset),
                            mods: cell_mods(cell),
                        }
                    }
                    None => GridCell {
                        ch: ' ',
                        fg: Color::Reset,
                        bg: Color::Reset,
                        mods: Modifier::empty(),
                    },
                });
            }
            grid_rows.push(row);
        }
        StyledGrid {
            rows: grid_rows,
            cursor: screen.cursor_position(),
            alive,
        }
    }
}

impl Drop for PaneSession {
    fn drop(&mut self) {
        let _ = send_frame(&mut self.stream, &frame_enc(&AttachClientMsg::Detach));
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
    }
}

/// Owns the open pane sessions, keyed by task id.
pub struct PaneManager {
    socket: PathBuf,
    dirty: Sender<()>,
    sessions: HashMap<String, PaneSession>,
}

impl PaneManager {
    /// A manager targeting `socket`; reader threads ping `dirty` on new output.
    pub fn new(socket: PathBuf, dirty: Sender<()>) -> Self {
        Self {
            socket,
            dirty,
            sessions: HashMap::new(),
        }
    }

    /// Whether a pane session exists for `id`.
    pub fn has(&self, id: &str) -> bool {
        self.sessions.contains_key(id)
    }

    /// Open a session for `id` (no-op if one already exists).
    pub fn open(&mut self, id: &str, rows: u16, cols: u16) -> std::io::Result<()> {
        if self.sessions.contains_key(id) {
            return Ok(());
        }
        let s = PaneSession::open(&self.socket, id, rows, cols, self.dirty.clone())?;
        self.sessions.insert(id.to_string(), s);
        Ok(())
    }

    /// Close and drop a session (sends Detach).
    pub fn close(&mut self, id: &str) {
        self.sessions.remove(id);
    }

    /// Drop every session not present in `keep` (e.g. panes closed via the layout).
    pub fn retain(&mut self, keep: &[String]) {
        self.sessions.retain(|id, _| keep.iter().any(|k| k == id));
    }

    /// Forward input bytes to the session `id`, if open.
    pub fn send_input(&mut self, id: &str, bytes: &[u8]) {
        if let Some(s) = self.sessions.get_mut(id) {
            s.send_input(bytes);
        }
    }

    /// Resize a session's PTY.
    pub fn resize(&mut self, id: &str, rows: u16, cols: u16) {
        if let Some(s) = self.sessions.get_mut(id) {
            s.resize(rows, cols);
        }
    }

    /// Snapshot one session's grid, if open.
    pub fn snapshot(&self, id: &str) -> Option<StyledGrid> {
        self.sessions.get(id).map(|s| s.snapshot())
    }
}

fn cell_mods(cell: &vt100::Cell) -> Modifier {
    let mut m = Modifier::empty();
    if cell.bold() {
        m |= Modifier::BOLD;
    }
    if cell.italic() {
        m |= Modifier::ITALIC;
    }
    if cell.underline() {
        m |= Modifier::UNDERLINED;
    }
    if cell.inverse() {
        m |= Modifier::REVERSED;
    }
    m
}

fn conv_color(c: vt100::Color, default: Color) -> Color {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn send_frame(s: &mut UnixStream, body: &[u8]) -> std::io::Result<()> {
    s.write_all(&(body.len() as u32).to_be_bytes())?;
    s.write_all(body)?;
    s.flush()
}

fn recv_frame(s: &mut UnixStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match s.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
    s.read_exact(&mut body)?;
    Ok(Some(body))
}

fn frame_enc<T: serde::Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}
