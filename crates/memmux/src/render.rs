//! Rendering for each view. Pure ratatui drawing over the [`Model`]; no I/O.
//!
//! Terminal-pane damage deltas (SUM-84) are handled by ratatui's built-in double-buffered
//! diffing — only cells that changed between frames are written to the terminal — so the pane
//! widget simply renders the current screen grid and lets the backend compute the delta.

use crate::app::{FormField, Model, View, PROVIDERS};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Row, Table};
use ratatui::Frame;

const ACCENT: Color = Color::Cyan;

/// Draw the whole UI for the current model.
pub fn render(f: &mut Frame, model: &Model) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    render_tabs(f, chunks[0], model);
    match model.view {
        View::Dashboard => render_dashboard(f, chunks[1], model),
        View::Tasks => render_tasks(f, chunks[1], model),
        View::Queue => render_queue(f, chunks[1], model),
        View::Timeline => render_timeline(f, chunks[1], model),
        View::NewTask => render_new_task(f, chunks[1], model),
        View::Term => render_term(f, chunks[1], model),
        View::Help => render_help(f, chunks[1]),
    }
    render_status(f, chunks[2], model);
}

fn render_tabs(f: &mut Frame, area: Rect, model: &Model) {
    let tabs = [
        ("1", "Dashboard", View::Dashboard),
        ("2", "Tasks", View::Tasks),
        ("3", "Queue", View::Queue),
        ("4", "Timeline", View::Timeline),
    ];
    let mut spans = vec![Span::styled(
        "MemMux ",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )];
    for (key, label, view) in tabs {
        let style = if model.view == view {
            Style::default().fg(Color::Black).bg(ACCENT)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(format!(" {key}:{label} "), style));
    }
    spans.push(Span::styled(
        "  n:new  ?:help  q:quit",
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_status(f: &mut Frame, area: Rect, model: &Model) {
    let budget = model
        .data
        .daemon
        .as_ref()
        .map(|d| format!("budget {} MiB", d.agent_budget_bytes / (1024 * 1024)))
        .unwrap_or_else(|| "budget —".to_string());
    let line = Line::from(vec![
        Span::styled(" ● ", Style::default().fg(Color::Green)),
        Span::raw(model.status.clone()),
        Span::raw("   "),
        Span::styled(budget, Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// The SYSTEM / AGENT BUDGET / PRESSURE header from Appendix A.
fn render_dashboard(f: &mut Frame, area: Rect, model: &Model) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    let (pct, stage) = model
        .data
        .pressure
        .as_ref()
        .map(|p| (p.utilization_pct.min(100) as u16, p.stage.clone()))
        .unwrap_or((0, "—".to_string()));
    let gauge = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" AGENT BUDGET · PRESSURE {stage} ")),
        )
        .gauge_style(Style::default().fg(ACCENT))
        .percent(pct)
        .label(format!("{pct}% used"));
    f.render_widget(gauge, rows[0]);

    render_task_table(f, rows[1], model, " TASKS ");
}

fn render_tasks(f: &mut Frame, area: Rect, model: &Model) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);
    render_task_table(f, cols[0], model, " TASKS ");
    // Right pane: recent events for context (stands in for a live attach screen).
    let lines: Vec<String> = model
        .data
        .events
        .iter()
        .rev()
        .take(area.height.saturating_sub(2) as usize)
        .map(|e| format!("#{} {} [{}]", e.seq, e.event_type, e.source))
        .collect();
    render_term_pane(f, cols[1], " ACTIVITY ", &lines, 0);
}

fn render_task_table(f: &mut Frame, area: Rect, model: &Model, title: &str) {
    let header = Row::new(["ID", "STATE", "TITLE", "PROVIDER"]).style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = model
        .data
        .tasks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let selected =
                i == model.selected && matches!(model.view, View::Tasks | View::Dashboard);
            let style = if selected {
                Style::default().fg(Color::Black).bg(ACCENT)
            } else {
                Style::default().fg(state_color(&t.state))
            };
            Row::new([
                short(&t.id, 18),
                t.state.clone(),
                short(&t.title, 28),
                t.provider.clone(),
            ])
            .style(style)
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(20),
            Constraint::Length(13),
            Constraint::Min(10),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(title.to_string()),
    );
    f.render_widget(table, area);

    if model.data.tasks.is_empty() {
        let hint = Paragraph::new("no tasks — press 'n' to create one")
            .style(Style::default().fg(Color::DarkGray));
        let inner = Rect {
            x: area.x + 2,
            y: area.y + 1,
            width: area.width.saturating_sub(4),
            height: 1,
        };
        f.render_widget(hint, inner);
    }
}

fn render_queue(f: &mut Frame, area: Rect, model: &Model) {
    // Queue view: queued tasks with (placeholder) admission scores + wait reasons (SUM-88).
    let queued: Vec<ListItem> = model
        .data
        .tasks
        .iter()
        .filter(|t| t.state == "QUEUED")
        .map(|t| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<20}", short(&t.id, 18)),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(format!("{}  ", short(&t.title, 30))),
                Span::styled("waiting: admission", Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    let list = List::new(queued).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" QUEUE — admission scores + waits "),
    );
    f.render_widget(list, area);
}

