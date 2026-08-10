//! Dependency-free SVG line-chart builder for the benchmark report figures (SUM-163).
//!
//! Hand-rolled SVG (no plotting crate) so a figure can be produced anywhere the harness runs and
//! committed next to the Markdown report. [`line_chart_svg`] emits a valid standalone
//! `<svg>…</svg>` document with axes, gridlines, a legend, and one `<polyline>` per series; all
//! caller-supplied text is XML-escaped.

use std::fmt::Write as _;

/// One named data series to plot, with its polyline colour.
#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    /// Legend label for this series.
    pub name: String,
    /// `(x, y)` data points in data space, in draw order.
    pub points: Vec<(f64, f64)>,
    /// SVG stroke colour (e.g. `"#1f77b4"`).
    pub color: String,
}

impl Series {
    /// Convenience constructor.
    pub fn new(name: impl Into<String>, points: Vec<(f64, f64)>, color: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            points,
            color: color.into(),
        }
    }
}

/// A palette of distinguishable colours for auto-assigning series strokes (Tableau-10 subset).
pub const PALETTE: [&str; 8] = [
    "#1f77b4", "#ff7f0e", "#2ca02c", "#d62728", "#9467bd", "#8c564b", "#e377c2", "#17becf",
];

/// Render a multi-series line chart as a standalone SVG document string.
///
/// The output always starts with `<svg` and ends with `</svg>`, has labelled axes with a handful
/// of gridlines, a legend keyed by series colour, and one `<polyline>` per non-empty series. Data
/// coordinates are mapped into the plot rectangle from the min/max across every series; a series
/// with a single point renders as a dot-sized segment. An empty `series` (or all-empty points)
/// still yields a valid chart with axes so a figure is never silently dropped.
pub fn line_chart_svg(title: &str, x_label: &str, y_label: &str, series: &[Series]) -> String {
    // Canvas geometry.
    let width = 720.0_f64;
    let height = 420.0_f64;
    let margin_left = 70.0_f64;
    let margin_right = 180.0_f64; // room for the legend
    let margin_top = 44.0_f64;
    let margin_bottom = 56.0_f64;
    let plot_w = width - margin_left - margin_right;
    let plot_h = height - margin_top - margin_bottom;
    let plot_x0 = margin_left;
    let plot_y0 = margin_top;
    let plot_x1 = margin_left + plot_w;
    let plot_y1 = margin_top + plot_h;

    // Data bounds across all series' points.
    let mut x_min = f64::INFINITY;
    let mut x_max = f64::NEG_INFINITY;
    let mut y_min = f64::INFINITY;
    let mut y_max = f64::NEG_INFINITY;
    for s in series {
        for &(x, y) in &s.points {
            if x.is_finite() {
                x_min = x_min.min(x);
                x_max = x_max.max(x);
            }
            if y.is_finite() {
                y_min = y_min.min(y);
                y_max = y_max.max(y);
            }
        }
    }
    // Fall back to a unit box when there is no data.
    if !x_min.is_finite() || !x_max.is_finite() {
        x_min = 0.0;
        x_max = 1.0;
    }
    if !y_min.is_finite() || !y_max.is_finite() {
        y_min = 0.0;
        y_max = 1.0;
    }
    // y axis starts at 0 for footprint-style charts and always has a non-zero span.
    if y_min > 0.0 {
        y_min = 0.0;
    }
    if (x_max - x_min).abs() < f64::EPSILON {
        x_max = x_min + 1.0;
    }
    if (y_max - y_min).abs() < f64::EPSILON {
        y_max = y_min + 1.0;
    }

    let sx = |x: f64| plot_x0 + (x - x_min) / (x_max - x_min) * plot_w;
    let sy = |y: f64| plot_y1 - (y - y_min) / (y_max - y_min) * plot_h;

    let mut out = String::new();
    let _ = write!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width:.0}\" height=\"{height:.0}\" \
viewBox=\"0 0 {width:.0} {height:.0}\" font-family=\"sans-serif\">"
    );
    // Background.
    let _ = write!(
        out,
        "<rect x=\"0\" y=\"0\" width=\"{width:.0}\" height=\"{height:.0}\" fill=\"#ffffff\"/>"
    );
    // Title.
    let _ = write!(
        out,
        "<text x=\"{tx:.1}\" y=\"24\" font-size=\"16\" font-weight=\"bold\" \
text-anchor=\"middle\">{title}</text>",
        tx = plot_x0 + plot_w / 2.0,
        title = escape_xml(title)
    );

    // Gridlines + tick labels (5 divisions each).
    const DIVS: usize = 5;
    for i in 0..=DIVS {
        let f = i as f64 / DIVS as f64;
        // Horizontal gridline (y).
        let gy = plot_y1 - f * plot_h;
        let yv = y_min + f * (y_max - y_min);
        let _ = write!(
            out,
            "<line x1=\"{x0:.1}\" y1=\"{gy:.1}\" x2=\"{x1:.1}\" y2=\"{gy:.1}\" \
stroke=\"#e6e6e6\" stroke-width=\"1\"/>",
            x0 = plot_x0,
            x1 = plot_x1
        );
        let _ = write!(
            out,
            "<text x=\"{lx:.1}\" y=\"{ly:.1}\" font-size=\"11\" text-anchor=\"end\" \
fill=\"#444\">{yv}</text>",
            lx = plot_x0 - 8.0,
            ly = gy + 4.0,
            yv = fmt_tick(yv)
        );
        // Vertical gridline (x).
        let gx = plot_x0 + f * plot_w;
        let xv = x_min + f * (x_max - x_min);
        let _ = write!(
            out,
            "<line x1=\"{gx:.1}\" y1=\"{y0:.1}\" x2=\"{gx:.1}\" y2=\"{y1:.1}\" \
stroke=\"#f0f0f0\" stroke-width=\"1\"/>",
            y0 = plot_y0,
            y1 = plot_y1
        );
        let _ = write!(
            out,
            "<text x=\"{gx:.1}\" y=\"{ly:.1}\" font-size=\"11\" text-anchor=\"middle\" \
fill=\"#444\">{xv}</text>",
            ly = plot_y1 + 18.0,
            xv = fmt_tick(xv)
        );
    }

    // Axes.
    let _ = write!(
        out,
        "<line x1=\"{x0:.1}\" y1=\"{y1:.1}\" x2=\"{x1:.1}\" y2=\"{y1:.1}\" stroke=\"#333\" \
stroke-width=\"1.5\"/>",
        x0 = plot_x0,
        x1 = plot_x1,
        y1 = plot_y1
    );
    let _ = write!(
        out,
        "<line x1=\"{x0:.1}\" y1=\"{y0:.1}\" x2=\"{x0:.1}\" y2=\"{y1:.1}\" stroke=\"#333\" \
stroke-width=\"1.5\"/>",
        x0 = plot_x0,
        y0 = plot_y0,
        y1 = plot_y1
    );

    // Axis labels.
    let _ = write!(
        out,
        "<text x=\"{lx:.1}\" y=\"{ly:.1}\" font-size=\"13\" text-anchor=\"middle\">{xl}</text>",
        lx = plot_x0 + plot_w / 2.0,
        ly = height - 12.0,
        xl = escape_xml(x_label)
    );
    let _ = write!(
        out,
        "<text x=\"18\" y=\"{ly:.1}\" font-size=\"13\" text-anchor=\"middle\" \
transform=\"rotate(-90 18 {ly:.1})\">{yl}</text>",
        ly = plot_y0 + plot_h / 2.0,
        yl = escape_xml(y_label)
    );

    // One polyline per series with data, plus a legend entry per series.
    for (i, s) in series.iter().enumerate() {
        let color = if s.color.is_empty() {
            PALETTE[i % PALETTE.len()]
        } else {
            s.color.as_str()
        };
        if !s.points.is_empty() {
            let pts = s
                .points
                .iter()
                .map(|&(x, y)| format!("{:.1},{:.1}", sx(x), sy(y)))
                .collect::<Vec<_>>()
                .join(" ");
            let _ = write!(
                out,
                "<polyline fill=\"none\" stroke=\"{c}\" stroke-width=\"2\" points=\"{pts}\"/>",
                c = escape_xml(color)
            );
        }
        // Legend swatch + label (always drawn so an empty series is still documented).
        let legend_x = plot_x1 + 16.0;
        let legend_y = plot_y0 + 8.0 + i as f64 * 20.0;
        let _ = write!(
            out,
            "<rect x=\"{lx:.1}\" y=\"{ry:.1}\" width=\"12\" height=\"12\" fill=\"{c}\"/>",
            lx = legend_x,
            ry = legend_y - 10.0,
            c = escape_xml(color)
        );
        let _ = write!(
            out,
            "<text x=\"{tx:.1}\" y=\"{ty:.1}\" font-size=\"12\" fill=\"#222\">{name}</text>",
            tx = legend_x + 18.0,
            ty = legend_y,
            name = escape_xml(&s.name)
        );
    }

    out.push_str("</svg>");
    out
}

