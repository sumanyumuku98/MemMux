//! `memmux` — the terminal UI runtime. Sets up the terminal, runs the event loop, maps key
//! events to [`app::Key`], applies the pure `update`, and executes the resulting effects against
//! the daemon. All UI logic lives in the library (`app` + `render`) so it stays testable.

use clap::{Parser, Subcommand};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use memmux::app::update;
use memmux::app::{Dir, Effect, Focus, Key, Model, Orient, View};
use memmux::client::Client;
use memmux::pane::PaneManager;
use memmux::render::render;
use memmux_proto::{CreateTaskRequest, Request};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use std::io::stdout;
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

    // User config (SUM-133): the pane leader key from <root>/config.toml (default Ctrl-b).
    let cfg = memmux::config::Config::load(&root);

    // Default the new-task repo to the launch directory's git root (SUM-119), and remember the raw
    // launch dir too so a plain shell / the folder browser can start anywhere (SUM-130).
    let mut model = Model {
        cwd_repo: cwd_git_root(),
        cwd: std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned()),
        prefix: cfg.prefix,
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
    // Mouse capture (SUM-133) lets clicks focus panes / select sidebar rows.
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;

    let result = run(&mut terminal, &client, &mut model, &hint_rx);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

fn run<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    client: &Client,
    model: &mut Model,
    hint_rx: &std::sync::mpsc::Receiver<String>,
) -> anyhow::Result<()> {
    // Reader threads ping `dirty` when a pane gets new output so we can redraw promptly (SUM-132).
    let (dirty_tx, dirty_rx) = mpsc::channel::<()>();
    let mut panes = PaneManager::new(client.socket().to_path_buf(), dirty_tx);
    let mut last_refresh = Instant::now();
    loop {
        // Snapshot live pane screens into the model, resize sessions to their rects, drop closed.
        sync_panes(model, &mut panes);
        // Surface the background update-check result once it arrives (SUM-129).
        if let Ok(msg) = hint_rx.try_recv() {
            model.status = msg;
        }
        terminal.draw(|f| render(f, model))?;

        // Redraw quickly while panes stream output; idle otherwise.
        let timeout = if model.panes.is_some() {
            Duration::from_millis(33)
        } else {
            Duration::from_millis(250)
        };
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if model.view == View::Home && model.focus == Focus::Panes {
                        // Focused pane grid: the leader drives pane commands, else keys are
                        // forwarded to the focused agent (SUM-132/133).
                        handle_pane_key(client, model, &mut panes, key.code, key.modifiers);
                    } else if let Some(k) = map_key(key.code, key.modifiers) {
                        for effect in update(model, k) {
                            apply_effect(client, model, &mut panes, effect);
                        }
                    }
                }
                // Left-click focuses a pane or selects a sidebar row (SUM-133).
                Event::Mouse(me)
                    if model.view == View::Home
                        && me.kind == MouseEventKind::Down(MouseButton::Left) =>
                {
                    handle_mouse_click(model, me.column, me.row);
                }
                _ => {}
            }
        }
        // Coalesce pane dirty pings (we redraw every loop anyway).
        while dirty_rx.try_recv().is_ok() {}

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

/// The pane grid's rect on screen — must match `render_home`'s split (SUM-132).
fn grid_area() -> Rect {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    Rect {
        x: memmux::render::SIDEBAR_WIDTH.min(cols),
        y: 1,
        width: cols.saturating_sub(memmux::render::SIDEBAR_WIDTH),
        height: rows.saturating_sub(2),
    }
}

/// The sidebar rect on screen — mirrors `render_home`'s split (SUM-133).
fn sidebar_area() -> Rect {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    Rect {
        x: 0,
        y: 1,
        width: memmux::render::SIDEBAR_WIDTH.min(cols),
        height: rows.saturating_sub(2),
    }
}

/// Route a left-click: select a sidebar row, else focus the pane under the cursor (SUM-133).
fn handle_mouse_click(model: &mut Model, col: u16, row: u16) {
    if model.select_nav_at(col, row, sidebar_area()) {
        return;
    }
    model.focus_pane_at(col, row, grid_area());
}

/// Each open pane's inner content size `(rows, cols)` (rect minus its 1-cell border).
fn pane_sizes(model: &Model) -> Vec<(String, (u16, u16))> {
    let Some(layout) = &model.panes else {
        return Vec::new();
    };
    let area = grid_area();
    let rects = if model.zoomed {
        match model.focused_pane() {
            Some(f) => vec![(f.to_string(), area)],
            None => layout.leaf_rects(area),
        }
    } else {
        layout.leaf_rects(area)
    };
    rects
        .into_iter()
        .map(|(id, r)| (id, (r.height.saturating_sub(2), r.width.saturating_sub(2))))
        .collect()
}

/// Resize live pane PTYs to their rects, snapshot them into the model, and drop closed sessions.
fn sync_panes(model: &mut Model, panes: &mut PaneManager) {
    let ids: Vec<String> = model.panes.as_ref().map(|l| l.leaves()).unwrap_or_default();
    panes.retain(&ids);
    for (id, (rows, cols)) in pane_sizes(model) {
        panes.resize(&id, rows, cols);
        if let Some(grid) = panes.snapshot(&id) {
            model.pane_screens.insert(id, grid);
        }
    }
    model.pane_screens.retain(|k, _| ids.iter().any(|i| i == k));
}