fn render_timeline(f: &mut Frame, area: Rect, model: &Model) {
    let lines: Vec<String> = model
        .data
        .events
        .iter()
        .skip(model.scroll)
        .map(|e| {
            let base = format!(
                "#{:<5} {:<20} {:<10} {}",
                e.seq, e.event_type, e.category, e.source
            );
            // Surface the recycle ledger inline (SUM-97): reclaimed memory + resume mode.
            match recycle_ledger_summary(e) {
                Some(summary) => format!("{base}  — {summary}"),
                None => base,
            }
        })
        .collect();
    render_term_pane(
        f,
        area,
        " RESOURCE / EVENT TIMELINE (j/k to scroll) ",
        &lines,
        0,
    );
}

/// If `e` is a `runtime_recycled` event, format its ledger payload as a compact summary
/// (reclaimed MiB + resume mode). Returns `None` for other events or unparsable payloads.
fn recycle_ledger_summary(e: &memmux_proto::EventView) -> Option<String> {
    if e.event_type != "runtime_recycled" {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(e.payload_json.as_deref()?).ok()?;
    let reclaimed = v.get("reclaimed_bytes")?.as_i64()?;
    let mode = v.get("resume_mode").and_then(|m| m.as_str()).unwrap_or("?");
    let mib = reclaimed / (1024 * 1024);
    if reclaimed > 0 {
        Some(format!("reclaimed {mib} MiB, resume={mode}"))
    } else {
        Some(format!("no measurable reclamation, resume={mode}"))
    }
}

fn render_new_task(f: &mut Frame, area: Rect, model: &Model) {
    let form = &model.form;
    let field_line = |label: &str, value: &str, focused: bool| {
        let marker = if focused { "▶ " } else { "  " };
        let style = if focused {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        Line::from(vec![
            Span::styled(format!("{marker}{label:<10}"), style),
            Span::raw(value.to_string()),
            if focused {
                Span::styled("▏", Style::default().fg(ACCENT))
            } else {
                Span::raw("")
            },
        ])
    };
    let provider_line = {
        let focused = form.field == FormField::Provider;
        let marker = if focused { "▶ " } else { "  " };
        let picker: Vec<Span> = PROVIDERS
            .iter()
            .enumerate()
            .map(|(i, p)| {
                if i == form.provider_idx {
                    Span::styled(
                        format!("[{p}] "),
                        Style::default().fg(Color::Black).bg(ACCENT),
                    )
                } else {
                    Span::styled(format!("{p} "), Style::default().fg(Color::DarkGray))
                }
            })
            .collect();
        let mut spans = vec![Span::styled(
            format!("{marker}{:<10}", "provider"),
            Style::default(),
        )];
        spans.extend(picker);
        Line::from(spans)
    };
    let body = vec![
        field_line("title", &form.title, form.field == FormField::Title),
        field_line("repo", &form.repo, form.field == FormField::Repo),
        provider_line,
        field_line("base", &form.base, form.field == FormField::Base),
        Line::from(""),
        Line::from(Span::styled(
            "Tab/↑↓ move · ←→ pick provider · Enter create · Esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title(" NEW TASK ")),
        area,
    );
}

fn render_help(f: &mut Frame, area: Rect) {
    let keys = [
        ("1–4", "switch views: Dashboard / Tasks / Queue / Timeline"),
        ("j / k or ↑ / ↓", "move selection (or scroll the timeline)"),
        ("n", "new task (form with provider picker)"),
        ("Enter", "confirm (submit the new-task form)"),
        ("Esc", "back to the dashboard"),
        ("? / h", "this help"),
        ("q", "quit"),
    ];
    let items: Vec<ListItem> = keys
        .iter()
        .map(|(k, d)| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{k:<16}"),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw(*d),
            ]))
        })
        .collect();
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" HELP / KEYMAP "),
        ),
        area,
    );
}

/// Live terminal view of the focused task: screen grid or scrollback history (SUM-84/85/86).
fn render_term(f: &mut Frame, area: Rect, model: &Model) {
    let id = model.focused_task.as_deref().unwrap_or("—");
    if model.show_history {
        let title = format!(" {id} — SCROLLBACK (h:live  a:attach  Esc:back) ");
        render_term_pane(f, area, &title, &model.history_rows, 0);
    } else {
        let title = format!(" {id} — LIVE (h:history  a:attach  r:refresh  Esc:back) ");
        let rows = if model.screen_rows.is_empty() {
            vec!["(starting… no output yet)".to_string()]
        } else {
            model.screen_rows.clone()
        };
        render_term_pane(f, area, &title, &rows, 0);
    }
}

/// The terminal-pane widget (SUM-84): renders `lines` in a bordered block starting at `scroll`.
fn render_term_pane(f: &mut Frame, area: Rect, title: &str, lines: &[String], scroll: usize) {
    let visible: Vec<Line> = lines
        .iter()
        .skip(scroll)
        .map(|l| Line::from(l.clone()))
        .collect();
    f.render_widget(
        Paragraph::new(visible).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title.to_string()),
        ),
        area,
    );
}

