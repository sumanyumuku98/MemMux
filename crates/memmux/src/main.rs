//! `memmux` — the terminal UI runtime. Sets up the terminal, runs the event loop, maps key
//! events to [`app::Key`], applies the pure `update`, and executes the resulting effects against
//! the daemon. All UI logic lives in the library (`app` + `render`) so it stays testable.

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use memmux::app::{update, Effect, Key, Model};
use memmux::client::Client;
use memmux::render::render;
use memmux_proto::{AttachClientMsg, AttachServerMsg, Request};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;
use std::io::{stdout, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(name = "memmux", version, about = "MemMux terminal UI")]
struct Cli {
    /// Managed root (defaults to $MEMMUX_ROOT or ~/.memmux); the socket is `<root>/memmux.sock`.
    #[arg(long)]
    root: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let root = cli
        .root
        .or_else(|| std::env::var_os("MEMMUX_ROOT").map(PathBuf::from))
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".memmux")
        });
    let client = Client::new(root.join("memmux.sock"));

    // Single-command startup (SUM-118): reuse a running daemon, else auto-start one.
    let autostart =
        memmux::supervisor::ensure_daemon(&root, &client, &memmux::supervisor::memmuxd_path());

    let mut model = Model::default();
    refresh(&client, &mut model);
    match autostart {
        Ok(true) => model.status = "started daemon".to_string(),
        Ok(false) => {} // reused a running daemon; refresh already set the status
        Err(e) => model.status = format!("daemon auto-start failed: {e}"),
    }

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;

    let result = run(&mut terminal, &client, &mut model);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    client: &Client,
    model: &mut Model,
) -> anyhow::Result<()> {
    let mut last_refresh = Instant::now();
    loop {
        terminal.draw(|f| render(f, model))?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Release {
                    if let Some(k) = map_key(key.code, key.modifiers) {
                        let effects = update(model, k);
                        let mut attach = None;
                        for effect in effects {
                            if let Some(id) = apply_effect(client, model, effect) {
                                attach = Some(id);
                            }
                        }
                        if let Some(id) = attach {
                            if let Err(e) = attach_passthrough(terminal, client, &id) {
                                model.status = format!("attach ended: {e}");
                            }
                            model.view = memmux::app::View::Term;
                        }
                    }
                }
            }
        }

        if model.should_quit {
            break;
        }
        if last_refresh.elapsed() >= Duration::from_millis(1000) {
            refresh(client, model);
            // Keep the live terminal view fresh while it's open.
            if model.view == memmux::app::View::Term {
                if let Some(id) = model.focused_task.clone() {
                    load_screen(client, model, &id);
                }
            }
            last_refresh = Instant::now();
        }
    }
    Ok(())
}

fn map_key(code: KeyCode, mods: KeyModifiers) -> Option<Key> {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        return Some(Key::Quit);
    }
    Some(match code {
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Char(c) => Key::Char(c),
        _ => return None,
    })
}

/// Apply one effect. Returns `Some(task_id)` if an attach passthrough should be entered (the
/// runtime must own the terminal for that, so it can't happen inside this function).
fn apply_effect(client: &Client, model: &mut Model, effect: Effect) -> Option<String> {
    match effect {
        Effect::Refresh => refresh(client, model),
        Effect::CreateTask(req) => match client.call(&Request::CreateTask(req)) {
            Ok(_) => refresh(client, model),
            Err(e) => model.status = format!("create failed: {e}"),
        },
        Effect::StartTask(id) => match client.start(&id) {
            Ok(()) => model.status = format!("started {id}"),
            Err(e) => model.status = format!("start failed: {e}"),
        },
        Effect::LoadScreen(id) => load_screen(client, model, &id),
        Effect::LoadHistory { id, cursor } => match client.read_history(&id, cursor, 500) {
            Ok(page) => model.history_rows = page.lines,
            Err(e) => model.status = format!("history failed: {e}"),
        },
        Effect::Attach(id) => return Some(id),
    }
    None
}

fn load_screen(client: &Client, model: &mut Model, id: &str) {
    match client.get_screen(id) {
        Ok(Some(s)) => model.screen_rows = s.rows,
        Ok(None) => {}
        Err(e) => model.status = format!("screen failed: {e}"),
    }
}

fn refresh(client: &Client, model: &mut Model) {
    match client.fetch() {
        Ok(data) => {
            model.set_data(data);
            model.status = "connected".to_string();
        }
        Err(e) => model.status = format!("daemon unreachable: {e}"),
    }
}

/// Interactive attach: full-screen raw passthrough to a task's PTY until Ctrl-a d (SUM-86).
fn attach_passthrough<B: Backend>(
    terminal: &mut Terminal<B>,
    client: &Client,
    id: &str,
) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(client.socket())?;
    send_frame(
        &mut stream,
        &serde_json::to_vec(&Request::Attach { id: id.to_string() })?,
    )?;

    // Read screen frames on a background thread.
    let mut reader_stream = stream.try_clone()?;
    let (tx, rx) = mpsc::channel::<AttachServerMsg>();
    let reader = std::thread::spawn(move || {
        while let Ok(Some(bytes)) = recv_frame(&mut reader_stream) {
            if let Ok(msg) = serde_json::from_slice::<AttachServerMsg>(&bytes) {
                let exited = matches!(msg, AttachServerMsg::Exited);
                if tx.send(msg).is_err() || exited {
                    break;
                }
            }
        }
    });

    let mut rows: Vec<String> = vec!["(attached — press Ctrl-a then d to detach)".to_string()];
    let mut prefix = false;
    let result = loop {
        terminal.draw(|f| {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!(" ATTACH {id} — Ctrl-a d to detach "));
            let text: Vec<Line> = rows.iter().map(|r| Line::from(r.clone())).collect();
            f.render_widget(Paragraph::new(text).block(block), f.area());
        })?;

        while let Ok(msg) = rx.try_recv() {
            match msg {
                AttachServerMsg::Screen(sv) => rows = sv.rows,
                AttachServerMsg::Exited => break,
            }
        }

        if event::poll(Duration::from_millis(30))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Release {
                    continue;
                }
                if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('a') {
                    prefix = true;
                    continue;
                }
                if prefix {
                    prefix = false;
                    if k.code == KeyCode::Char('d') {
                        let _ =
                            send_frame(&mut stream, &serde_json::to_vec(&AttachClientMsg::Detach)?);
                        break Ok(());
                    }
                }
                if let Some(bytes) = key_to_bytes(k.code, k.modifiers) {
                    send_frame(
                        &mut stream,
                        &serde_json::to_vec(&AttachClientMsg::Input { data: bytes })?,
                    )?;
                }
            }
        }
    };

    drop(stream);
    let _ = reader.join();
    result
}

/// Encode a key event as the bytes a PTY expects.
fn key_to_bytes(code: KeyCode, mods: KeyModifiers) -> Option<Vec<u8>> {
    Some(match code {
        KeyCode::Char(c) => {
            if mods.contains(KeyModifiers::CONTROL) && c.is_ascii_alphabetic() {
                vec![(c.to_ascii_lowercase() as u8) & 0x1f]
            } else {
                c.to_string().into_bytes()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => vec![0x1b, b'[', b'A'],
        KeyCode::Down => vec![0x1b, b'[', b'B'],
        KeyCode::Right => vec![0x1b, b'[', b'C'],
        KeyCode::Left => vec![0x1b, b'[', b'D'],
        _ => return None,
    })
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
