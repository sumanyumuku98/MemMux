//! Rendering for the Home view + modals (SUM-126/127). Pure ratatui drawing over the [`Model`];
//! no I/O. All colours come from [`crate::theme`] so the look stays cohesive.

use crate::app::{
    FormField, Model, NavItem, PendingAction, SidebarSection, View, WsRow, LAUNCH_ITEMS, PROVIDERS,
};
use crate::theme;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

/// Sidebar width in columns (shared with the runtime's pane-geometry math — SUM-132).
pub const SIDEBAR_WIDTH: u16 = 30;

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
        View::Help => render_help(f, chunks[1], model),
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

    // The wordmark sweeps violet→cyan (the brand gradient) across "◆ MemMux" (SUM-131).
    let mut spans = theme::gradient_line("◆ MemMux", theme::ACCENT, theme::ACCENT2);
    spans.extend([
        Span::styled("  memory budget ", theme::dim()),
        Span::styled(budget, Style::default().fg(theme::ACCENT2)),
        Span::styled(format!("  ·  {pct}% used · {stage}"), theme::dim()),
    ]);
    let left = Line::from(spans);
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
    // With panes open in the active group, the main area is the pane grid; otherwise the detail.
    if model.active_panes().is_some() {
        render_pane_grid(f, cols[1], model);
    } else {
        render_detail(f, cols[1], model);
    }
}

/// Render the active workspace group's panes into `area` (SUM-132/134): the tiling tree, or just
/// the focused pane when zoomed.
fn render_pane_grid(f: &mut Frame, area: Rect, model: &Model) {
    let Some(layout) = model.active_panes() else {
        return;
    };
    if model.zoomed {
        if let Some(id) = model.focused_pane() {
            render_pane(f, area, id, model);
            return;
        }
    }
    for (id, rect) in layout.leaf_rects(area) {
        render_pane(f, rect, &id, model);
    }
}

