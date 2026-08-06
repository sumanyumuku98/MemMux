//! Rendering for the Home view + modals (SUM-126/127). Pure ratatui drawing over the [`Model`];
//! no I/O. All colours come from [`crate::theme`] so the look stays cohesive.

use crate::app::{FormField, Model, NavItem, PendingAction, View, LAUNCH_ITEMS, PROVIDERS};
use crate::theme;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

const SIDEBAR_WIDTH: u16 = 30;

/// Draw the whole UI for the current model.
pub fn render(f: &mut Frame, model: &Model) {
    // Fill the background so the whole app shares the theme's deep dark surface.
    f.render_widget(
        Block::default().style(Style::default().bg(theme::BG)),
        f.area(),
    );

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    render_header(f, chunks[0], model);
    render_home(f, chunks[1], model);
    render_status(f, chunks[2], model);

    // Modals draw over the body.
    match model.view {
        View::NewTask => render_new_task(f, chunks[1], model),
        View::Launch => render_launch(f, chunks[1], model),
        View::Confirm => render_confirm(f, chunks[1], model),
        View::OpenFolder => render_open_folder(f, chunks[1], model),
        View::Help => render_help(f, chunks[1]),
        View::Home => {}
    }
}

fn render_header(f: &mut Frame, area: Rect, model: &Model) {
    let budget = model
        .data
        .daemon
        .as_ref()
        .map(|d| format!("{} MiB", d.agent_budget_bytes / (1024 * 1024)))
        .unwrap_or_else(|| "—".to_string());
    let (pct, stage) = model
        .data
        .pressure
        .as_ref()
        .map(|p| (p.utilization_pct, p.stage.clone()))
        .unwrap_or((0, "—".to_string()));

    let left = Line::from(vec![
        Span::styled("◆ ", Style::default().fg(theme::ACCENT)),
        Span::styled(
            "MemMux",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  memory budget ", theme::dim()),
        Span::styled(budget, Style::default().fg(theme::ACCENT2)),
        Span::styled(format!("  ·  {pct}% used · {stage}"), theme::dim()),
    ]);
    f.render_widget(Paragraph::new(left), pad(area));
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "c launch  x close  o folder  ? help  q quit ",
            theme::dim(),
        )))
        .alignment(Alignment::Right),
        area,
    );
}

fn render_home(f: &mut Frame, area: Rect, model: &Model) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(10)])
        .split(area);
    render_sidebar(f, cols[0], model);
    render_detail(f, cols[1], model);
}

fn render_sidebar(f: &mut Frame, area: Rect, model: &Model) {
    let nav = model.nav_items();
    let inner_w = area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = nav
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let selected = i == model.selected;
            let line = match *item {
                NavItem::Workspace(wi) => {
                    let w = &model.data.workspaces[wi];
                    Line::from(vec![
                        Span::styled("▸ ", Style::default().fg(theme::ACCENT)),
                        Span::styled(
                            short(&w.name, 20),
                            Style::default()
                                .fg(theme::ACCENT)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!("  {}", w.task_count), theme::dim()),
                    ])
                }
                NavItem::Unregistered => Line::from(Span::styled("▸ other", theme::dim())),
                NavItem::Agent(ti) => {
                    let t = &model.data.tasks[ti];
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled("● ", Style::default().fg(theme::state_color(&t.state))),
                        Span::styled(short(&t.title, 22), Style::default().fg(theme::FG)),
                    ])
                }
            };
            let style = if selected {
                theme::selected()
            } else {
                Style::default()
            };
            // Pad to full width so the selection highlight spans the row.
            ListItem::new(pad_line(line, inner_w)).style(style)
        })
        .collect();

    let list = List::new(items).block(panel(" WORKSPACES "));
    f.render_widget(list, area);

    if nav.is_empty() {
        let hint = Paragraph::new(Line::from(Span::styled(
            "press c to launch an agent",
            theme::dim(),
        )))
        .alignment(Alignment::Center);
        f.render_widget(hint, centered_v(area));
    }
}

