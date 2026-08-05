//! Giant-line truncation (SUM-54 / §8.4).
//!
//! A single pathological line (a minified bundle, a base64 blob) must not blow the resident
//! byte cap. Lines over the cap are truncated to a head, and the full content is handed back to
//! the caller to spill to a disk artifact.

/// The result of applying the per-line size cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TruncateOutcome {
    /// The line fit within the cap; kept as-is.
    Kept(String),
    /// The line exceeded the cap. `head` is retained in the buffer; `full` must be persisted to
    /// a disk artifact by the caller, then referenced from the stored line.
    Truncated {
        /// Retained head of the line.
        head: String,
        /// Total byte length of the original line.
        full_bytes: u64,
        /// The full original content, to be written to an artifact.
        full: String,
    },
}

/// Truncate `line` to at most `max_line_bytes` of retained head.
///
/// The head is cut on a UTF-8 char boundary at or below the cap so the retained string is
/// always valid. A `max_line_bytes` of 0 is treated as 1.
pub fn truncate_line(line: &str, max_line_bytes: usize) -> TruncateOutcome {
    if line.len() <= max_line_bytes {
        return TruncateOutcome::Kept(line.to_string());
    }
    let cap = max_line_bytes.max(1);
    // Find the largest char boundary <= cap.
    let mut end = cap.min(line.len());
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    TruncateOutcome::Truncated {
        head: line[..end].to_string(),
        full_bytes: line.len() as u64,
        full: line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_line_is_kept() {
        assert_eq!(
            truncate_line("hello", 64),
            TruncateOutcome::Kept("hello".into())
        );
    }

    #[test]
    fn long_line_is_truncated_with_full_content() {
        let line = "A".repeat(1000);
        match truncate_line(&line, 100) {
            TruncateOutcome::Truncated {
                head,
                full_bytes,
                full,
            } => {
                assert_eq!(head.len(), 100);
                assert_eq!(full_bytes, 1000);
                assert_eq!(full.len(), 1000);
            }
            _ => panic!("expected truncation"),
        }
    }

    #[test]
    fn truncation_respects_utf8_boundaries() {
        // "é" is 2 bytes; a cap landing mid-char must back off to a boundary.
        let line = "é".repeat(50); // 100 bytes
        match truncate_line(&line, 5) {
            TruncateOutcome::Truncated { head, .. } => {
                assert!(head.len() <= 5);
                // Must be valid UTF-8 (guaranteed by String), and an even byte length here.
                assert_eq!(head.len() % 2, 0);
            }
            _ => panic!("expected truncation"),
        }
    }
}