/// Bring a task's provider up (start, else restart) and open it as a live pane (SUM-132).
fn open_agent_pane(client: &Client, model: &mut Model, panes: &mut PaneManager, id: &str) {
    if client.start(id).is_ok() || client.restart(id).is_ok() {
        model.open_pane(id);
        refresh(client, model);
        let (rows, cols) = pane_sizes(model)
            .into_iter()
            .find(|(pid, _)| pid == id)
            .map(|(_, sz)| sz)
            .unwrap_or((24, 80));
        if let Err(e) = panes.open(id, rows.max(1), cols.max(1)) {
            model.status = format!("attach failed: {e}");
            model.close_pane(id);
        }
    } else {
        model.close_pane(id);
        refresh(client, model);
        model.status = format!("{id}: agent exited — Enter to retry, x to remove");
    }
}

/// Keys while a pane is focused: the configurable leader (default `Ctrl-b`) + command, else forward
/// to the agent (SUM-132/133). The leader comes from `model.prefix` (config.toml).
fn handle_pane_key(
    client: &Client,
    model: &mut Model,
    panes: &mut PaneManager,
    code: KeyCode,
    mods: KeyModifiers,
) {
    let pfx = model.prefix;
    let prefix = mods.contains(KeyModifiers::CONTROL) == pfx.ctrl && code == KeyCode::Char(pfx.ch);
    if model.prefix_active {
        model.prefix_active = false;
        match code {
            KeyCode::Char('o') | KeyCode::Char('d') | KeyCode::Esc => model.focus = Focus::Sidebar,
            KeyCode::Char('h') => model.focus_dir(Dir::Left, grid_area()),
            KeyCode::Char('j') => model.focus_dir(Dir::Down, grid_area()),
            KeyCode::Char('k') => model.focus_dir(Dir::Up, grid_area()),
            KeyCode::Char('l') => model.focus_dir(Dir::Right, grid_area()),
            KeyCode::Char('z') => model.toggle_zoom(),
            KeyCode::Char('x') => {
                if let Some(id) = model.focused_pane().map(str::to_string) {
                    model.close_pane(&id);
                    panes.close(&id);
                }
            }
            // Split: choose orientation for the next opened pane, then pick an agent to launch.
            KeyCode::Char('v') => {
                model.split_orient = Orient::Cols;
                open_launch_from_panes(model);
            }
            KeyCode::Char('-') => {
                model.split_orient = Orient::Rows;
                open_launch_from_panes(model);
            }
            // Leader pressed twice → send a literal leader byte to the agent.
            _ if prefix => {
                if let Some(id) = model.focused_pane().map(str::to_string) {
                    panes.send_input(&id, &[pfx.literal_byte()]);
                }
            }
            _ => {}
        }
        return;
    }
    if prefix {
        model.prefix_active = true;
        return;
    }
    if let Some(id) = model.focused_pane().map(str::to_string) {
        if let Some(bytes) = key_to_bytes(code, mods) {
            panes.send_input(&id, &bytes);
        }
    }
    // The runtime will start the provider / open the session when the palette pick returns.
    let _ = client;
}

/// Open the quick-launch palette from within the pane grid (Ctrl-b v / Ctrl-b -).
fn open_launch_from_panes(model: &mut Model) {
    let repo = model.launch_target_repo();
    model.launch_repo = repo;
    model.launch_selected = 0;
    model.view = View::Launch;
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

/// Apply one effect against the daemon and the local pane manager (SUM-132).
fn apply_effect(client: &Client, model: &mut Model, panes: &mut PaneManager, effect: Effect) {
    match effect {
        Effect::Refresh => refresh(client, model),
        Effect::CreateTask(req) => match client.call(&Request::CreateTask(req)) {
            Ok(_) => refresh(client, model),
            Err(e) => model.status = format!("create failed: {e}"),
        },
        // Quick-launch (SUM-130/132): create the task, then open it as a live pane.
        Effect::QuickLaunch {
            provider,
            repo,
            shell,
        } => {
            if let Some(id) = quick_launch(client, model, &provider, &repo, shell) {
                open_agent_pane(client, model, panes, &id);
            }
        }
        // Open an agent as a pane (SUM-132): start-or-restart, then wire the live session.
        Effect::OpenPane(id) => open_agent_pane(client, model, panes, &id),
        Effect::TerminateTask(id) => match client.terminate(&id) {
            Ok(()) => {
                // Terminating an open agent also closes its pane.
                model.close_pane(&id);
                panes.close(&id);
                model.status = format!("terminated {id}");
                refresh(client, model);
            }
            Err(e) => model.status = format!("terminate failed: {e}"),
        },
        Effect::ForgetTask(id) => match client.forget(&id) {
            Ok(()) => {
                model.close_pane(&id);
                panes.close(&id);
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
    }
}

/// Quick-launch (SUM-130): create a task for `provider` in `repo` and return its id. A `shell`
/// launch uses the generic provider with `$SHELL` (the generic adapter with no command exits
/// immediately, so one must be supplied). The caller opens the pane.
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
    match client.create(req) {
        Ok(id) => {
            model.status = format!("launched {provider}");
            Some(id)
        }
        Err(e) => {
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