fn render_detail(f: &mut Frame, area: Rect, model: &Model) {
    let body: Vec<Line> = match model.selected_nav() {
        Some(NavItem::Agent(ti)) => {
            let t = &model.data.tasks[ti];
            vec![
                kv("agent", &t.title),
                kv("id", &t.id),
                kv("provider", &t.provider),
                Line::from(vec![
                    Span::styled(format!("{:<12}", "state"), theme::dim()),
                    Span::styled(
                        t.state.clone(),
                        Style::default().fg(theme::state_color(&t.state)),
                    ),
                ]),
                kv("workspace", &workspace_name(model, &t.repository)),
                Line::from(""),
                Line::from(Span::styled(
                    "Enter → open interactive session (Ctrl-a d to detach)",
                    Style::default().fg(theme::ACCENT2),
                )),
            ]
        }
        Some(NavItem::Workspace(wi)) => {
            let w = &model.data.workspaces[wi];
            vec![
                kv("workspace", &w.name),
                kv("path", &w.path),
                kv("agents", &w.task_count.to_string()),
                Line::from(""),
                Line::from(Span::styled(
                    "Enter / n → launch an agent here",
                    Style::default().fg(theme::ACCENT2),
                )),
            ]
        }
        _ => vec![
            Line::from(Span::styled("No agent selected.", theme::dim())),
            Line::from(""),
            Line::from(Span::styled(
                "o → open a folder as a workspace",
                theme::dim(),
            )),
            Line::from(Span::styled("n → new agent", theme::dim())),
        ],
    };
    f.render_widget(Paragraph::new(body).block(panel(" AGENT ")), area);
}

fn render_new_task(f: &mut Frame, area: Rect, model: &Model) {
    let form = &model.form;
    let field = |label: &str, value: &str, focused: bool| {
        let marker = if focused { "▶ " } else { "  " };
        let style = if focused {
            Style::default()
                .fg(theme::ACCENT2)
                .add_modifier(Modifier::BOLD)
        } else {
            theme::dim()
        };
        Line::from(vec![
            Span::styled(format!("{marker}{label:<10}"), style),
            Span::styled(value.to_string(), Style::default().fg(theme::FG)),
        ])
    };
    let provider = {
        let focused = form.field == FormField::Provider;
        let marker = if focused { "▶ " } else { "  " };
        Line::from(vec![
            Span::styled(
                format!("{marker}{:<10}", "provider"),
                if focused {
                    Style::default()
                        .fg(theme::ACCENT2)
                        .add_modifier(Modifier::BOLD)
                } else {
                    theme::dim()
                },
            ),
            Span::styled(
                format!("‹ {} ›", form.provider()),
                Style::default().fg(theme::ACCENT),
            ),
            Span::styled(format!("   ({})", PROVIDERS.join(" ")), theme::dim()),
        ])
    };
    let text = vec![
        field("title", &form.title, form.field == FormField::Title),
        Line::from(Span::styled(
            "  (optional — auto-named if blank)",
            theme::dim(),
        )),
        field("repo", &form.repo, form.field == FormField::Repo),
        provider,
        field("base", &form.base, form.field == FormField::Base),
        Line::from(""),
        Line::from(Span::styled(
            "tab/↑↓ move · ←→ provider · enter launch · esc cancel",
            theme::dim(),
        )),
    ];
    modal(f, area, " NEW AGENT ", text);
}