fn state_color(state: &str) -> Color {
    match state {
        "ACTIVE" | "TOOL_RUNNING" => Color::Green,
        "WAITING_USER" => Color::Yellow,
        "QUEUED" | "BLOCKED" => Color::Cyan,
        "HIBERNATED" | "IDLE" => Color::Blue,
        "FAILED" | "TERMINATED" | "TERMINATING" => Color::Red,
        _ => Color::Gray,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Data, Model};
    use memmux_proto::{DaemonInfo, EventView, PressureView, TaskView};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn buffer_text(model: &Model) -> String {
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
        terminal.draw(|f| render(f, model)).unwrap();
        let buf = terminal.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect()
    }

    fn sample_model(view: View) -> Model {
        let mut m = Model {
            view,
            ..Model::default()
        };
        m.set_data(Data {
            tasks: vec![TaskView {
                id: "task_abc".into(),
                title: "Refactor auth".into(),
                provider: "claude-code".into(),
                state: "QUEUED".into(),
                repository: "repo_1".into(),
                base_branch: "main".into(),
                created_at_ms: 0,
                updated_at_ms: 0,
            }],
            pressure: Some(PressureView {
                agent_budget_bytes: 20 * 1024 * 1024 * 1024,
                used_bytes: 0,
                utilization_pct: 0,
                stage: "Normal".into(),
            }),
            daemon: Some(DaemonInfo {
                protocol_version: "0.1.0".into(),
                daemon_version: "0.1.0".into(),
                task_count: 1,
                agent_budget_bytes: 20 * 1024 * 1024 * 1024,
            }),
            events: vec![
                EventView {
                    seq: 1,
                    task_id: Some("task_abc".into()),
                    ts_ms: 0,
                    category: "lifecycle".into(),
                    event_type: "task_created".into(),
                    severity: "info".into(),
                    source: "daemon".into(),
                    payload_json: None,
                },
                EventView {
                    seq: 2,
                    task_id: Some("task_abc".into()),
                    ts_ms: 0,
                    category: "lifecycle".into(),
                    event_type: "runtime_recycled".into(),
                    severity: "info".into(),
                    source: "daemon".into(),
                    payload_json: Some(
                        r#"{"rss_before":3221225472,"rss_after":1073741824,"reclaimed_bytes":2147483648,"resume_mode":"native","resume_latency_ms":120,"git_patch_hash":"abcd"}"#
                            .into(),
                    ),
                },
            ],
        });
        m
    }

    #[test]
    fn dashboard_renders_header_and_task() {
        let text = buffer_text(&sample_model(View::Dashboard));
        assert!(text.contains("MemMux"));
        assert!(text.contains("AGENT BUDGET"));
        assert!(text.contains("Refactor auth"));
        assert!(text.contains("Normal"));
    }

    #[test]
    fn tasks_view_shows_activity_events() {
        let text = buffer_text(&sample_model(View::Tasks));
        assert!(text.contains("task_created"));
        assert!(text.contains("ACTIVITY"));
    }

    #[test]
    fn queue_view_lists_queued_tasks() {
        let text = buffer_text(&sample_model(View::Queue));
        assert!(text.contains("QUEUE"));
        assert!(text.contains("waiting: admission"));
    }

    #[test]
    fn timeline_surfaces_the_recycle_ledger() {
        let text = buffer_text(&sample_model(View::Timeline));
        assert!(text.contains("runtime_recycled"));
        // The reclaimed memory + resume mode are surfaced inline (SUM-97).
        assert!(text.contains("reclaimed 2048 MiB"));
        assert!(text.contains("resume=native"));
    }

    #[test]
    fn new_task_form_renders_provider_picker() {
        let mut m = sample_model(View::NewTask);
        m.form.title = "T".into();
        let text = buffer_text(&m);
        assert!(text.contains("NEW TASK"));
        assert!(text.contains("claude-code"));
        assert!(text.contains("provider"));
    }

    #[test]
    fn help_view_lists_keys() {
        let text = buffer_text(&sample_model(View::Help));
        assert!(text.contains("HELP"));
        assert!(text.contains("new task"));
    }

    #[test]
    fn term_view_renders_live_screen() {
        let mut m = sample_model(View::Term);
        m.focused_task = Some("task_abc".into());
        m.screen_rows = vec!["hello from the pty".into()];
        let text = buffer_text(&m);
        assert!(text.contains("LIVE"));
        assert!(text.contains("hello from the pty"));
    }

    #[test]
    fn term_view_renders_scrollback() {
        let mut m = sample_model(View::Term);
        m.focused_task = Some("task_abc".into());
        m.show_history = true;
        m.history_rows = vec!["old-line-1".into()];
        let text = buffer_text(&m);
        assert!(text.contains("SCROLLBACK"));
        assert!(text.contains("old-line-1"));
    }
}