/// Render one agent pane: a rounded panel (accent border when focused) whose body is the agent's
/// live colored screen snapshot (SUM-132).
fn render_pane(f: &mut Frame, area: Rect, id: &str, model: &Model) {
    let focused = model.focused_pane() == Some(id);
    let task = model.data.tasks.iter().find(|t| t.id == id);
    let (title_text, state) = match task {
        Some(t) => (short(&t.title, 24), t.state.clone()),
        None => (short(id, 24), String::new()),
    };
    let border = if focused {
        theme::ACCENT
    } else {
        theme::state_color(&state)
    };
    let title = format!(" {title_text} · {state} ");
    let block = panel_accent(&title, border);
    let inner = block.inner(area);
    f.render_widget(block, area);

    match model.pane_screens.get(id) {
        Some(grid) if !grid.rows.is_empty() => {
            let lines: Vec<Line> = grid
                .rows
                .iter()
                .take(inner.height as usize)
                .map(|row| {
                    Line::from(
                        row.iter()
                            .take(inner.width as usize)
                            .map(|c| {
                                Span::styled(
                                    c.ch.to_string(),
                                    Style::default().fg(c.fg).bg(c.bg).add_modifier(c.mods),
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            f.render_widget(Paragraph::new(lines), inner);
            // Place the real terminal cursor in the focused, live pane.
            if focused && grid.alive {
                let (cr, cc) = grid.cursor;
                if cc < inner.width && cr < inner.height {
                    f.set_cursor_position((inner.x + cc, inner.y + cr));
                }
            }
        }
        _ => {
            let msg = if model.pane_screens.get(id).map(|g| g.alive) == Some(false) {
                format!("· agent exited — {} x to close ·", model.prefix.label())
            } else {
                "· starting… ·".to_string()
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(msg, theme::dim())))
                    .alignment(Alignment::Center),
                inner,
            );
        }
    }
}

/// Draw the two stacked sidebar panels (SUM-134): WORKSPACES on top, AGENTS below. The focused
/// section's selection glows bright; the other section's selection is shown dim.
fn render_sidebar(f: &mut Frame, area: Rect, model: &Model) {
    let (top, bottom) = model.sidebar_split(area);
    render_ws_panel(f, top, model);
    render_agents_panel(f, bottom, model);
}

/// The WORKSPACES panel: one row per registered workspace plus an "Other" row for ungrouped agents.
fn render_ws_panel(f: &mut Frame, area: Rect, model: &Model) {
    let focused = model.sidebar == SidebarSection::Workspaces;
    let inner_w = area.width.saturating_sub(2) as usize;
    let rows = model.ws_items();
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let selected = i == model.ws_selected;
            let line = match *row {
                WsRow::Workspace(wi) => {
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
                WsRow::Other => Line::from(Span::styled("▸ Other", theme::dim())),
            };
            selectable_row(line, selected, focused, inner_w)
        })
        .collect();

    let list = List::new(items).block(section_panel(" WORKSPACES ", focused));
    f.render_widget(list, area);

    if rows.is_empty() {
        let hint = Paragraph::new(Line::from(Span::styled(
            "press o to add a workspace",
            theme::dim(),
        )))
        .alignment(Alignment::Center);
        f.render_widget(hint, centered_v(area));
    }
}

/// The AGENTS panel: agents grouped under dim workspace headers. Only agent rows are selectable.
fn render_agents_panel(f: &mut Frame, area: Rect, model: &Model) {
    let focused = model.sidebar == SidebarSection::Agents;
    let nav = model.nav_items();
    let inner_w = area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = nav
        .iter()
        .enumerate()
        .map(|(i, item)| match *item {
            NavItem::Workspace(wi) => {
                let w = &model.data.workspaces[wi];
                // Headers are dim, non-selectable labels.
                let line = Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        short(&w.name, 20),
                        Style::default()
                            .fg(theme::ACCENT)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]);
                ListItem::new(pad_line(line, inner_w))
            }
            NavItem::Unregistered => {
                let line = Line::from(vec![Span::raw(" "), Span::styled("other", theme::dim())]);
                ListItem::new(pad_line(line, inner_w))
            }
            NavItem::Agent(ti) => {
                let t = &model.data.tasks[ti];
                let selected = i == model.agent_selected;
                let line = Line::from(vec![
                    Span::raw("  "),
                    Span::styled("● ", Style::default().fg(theme::state_color(&t.state))),
                    Span::styled(short(&t.title, 22), Style::default().fg(theme::FG)),
                ]);
                selectable_row(line, selected, focused, inner_w)
            }
        })
        .collect();

    let list = List::new(items).block(section_panel(" AGENTS ", focused));
    f.render_widget(list, area);

    if !nav.iter().any(|n| matches!(n, NavItem::Agent(_))) {
        let hint = Paragraph::new(Line::from(Span::styled(
            "press c to launch an agent",
            theme::dim(),
        )))
        .alignment(Alignment::Center);
        f.render_widget(hint, centered_v(area));
    }
}

/// Style a selectable sidebar row (SUM-134): a bright glow + highlight when it's the focused
/// section's selection, a dim glow when it's the unfocused section's selection.
fn selectable_row(
    mut line: Line<'static>,
    selected: bool,
    focused: bool,
    width: usize,
) -> ListItem<'static> {
    let style = if selected && focused {
        theme::selected()
    } else {
        Style::default()
    };
    // A left glow bar marks the selected row (SUM-131); a space keeps others aligned.
    let bar = if selected {
        let color = if focused {
            theme::ACCENT2
        } else {
            theme::SURFACE
        };
        Span::styled("▐", Style::default().fg(color))
    } else {
        Span::raw(" ")
    };
    line.spans.insert(0, bar);
    ListItem::new(pad_line(line, width)).style(style)
}

/// A detail row descriptor: whichever sidebar section is focused decides what to show (SUM-134).
enum DetailFocus {
    Agent(usize),
    Workspace(usize),
    None,
}

