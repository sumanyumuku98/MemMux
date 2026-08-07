//! The MemMux TUI theme (SUM-126): one place for every colour + common styles, so the UI has a
//! cohesive, modern look. The palette is derived from the brand icon — a violet→cyan gradient on
//! a deep dark background.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders};

/// Deep background.
pub const BG: Color = Color::Rgb(0x0d, 0x11, 0x17);
/// A dim scrim painted over the body behind a modal to fake depth/blur (SUM-131).
pub const SCRIM: Color = Color::Rgb(0x08, 0x0a, 0x0e);
/// Panel / sidebar surface (slightly lifted from the background).
pub const SURFACE: Color = Color::Rgb(0x16, 0x1b, 0x22);
/// Soft highlight background for the selected row.
pub const SELECT_BG: Color = Color::Rgb(0x24, 0x2b, 0x38);
/// Primary foreground text.
pub const FG: Color = Color::Rgb(0xc9, 0xd1, 0xd9);
/// Muted / secondary text.
pub const DIM: Color = Color::Rgb(0x6e, 0x76, 0x81);
/// Primary accent (brand violet).
pub const ACCENT: Color = Color::Rgb(0x7c, 0x5c, 0xff);
/// Secondary accent (brand cyan).
pub const ACCENT2: Color = Color::Rgb(0x22, 0xd3, 0xee);
/// Success / running.
pub const SUCCESS: Color = Color::Rgb(0x3f, 0xb9, 0x50);
/// Warning / waiting.
pub const WARN: Color = Color::Rgb(0xd2, 0x99, 0x22);
/// Error / failed.
pub const ERROR: Color = Color::Rgb(0xf8, 0x51, 0x49);

/// Base text style on the app background.
pub fn base() -> Style {
    Style::default().fg(FG).bg(BG)
}

/// Muted style for hints and secondary labels.
pub fn dim() -> Style {
    Style::default().fg(DIM)
}

/// Section title / heading style (violet, bold).
pub fn title() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// Selected-row style: soft surface highlight with a bright foreground.
pub fn selected() -> Style {
    Style::default()
        .fg(Color::White)
        .bg(SELECT_BG)
        .add_modifier(Modifier::BOLD)
}

/// Colour for a task/agent state label.
pub fn state_color(state: &str) -> Color {
    match state {
        "ACTIVE" | "TOOL_RUNNING" => SUCCESS,
        "WAITING_USER" => WARN,
        "FAILED" | "TERMINATED" | "TERMINATING" => ERROR,
        "HIBERNATED" | "IDLE" | "BLOCKED" => DIM,
        "RECYCLING" | "RESUMING" | "CHECKPOINTING" => ACCENT2,
        _ => FG, // QUEUED / CREATED / ADMITTING / STARTING
    }
}

/// A rounded-border [`Block`] with a themed border colour (SUM-131). Pass `ACCENT` for a focused
/// panel, `SURFACE`/`DIM` otherwise.
pub fn rounded(border: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
}

/// Linearly interpolate between two `Rgb` colours; `t` is clamped to `[0,1]`. Non-`Rgb` inputs
/// fall back to `a` (the theme palette is all `Rgb`).
pub fn lerp_rgb(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) else {
        return a;
    };
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
}

/// Render `text` as per-character [`Span`]s whose foreground sweeps `from`→`to` — a truecolor
/// gradient for the header wordmark (SUM-131).
pub fn gradient_line(text: &str, from: Color, to: Color) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len().max(1);
    chars
        .into_iter()
        .enumerate()
        .map(|(i, ch)| {
            let t = i as f32 / (n.saturating_sub(1).max(1)) as f32;
            Span::styled(
                ch.to_string(),
                Style::default()
                    .fg(lerp_rgb(from, to, t))
                    .add_modifier(Modifier::BOLD),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_hits_endpoints_and_midpoint() {
        let a = Color::Rgb(0, 0, 0);
        let b = Color::Rgb(100, 200, 50);
        assert_eq!(lerp_rgb(a, b, 0.0), a);
        assert_eq!(lerp_rgb(a, b, 1.0), b);
        assert_eq!(lerp_rgb(a, b, 0.5), Color::Rgb(50, 100, 25));
        // Clamps out-of-range t.
        assert_eq!(lerp_rgb(a, b, -1.0), a);
        assert_eq!(lerp_rgb(a, b, 2.0), b);
    }

    #[test]
    fn gradient_line_spans_every_char_and_ends_on_the_target() {
        let spans = gradient_line("◆ MemMux", ACCENT, ACCENT2);
        assert_eq!(spans.len(), "◆ MemMux".chars().count());
        assert_eq!(spans.last().unwrap().style.fg, Some(ACCENT2));
    }
}
