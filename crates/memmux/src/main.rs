//! `memmux` — the terminal UI runtime. Sets up the terminal, runs the event loop, maps key
//! events to [`app::Key`], applies the pure `update`, and executes the resulting effects against
//! the daemon. All UI logic lives in the library (`app` + `render`) so it stays testable.

use clap::{Parser, Subcommand};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use memmux::app::{update, Effect, Key, Model};
use memmux::client::Client;
use memmux::render::render;
use memmux_proto::{AttachClientMsg, AttachServerMsg, CreateTaskRequest, Request};
use ratatui::backend::{Backend, CrosstermBackend};
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
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Update MemMux to the latest release, in place (SUM-129).
    Update,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // `memmux update`: self-update the installed binaries and exit (no TUI).
    if matches!(cli.command, Some(Command::Update)) {
        return run_self_update();
    }

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

    // Default the new-task repo to the launch directory's git root (SUM-119), and remember the raw
    // launch dir too so a plain shell / the folder browser can start anywhere (SUM-130).
    let mut model = Model {
        cwd_repo: cwd_git_root(),
        cwd: std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned()),
        ..Model::default()
    };
    refresh(&client, &mut model);
    match autostart {
        Ok(true) => model.status = "started daemon".to_string(),
        Ok(false) => {} // reused a running daemon; refresh already set the status
        Err(e) => model.status = format!("daemon auto-start failed: {e}"),
    }

    // Non-blocking "update available" check (SUM-129) — result surfaces in the status line.
    let (hint_tx, hint_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        if let Some(msg) = memmux::update::check_for_update(env!("CARGO_PKG_VERSION")) {
            let _ = hint_tx.send(msg);
        }
    });

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;

    let result = run(&mut terminal, &client, &mut model, &hint_rx);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    client: &Client,
    model: &mut Model,
    hint_rx: &std::sync::mpsc::Receiver<String>,
) -> anyhow::Result<()> {
    let mut last_refresh = Instant::now();
    loop {
        // Surface the background update-check result once it arrives (SUM-129).
        if let Ok(msg) = hint_rx.try_recv() {
            model.status = msg;
        }
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
                            // Raw passthrough returned (detached / agent exited); refresh the list.
                            refresh(client, model);
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
            last_refresh = Instant::now();
        }
    }
    Ok(())
}

/// `memmux update`: resolve the newest release and swap the installed binaries in place (SUM-129).
fn run_self_update() -> anyhow::Result<()> {
    // Install where the current binary lives, unless overridden (matches the installer's env).
    let bin_dir = std::env::var_os("MEMMUX_BIN_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    println!("checking for updates…");
    match memmux::update::run_update(env!("CARGO_PKG_VERSION"), &bin_dir) {
        Ok(memmux::update::Outcome::UpToDate(v)) => {
            println!("memmux is already up to date (v{v})");
        }
        Ok(memmux::update::Outcome::Updated {
            from,
            to,
            installed,
        }) => {
            println!("updated {}: {from} → {to}", installed.join(" + "));
            println!("restart the daemon to use the new version:  pkill memmuxd");
        }
        Err(e) => {
            eprintln!("update failed: {e}");
            std::process::exit(1);
        }
    }
    Ok(())
}

/// The git root of the current directory, if any (default repo for new tasks — SUM-119).
fn cwd_git_root() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!root.is_empty()).then_some(root)
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
        // One-key quick-launch (SUM-130): create → start → attach, all here. Returns the id to
        // attach only when a provider actually came up.
        Effect::QuickLaunch {
            provider,
            repo,
            shell,
        } => return quick_launch(client, model, &provider, &repo, shell),
        // Open an agent (SUM-130): reuse/start/restart, attaching only if a live provider results —
        // this is the fix for Enter flashing a blank screen on a dead agent.
        Effect::OpenTask(id) => {
            if client.start(&id).is_ok() || client.restart(&id).is_ok() {
                refresh(client, model);
                return Some(id);
            }
            refresh(client, model);
            model.status = format!("{id}: agent exited — press Enter to restart, x to remove");
        }
        Effect::TerminateTask(id) => match client.terminate(&id) {
            Ok(()) => {
                model.status = format!("terminated {id}");
                refresh(client, model);
            }
            Err(e) => model.status = format!("terminate failed: {e}"),
        },
        Effect::ForgetTask(id) => match client.forget(&id) {
            Ok(()) => {
                model.status = format!("removed {id}");
                refresh(client, model);
            }
            Err(e) => model.status = format!("remove failed: {e}"),
        },
        Effect::ListDir(path) => list_dir(model, &path),
        Effect::AddWorkspace(path) => match client.add_workspace(&path) {
            Ok(()) => refresh(client, model),
            Err(e) => model.status = format!("open folder failed: {e}"),
        },
        Effect::Attach(id) => return Some(id),
    }
    None
}