fn detail_focus(model: &Model) -> DetailFocus {
    match model.sidebar {
        SidebarSection::Agents => match model.selected_nav() {
            Some(NavItem::Agent(ti)) => DetailFocus::Agent(ti),
            _ => DetailFocus::None,
        },
        SidebarSection::Workspaces => match model.selected_ws() {
            Some(WsRow::Workspace(wi)) => DetailFocus::Workspace(wi),
            _ => DetailFocus::None,
        },
    }
}

fn render_detail(f: &mut Frame, area: Rect, model: &Model) {
    let body: Vec<Line> = match detail_focus(model) {
        DetailFocus::Agent(ti) => {
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
                    format!(
                        "Enter → open as a pane ({} o to return to the sidebar)",
                        model.prefix.label()
                    ),
                    Style::default().fg(theme::ACCENT2),
                )),
            ]
        }
        DetailFocus::Workspace(wi) => {
            let w = &model.data.workspaces[wi];
            vec![
                kv("workspace", &w.name),
                kv("path", &w.path),
                kv("agents", &w.task_count.to_string()),
                Line::from(""),
                Line::from(Span::styled(
                    "Enter / c → launch an agent here",
                    Style::default().fg(theme::ACCENT2),
                )),
            ]
        }
        DetailFocus::None => vec![
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

fn render_help(f: &mut Frame, area: Rect, model: &Model) {
    let p = model.prefix.label(); // configurable leader, e.g. "Ctrl-b"
    let keys: Vec<(String, String)> = vec![
        (
            "tab".into(),
            "switch sidebar section (WORKSPACES ⇆ AGENTS)".into(),
        ),
        (
            "j / k".into(),
            "move selection within the focused section".into(),
        ),
        (
            "enter".into(),
            "AGENTS: open the agent as a pane · WORKSPACES: launch into it".into(),
        ),
        (
            "c".into(),
            "launch an agent or shell (quick-launch palette)".into(),
        ),
        (
            "x".into(),
            "sidebar: terminate a running agent or remove a dead one".into(),
        ),
        ("n".into(), "new agent via the full form".into()),
        (
            "o".into(),
            "browse for a folder to register as a workspace".into(),
        ),
        ("—— panes ——".into(), format!("(focus a pane, then {p} …)")),
        (format!("{p} h/j/k/l"), "move focus between panes".into()),
        (
            format!("{p} v / -"),
            "split: launch a new pane right / down".into(),
        ),
        (format!("{p} z"), "zoom the focused pane".into()),
        (format!("{p} x"), "close the focused pane".into()),
        (format!("{p} o / d"), "return focus to the sidebar".into()),
        (
            format!("{p} {p}"),
            "send a literal leader to the agent".into(),
        ),
        ("? ".into(), "toggle this help".into()),
        ("q".into(), "quit".into()),
        (
            "config".into(),
            "set the leader in ~/.memmux/config.toml → prefix = \"ctrl-b\"".into(),
        ),
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

/// A rounded, themed panel with a title (SUM-131). Border defaults to the surface colour.
fn panel(title: &str) -> Block<'static> {
    panel_accent(title, theme::SURFACE)
}

/// A rounded panel whose border uses `border` — e.g. `ACCENT` for a focused pane (SUM-131).
fn panel_accent(title: &str, border: ratatui::style::Color) -> Block<'static> {
    theme::rounded(border).title(Span::styled(title.to_string(), theme::title()))
}

/// A sidebar section panel (SUM-134): an accent border marks the focused section, else the surface.
fn section_panel(title: &str, focused: bool) -> Block<'static> {
    let border = if focused {
        theme::ACCENT
    } else {
        theme::SURFACE
    };
    panel_accent(title, border)
}

