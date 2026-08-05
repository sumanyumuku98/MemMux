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
use memmux_proto::Request;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::path::PathBuf;
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

    let mut model = Model::default();
    refresh(&client, &mut model);

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
                        for effect in effects {
                            apply_effect(client, model, effect);
                        }
                    }
                }
            }
        }

        if model.should_quit {
            break;
        }
        if last_refresh.elapsed() >= Duration::from_secs(1) {
            refresh(client, model);
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

fn apply_effect(client: &Client, model: &mut Model, effect: Effect) {
    match effect {
        Effect::Refresh => refresh(client, model),
        Effect::CreateTask(req) => match client.call(&Request::CreateTask(req)) {
            Ok(_) => refresh(client, model),
            Err(e) => model.status = format!("create failed: {e}"),
        },
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
