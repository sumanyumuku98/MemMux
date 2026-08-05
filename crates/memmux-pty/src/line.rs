//! The unit of stored terminal output.
//!
//! A [`StoredLine`] is deliberately compact so the resident buffer's memory is a function of
//! the cap, not of how much output has scrolled past. Repeated runs and oversized lines are
//! represented by fixed-size variants rather than raw bytes (§8.2, §8.4).

use serde::{Deserialize, Serialize};

/// Fixed per-entry bookkeeping overhead (enum tag, counters, pointers) charged to every line
/// so the byte cap accounts for structure, not just payload.
pub const LINE_OVERHEAD_BYTES: usize = 24;

/// One logical line held in the bounded buffer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredLine {
    /// A normal line of text.
    Text {
        /// The line content (without the trailing newline).
        text: String,
    },
    /// A run of `count` consecutive identical lines collapsed to one entry (SUM-53).
    Repeated {
        /// The repeated line content.
        text: String,
        /// How many identical lines this entry represents (>= 2).
        count: u64,
    },
    /// A line that exceeded the per-line size cap, truncated to `head` with the full content
    /// spilled to a disk artifact (SUM-54).
    Truncated {
        /// The retained head of the line.
        head: String,
        /// Total byte length of the original line.
        full_bytes: u64,
        /// Reference to the on-disk artifact holding the full line.
        artifact_ref: String,
    },
}

impl StoredLine {
    /// Approximate resident memory this entry occupies (payload + fixed overhead).
    pub fn resident_bytes(&self) -> usize {
        let payload = match self {
            StoredLine::Text { text } => text.len(),
            StoredLine::Repeated { text, .. } => text.len(),
            StoredLine::Truncated {
                head, artifact_ref, ..
            } => head.len() + artifact_ref.len(),
        };
        payload + LINE_OVERHEAD_BYTES
    }

    /// How many terminal rows this entry represents when rendered.
    pub fn rendered_rows(&self) -> u64 {
        match self {
            StoredLine::Text { .. } | StoredLine::Truncated { .. } => 1,
            StoredLine::Repeated { count, .. } => *count,
        }
    }

    /// The text used to compare two lines for collapse purposes.
    pub fn collapse_key(&self) -> Option<&str> {
        match self {
            StoredLine::Text { text } => Some(text),
            StoredLine::Repeated { text, .. } => Some(text),
            // Truncated lines are never collapsed (their heads may coincide by chance).
            StoredLine::Truncated { .. } => None,
        }
    }

    /// Render the entry for display (collapsed runs show a `×N` suffix).
    pub fn render(&self) -> String {
        match self {
            StoredLine::Text { text } => text.clone(),
            StoredLine::Repeated { text, count } => format!("{text} ×{count}"),
            StoredLine::Truncated {
                head,
                full_bytes,
                artifact_ref,
            } => {
                format!("{head}…(truncated {full_bytes} bytes, see {artifact_ref})")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resident_bytes_include_overhead() {
        let l = StoredLine::Text {
            text: "hello".into(),
        };
        assert_eq!(l.resident_bytes(), 5 + LINE_OVERHEAD_BYTES);
    }

    #[test]
    fn rendered_rows_reflect_collapse() {
        assert_eq!(StoredLine::Text { text: "x".into() }.rendered_rows(), 1);
        assert_eq!(
            StoredLine::Repeated {
                text: "x".into(),
                count: 9
            }
            .rendered_rows(),
            9
        );
    }

    #[test]
    fn render_formats_each_variant() {
        assert_eq!(StoredLine::Text { text: "hi".into() }.render(), "hi");
        assert_eq!(
            StoredLine::Repeated {
                text: "tick".into(),
                count: 3
            }
            .render(),
            "tick ×3"
        );
        let t = StoredLine::Truncated {
            head: "AAAA".into(),
            full_bytes: 9000,
            artifact_ref: "art_1".into(),
        };
        assert!(t.render().contains("truncated 9000 bytes"));
    }

    #[test]
    fn truncated_lines_never_collapse() {
        let t = StoredLine::Truncated {
            head: "h".into(),
            full_bytes: 10,
            artifact_ref: "a".into(),
        };
        assert!(t.collapse_key().is_none());
    }
}