/// Interactive folder browser (SUM-130): arrow-navigate directories and press `a` to register the
/// folder currently shown, instead of hand-typing an absolute path.
fn render_open_folder(f: &mut Frame, area: Rect, model: &Model) {
    let dir = if model.browse_dir.is_empty() {
        "…"
    } else {
        model.browse_dir.as_str()
    };
    let mut text = vec![
        Line::from(vec![
            Span::styled("in  ", theme::dim()),
            Span::styled(
                dir.to_string(),
                Style::default()
                    .fg(theme::ACCENT2)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
    ];
    // Show a window of entries around the selection so the modal stays a sensible height.
    let max_rows = area.height.saturating_sub(8).max(4) as usize;
    let start = model.browse_selected.saturating_sub(max_rows / 2);
    for (i, name) in model
        .browse_entries
        .iter()
        .enumerate()
        .skip(start)
        .take(max_rows)
    {
        let label = if name == ".." {
            "..  (up)".to_string()
        } else {
            format!("{name}/")
        };
        let style = if i == model.browse_selected {
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::FG)
        };
        let marker = if i == model.browse_selected {
            "▶ "
        } else {
            "  "
        };
        text.push(Line::from(Span::styled(format!("{marker}{label}"), style)));
    }
    text.push(Line::from(""));
    text.push(Line::from(Span::styled(
        "↑↓ move · enter/→ open · ←/⌫ up · a add this folder · esc",
        theme::dim(),
    )));
    modal(f, area, " ADD WORKSPACE — BROWSE ", text);
}

/// One-key quick-launch palette (SUM-130): pick any agent — or a plain shell — for `launch_repo`.
fn render_launch(f: &mut Frame, area: Rect, model: &Model) {
    let repo = if model.launch_repo.is_empty() {
        "(no folder — press o to pick one)".to_string()
    } else {
        model.launch_repo.clone()
    };
    let mut text = vec![
        Line::from(vec![
            Span::styled("launch into  ", theme::dim()),
            Span::styled(repo, Style::default().fg(theme::ACCENT2)),
        ]),
        Line::from(""),
    ];
    for (i, (label, _provider, is_shell)) in LAUNCH_ITEMS.iter().enumerate() {
        let selected = i == model.launch_selected;
        let marker = if selected { "▶ " } else { "  " };
        let style = if selected {
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::FG)
        };
        let desc = if *is_shell {
            "  plain interactive shell"
        } else {
            ""
        };
        text.push(Line::from(vec![
            Span::styled(format!("{marker}{}. {label}", i + 1), style),
            Span::styled(desc.to_string(), theme::dim()),
        ]));
    }
    text.push(Line::from(""));
    text.push(Line::from(Span::styled(
        "1–5 or ↑↓+enter to launch · esc to cancel",
        theme::dim(),
    )));
    modal(f, area, " LAUNCH AGENT ", text);
}

/// Confirmation prompt for a destructive close action (SUM-130).
fn render_confirm(f: &mut Frame, area: Rect, model: &Model) {
    let (verb, id, note) = match &model.pending {
        Some(PendingAction::Terminate(id)) => (
            "Terminate",
            id.as_str(),
            "the agent is killed; a dirty worktree is preserved",
        ),
        Some(PendingAction::Forget(id)) => (
            "Remove",
            id.as_str(),
            "the agent is dropped from the list for good",
        ),
        None => ("", "", ""),
    };
    let text = vec![
        Line::from(Span::styled(
            format!("{verb} {id}?"),
            Style::default()
                .fg(theme::ERROR)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(note.to_string(), theme::dim())),
        Line::from(""),
        Line::from(Span::styled(
            "enter / y  yes      esc / n  no",
            theme::dim(),
        )),
    ];
    modal(f, area, " CONFIRM ", text);
}

fn render_help(f: &mut Frame, area: Rect) {
    let keys = [
        ("j / k", "move selection"),
        (
            "enter",
            "open agent (starts/restarts + attaches) · or launch into a workspace",
        ),
        ("c", "launch an agent or shell (quick-launch palette)"),
        ("x", "close: terminate a running agent or remove a dead one"),
        ("n", "new agent via the full form"),
        ("o", "browse for a folder to register as a workspace"),
        ("Ctrl-a d", "detach from an agent"),
        ("? ", "toggle this help"),
        ("q", "quit"),
    ];
    let text: Vec<Line> = keys
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(
                    format!("  {k:<10}"),
                    Style::default()
                        .fg(theme::ACCENT2)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled((*d).to_string(), Style::default().fg(theme::FG)),
            ])
        })
        .collect();
    modal(f, area, " HELP ", text);
}

fn render_status(f: &mut Frame, area: Rect, model: &Model) {
    let dot = if model.status.starts_with("daemon") && model.status.contains("unreachable") {
        Span::styled(" ● ", Style::default().fg(theme::ERROR))
    } else {
        Span::styled(" ● ", Style::default().fg(theme::SUCCESS))
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            dot,
            Span::styled(model.status.clone(), theme::dim()),
        ])),
        area,
    );
}

// --- helpers ---------------------------------------------------------------------------------

/// A bordered panel with a themed title.
fn panel(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::SURFACE))
        .title(Span::styled(title.to_string(), theme::title()))
}

/// Render a centered modal box over `area`.
fn modal(f: &mut Frame, area: Rect, title: &str, text: Vec<Line>) {
    let w = area.width.min(72);
    let h = (text.len() as u16 + 2).min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(text)
            .block(panel(title))
            .style(Style::default().bg(theme::SURFACE)),
        rect,
    );
}

fn kv<'a>(key: &'a str, value: &str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{key:<12}"), theme::dim()),
        Span::styled(value.to_string(), Style::default().fg(theme::FG)),
    ])
}