/// Format an axis tick value compactly (integers without a trailing `.0`).
fn fmt_tick(v: f64) -> String {
    if (v.round() - v).abs() < 1e-6 {
        format!("{:.0}", v)
    } else {
        format!("{:.1}", v)
    }
}

/// Escape the five XML predefined entities so caller text is safe inside SVG.
fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_is_wellformed_with_a_polyline_per_series_and_the_title() {
        let series = vec![
            Series::new(
                "memmux",
                vec![(0.0, 10.0), (100.0, 20.0), (200.0, 15.0)],
                "#1f77b4",
            ),
            Series::new("tmux", vec![(0.0, 5.0), (100.0, 6.0)], "#ff7f0e"),
        ];
        let svg = line_chart_svg("Footprint over time", "elapsed (ms)", "MiB", &series);
        assert!(
            svg.starts_with("<svg"),
            "should start with <svg: {}",
            &svg[..20]
        );
        assert!(svg.trim_end().ends_with("</svg>"));
        assert_eq!(svg.matches("<polyline").count(), 2);
        assert!(svg.contains("Footprint over time"));
        assert!(svg.contains("elapsed (ms)"));
        assert!(svg.contains("MiB"));
    }

    #[test]
    fn empty_series_still_produces_valid_axes() {
        let svg = line_chart_svg("Empty", "x", "y", &[]);
        assert!(svg.starts_with("<svg"));
        assert!(svg.trim_end().ends_with("</svg>"));
        assert_eq!(svg.matches("<polyline").count(), 0);
        assert!(svg.contains("Empty"));
    }

    #[test]
    fn a_series_with_no_points_draws_a_legend_but_no_polyline() {
        let series = vec![Series::new("flat", vec![], "#2ca02c")];
        let svg = line_chart_svg("Swap", "elapsed (ms)", "bytes", &series);
        assert_eq!(svg.matches("<polyline").count(), 0);
        assert!(svg.contains("flat"));
    }

    #[test]
    fn special_characters_in_text_are_escaped() {
        let series = vec![Series::new("a<b>&\"'", vec![(0.0, 1.0)], "#333")];
        let svg = line_chart_svg("t<i>&tle", "x&y", "<z>", &series);
        assert!(svg.contains("t&lt;i&gt;&amp;tle"));
        assert!(svg.contains("a&lt;b&gt;&amp;&quot;&apos;"));
        assert!(!svg.contains("t<i>&tle"));
    }
}