/// Quick-launch (SUM-130): create a task for `provider` in `repo`, start it, and — on success —
/// return its id so the caller enters attach. A `shell` launch uses the generic provider with
/// `$SHELL` (the generic adapter with no command exits immediately, so one must be supplied).
fn quick_launch(
    client: &Client,
    model: &mut Model,
    provider: &str,
    repo: &str,
    shell: bool,
) -> Option<String> {
    let command = shell.then(|| vec![default_shell()]);
    let req = CreateTaskRequest {
        title: String::new(),
        repository_path: repo.to_string(),
        provider: provider.to_string(),
        base_branch: "main".to_string(),
        resource_class: None,
        priority: None,
        command,
    };
    let id = match client.create(req) {
        Ok(id) => id,
        Err(e) => {
            model.status = format!("launch failed: {e}");
            return None;
        }
    };
    match client.start(&id) {
        Ok(()) => {
            refresh(client, model);
            model.status = format!("launched {provider}");
            Some(id)
        }
        Err(e) => {
            refresh(client, model);
            model.status = format!("launch failed: {e}");
            None
        }
    }
}

/// The user's interactive shell for a plain-terminal quick-launch, falling back to `/bin/sh`.
fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// List the sub-directories of `path` for the folder browser (SUM-130), writing the result into
/// the model. A leading `..` is offered unless `path` is the filesystem root. Unreadable entries
/// are skipped; hidden dirs (dotfiles) are shown so repos like `.config` work.
fn list_dir(model: &mut Model, path: &str) {
    let dir = std::path::Path::new(path);
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut entries: Vec<String> = Vec::new();
    if canonical.parent().is_some() {
        entries.push("..".to_string());
    }
    match std::fs::read_dir(&canonical) {
        Ok(rd) => {
            let mut dirs: Vec<String> = rd
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            dirs.sort();
            entries.extend(dirs);
        }
        Err(e) => model.status = format!("cannot read {path}: {e}"),
    }
    model.browse_dir = canonical.to_string_lossy().into_owned();
    model.browse_entries = entries;
    model.browse_selected = 0;
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

/// Interactive attach: a **true raw passthrough** to the task's PTY (SUM-125). The agent's raw
/// output bytes are written straight to our terminal so it renders the agent's own colours/UI
/// natively; our keystrokes are forwarded as input. `Ctrl-a d` detaches (`Ctrl-a Ctrl-a` sends a
/// literal Ctrl-a). The agent is sized to our real terminal so it repaints full-screen.
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

    // Size the agent to our real terminal, then clear so it repaints over a blank screen.
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    send_frame(
        &mut stream,
        &serde_json::to_vec(&AttachClientMsg::Resize { rows, cols })?,
    )?;
    {
        let mut out = stdout();
        let _ = execute!(
            out,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
            crossterm::cursor::MoveTo(0, 0)
        );
    }

    // Reader thread: write the agent's raw bytes straight to our stdout.
    let mut reader_stream = stream.try_clone()?;
    let (exit_tx, exit_rx) = mpsc::channel::<()>();
    let reader = std::thread::spawn(move || {
        let mut out = stdout();
        while let Ok(Some(bytes)) = recv_frame(&mut reader_stream) {
            match serde_json::from_slice::<AttachServerMsg>(&bytes) {
                Ok(AttachServerMsg::Output { data: b }) => {
                    let _ = out.write_all(&b);
                    let _ = out.flush();
                }
                Ok(AttachServerMsg::Exited) => break,
                _ => {}
            }
        }
        let _ = exit_tx.send(());
    });

    let mut prefix = false;
    let result = loop {
        if exit_rx.try_recv().is_ok() {
            break Ok(()); // agent exited
        }
        if event::poll(Duration::from_millis(20))? {
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => {
                    let ctrl_a =
                        k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('a');
                    if ctrl_a && !prefix {
                        prefix = true;
                        continue;
                    }
                    if prefix {
                        prefix = false;
                        if k.code == KeyCode::Char('d') {
                            let _ = send_frame(
                                &mut stream,
                                &serde_json::to_vec(&AttachClientMsg::Detach)?,
                            );
                            break Ok(());
                        }
                        if ctrl_a {
                            // Ctrl-a Ctrl-a -> send a literal Ctrl-a to the agent.
                            send_frame(
                                &mut stream,
                                &serde_json::to_vec(&AttachClientMsg::Input { data: vec![0x01] })?,
                            )?;
                            continue;
                        }
                    }
                    if let Some(bytes) = key_to_bytes(k.code, k.modifiers) {
                        send_frame(
                            &mut stream,
                            &serde_json::to_vec(&AttachClientMsg::Input { data: bytes })?,
                        )?;
                    }
                }
                Event::Resize(cols, rows) => {
                    let _ = send_frame(
                        &mut stream,
                        &serde_json::to_vec(&AttachClientMsg::Resize { rows, cols })?,
                    );
                }
                _ => {}
            }
        }
    };

    drop(stream);
    let _ = reader.join();
    // The agent painted directly to the screen; force a clean TUI redraw on return.
    terminal.clear()?;
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