/// Human name for a task's repository: the registered workspace name if known, else the short id.
fn workspace_name(model: &Model, repo_id: &str) -> String {
    model
        .data
        .workspaces
        .iter()
        .find(|w| w.id == repo_id)
        .map(|w| w.name.clone())
        .unwrap_or_else(|| short(repo_id, 16))
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Pad a single-span-friendly line with trailing spaces to `width` so a row highlight fills it.
fn pad_line(mut line: Line<'static>, width: usize) -> Line<'static> {
    let used: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if used < width {
        line.spans.push(Span::raw(" ".repeat(width - used)));
    }
    line
}

/// A one-column-inset copy of an area (keeps header text off the border edge).
fn pad(area: Rect) -> Rect {
    Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(1),
        height: area.height,
    }
}

fn centered_v(area: Rect) -> Rect {
    Rect {
        x: area.x + 1,
        y: area.y + area.height / 2,
        width: area.width.saturating_sub(2),
        height: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Data, View};
    use memmux_proto::{DaemonInfo, TaskView, WorkspaceView};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn model() -> Model {
        let mut m = Model::default();
        m.set_data(Data {
            daemon: Some(DaemonInfo {
                protocol_version: "0.1.0".into(),
                daemon_version: "0.2.0".into(),
                task_count: 1,
                agent_budget_bytes: 20 * 1024 * 1024 * 1024,
            }),
            workspaces: vec![WorkspaceView {
                id: "repo_a".into(),
                path: "/src/product".into(),
                name: "product".into(),
                created_at_ms: 0,
                task_count: 1,
            }],
            tasks: vec![TaskView {
                id: "task_abc".into(),
                title: "Refactor auth".into(),
                provider: "claude-code".into(),
                state: "ACTIVE".into(),
                repository: "repo_a".into(),
                base_branch: "main".into(),
                created_at_ms: 0,
                updated_at_ms: 0,
            }],
            ..Default::default()
        });
        m
    }

    fn text_of(model: &Model) -> String {
        let mut t = Terminal::new(TestBackend::new(100, 24)).unwrap();
        t.draw(|f| render(f, model)).unwrap();
        let buf = t.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn home_shows_header_sidebar_and_agent() {
        let s = text_of(&model());
        assert!(s.contains("MemMux"));
        assert!(s.contains("memory budget"));
        assert!(s.contains("WORKSPACES"));
        assert!(s.contains("product")); // workspace name in sidebar
        assert!(s.contains("Refactor auth")); // agent title
    }

    #[test]
    fn no_numbered_tab_bar() {
        let s = text_of(&model());
        assert!(!s.contains("1:Dashboard"));
        assert!(!s.contains("2:Tasks"));
        assert!(!s.contains("Timeline"));
    }

    #[test]
    fn selecting_an_agent_shows_the_interactive_hint() {
        let mut m = model();
        m.selected = 1; // the agent row
        let s = text_of(&m);
        assert!(s.contains("interactive session"));
    }

    #[test]
    fn help_lists_keys() {
        let mut m = model();
        m.view = View::Help;
        let s = text_of(&m);
        assert!(s.contains("HELP"));
        assert!(s.contains("detach"));
    }

    #[test]
    fn new_task_modal_renders() {
        let mut m = model();
        m.view = View::NewTask;
        let s = text_of(&m);
        assert!(s.contains("NEW AGENT"));
        assert!(s.contains("provider"));
    }

    #[test]
    fn launch_palette_lists_every_agent_and_a_shell() {
        let mut m = model();
        m.view = View::Launch;
        m.launch_repo = "/src/product".into();
        let s = text_of(&m);
        assert!(s.contains("LAUNCH AGENT"));
        assert!(s.contains("claude-code"));
        assert!(s.contains("codex"));
        assert!(s.contains("shell"));
        assert!(s.contains("/src/product"));
    }

    #[test]
    fn confirm_modal_names_the_action() {
        let mut m = model();
        m.view = View::Confirm;
        m.pending = Some(PendingAction::Terminate("task_abc".into()));
        let s = text_of(&m);
        assert!(s.contains("CONFIRM"));
        assert!(s.contains("Terminate task_abc"));
    }

    #[test]
    fn folder_browser_lists_entries() {
        let mut m = model();
        m.view = View::OpenFolder;
        m.browse_dir = "/src".into();
        m.browse_entries = vec!["..".into(), "product".into()];
        let s = text_of(&m);
        assert!(s.contains("BROWSE"));
        assert!(s.contains("/src"));
        assert!(s.contains("product/"));
    }
}