/// Render a centered modal box over `area`, dimming the body behind it with a scrim so the modal
/// pops (a fake for the "background blur" we can't do in a terminal — SUM-131).
fn modal(f: &mut Frame, area: Rect, title: &str, text: Vec<Line>) {
    // Scrim: repaint the whole body area in a darker-than-background colour.
    f.render_widget(
        Block::default().style(Style::default().bg(theme::SCRIM)),
        area,
    );

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
    fn selecting_an_agent_shows_the_open_pane_hint() {
        use crate::app::SidebarSection;
        let mut m = model();
        m.sidebar = SidebarSection::Agents;
        m.agent_selected = 1; // the agent row (0 is the workspace header)
        let s = text_of(&m);
        assert!(s.contains("open as a pane"));
    }

    #[test]
    fn sidebar_shows_both_sections() {
        let s = text_of(&model());
        assert!(s.contains("WORKSPACES"));
        assert!(s.contains("AGENTS"));
    }

    #[test]
    fn only_the_active_workspace_group_is_rendered() {
        use crate::app::Data;
        let mut m = Model::default();
        m.set_data(Data {
            workspaces: vec![
                WorkspaceView {
                    id: "repo_a".into(),
                    path: "/src/a".into(),
                    name: "alpha".into(),
                    created_at_ms: 0,
                    task_count: 1,
                },
                WorkspaceView {
                    id: "repo_b".into(),
                    path: "/src/b".into(),
                    name: "beta".into(),
                    created_at_ms: 0,
                    task_count: 1,
                },
            ],
            tasks: vec![
                TaskView {
                    id: "a1".into(),
                    title: "Alpha agent".into(),
                    provider: "codex".into(),
                    state: "ACTIVE".into(),
                    repository: "repo_a".into(),
                    base_branch: "main".into(),
                    created_at_ms: 0,
                    updated_at_ms: 0,
                },
                TaskView {
                    id: "b1".into(),
                    title: "Beta agent".into(),
                    provider: "codex".into(),
                    state: "ACTIVE".into(),
                    repository: "repo_b".into(),
                    base_branch: "main".into(),
                    created_at_ms: 0,
                    updated_at_ms: 0,
                },
            ],
            ..Default::default()
        });
        m.open_pane("a1"); // repo_a group
        m.open_pane("b1"); // repo_b group — now the active group
                           // Distinct screen content per pane; only pane bodies (never the sidebar) show it.
        let cell = |ch: char| crate::app::GridCell {
            ch,
            fg: theme::FG,
            bg: theme::BG,
            mods: Modifier::empty(),
        };
        m.pane_screens.insert(
            "a1".into(),
            crate::app::StyledGrid {
                rows: vec![vec![cell('A'), cell('A'), cell('A')]],
                cursor: (0, 0),
                alive: true,
            },
        );
        m.pane_screens.insert(
            "b1".into(),
            crate::app::StyledGrid {
                rows: vec![vec![cell('B'), cell('B'), cell('B')]],
                cursor: (0, 0),
                alive: true,
            },
        );
        let s = text_of(&m);
        // Only the active group's pane (b1) renders its live screen; a1's screen is hidden.
        assert!(s.contains("BBB"));
        assert!(!s.contains("AAA"));
    }

    #[test]
    fn help_lists_keys() {
        let mut m = model();
        m.view = View::Help;
        let s = text_of(&m);
        assert!(s.contains("HELP"));
        assert!(s.contains("panes"));
    }

    #[test]
    fn open_pane_renders_the_agent_screen_with_focus_border() {
        use crate::app::{GridCell, StyledGrid};
        let mut m = model();
        m.open_pane("task_abc");
        m.pane_screens.insert(
            "task_abc".into(),
            StyledGrid {
                rows: vec![vec![
                    GridCell {
                        ch: 'h',
                        fg: theme::FG,
                        bg: theme::BG,
                        mods: ratatui::style::Modifier::empty(),
                    },
                    GridCell {
                        ch: 'i',
                        fg: theme::FG,
                        bg: theme::BG,
                        mods: ratatui::style::Modifier::empty(),
                    },
                ]],
                cursor: (0, 2),
                alive: true,
            },
        );
        let s = text_of(&m);
        // The sidebar stays visible AND the agent's live screen renders in the pane.
        assert!(s.contains("WORKSPACES"));
        assert!(s.contains("hi"));
        assert!(s.contains("Refactor auth")); // agent title in the pane border
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
