//! Bounded ring buffer with line + byte caps and repeated-line collapse (SUM-50, SUM-53).
//!
//! The buffer keeps only a recent window resident. Both a line-count cap and a byte cap are
//! enforced; whichever binds first evicts the oldest entries, which are drained to the chunk
//! store. Consecutive identical lines collapse into a single counted entry so spinner/progress
//! floods do not grow memory (§8.2).

use crate::line::StoredLine;
use std::collections::VecDeque;

/// Caps governing the resident buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingCaps {
    /// Maximum resident line entries (spec suggests 2k–5k).
    pub max_lines: usize,
    /// Maximum resident bytes across all entries.
    pub max_bytes: usize,
}

impl Default for RingCaps {
    fn default() -> Self {
        Self {
            max_lines: 4096,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

/// A bounded, collapse-aware buffer of terminal lines.
#[derive(Debug)]
pub struct BoundedBuffer {
    caps: RingCaps,
    lines: VecDeque<StoredLine>,
    resident_bytes: usize,
    total_ingested: u64,
    evicted: Vec<StoredLine>,
}

impl BoundedBuffer {
    /// Create an empty buffer with the given caps.
    pub fn new(caps: RingCaps) -> Self {
        Self {
            caps,
            lines: VecDeque::new(),
            resident_bytes: 0,
            total_ingested: 0,
            evicted: Vec::new(),
        }
    }

    /// Push an already-prepared [`StoredLine`].
    ///
    /// If it is a `Text`/`Repeated` line identical to the last entry, the last entry is
    /// collapsed into a `Repeated` run instead of adding a new entry (SUM-53). Otherwise it is
    /// appended and the caps are enforced, evicting oldest entries as needed.
    pub fn push(&mut self, line: StoredLine) {
        self.total_ingested += 1;

        if let Some(key) = line.collapse_key().map(str::to_owned) {
            if let Some(last) = self.lines.back_mut() {
                if last.collapse_key() == Some(key.as_str()) {
                    // Collapse: fold into / extend a Repeated run without growing memory.
                    let added = match last {
                        StoredLine::Text { text } => {
                            let text = std::mem::take(text);
                            *last = StoredLine::Repeated { text, count: 2 };
                            // A Repeated entry is ~ the same size as the Text it replaced.
                            0isize
                        }
                        StoredLine::Repeated { count, .. } => {
                            *count += 1;
                            0isize
                        }
                        StoredLine::Truncated { .. } => unreachable!("collapse_key was Some"),
                    };
                    let _ = added;
                    return;
                }
            }
        }

        self.resident_bytes += line.resident_bytes();
        self.lines.push_back(line);
        self.enforce_caps();
    }

    /// Evict oldest entries until both caps are satisfied.
    fn enforce_caps(&mut self) {
        while self.lines.len() > self.caps.max_lines
            || (self.resident_bytes > self.caps.max_bytes && self.lines.len() > 1)
        {
            if let Some(old) = self.lines.pop_front() {
                self.resident_bytes = self.resident_bytes.saturating_sub(old.resident_bytes());
                self.evicted.push(old);
            } else {
                break;
            }
        }
    }

    /// Take the entries evicted since the last drain (hand these to the chunk store).
    pub fn drain_evicted(&mut self) -> Vec<StoredLine> {
        std::mem::take(&mut self.evicted)
    }

    /// Current resident byte estimate.
    pub fn resident_bytes(&self) -> usize {
        self.resident_bytes
    }

    /// Number of resident entries.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Total lines ever ingested (including collapsed and evicted).
    pub fn total_ingested(&self) -> u64 {
        self.total_ingested
    }

    /// Total terminal rows the resident entries represent (collapsed runs expand).
    pub fn rendered_rows(&self) -> u64 {
        self.lines.iter().map(StoredLine::rendered_rows).sum()
    }

    /// Iterate the resident entries oldest-to-newest.
    pub fn iter(&self) -> impl Iterator<Item = &StoredLine> {
        self.lines.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> StoredLine {
        StoredLine::Text {
            text: s.to_string(),
        }
    }

    #[test]
    fn line_cap_evicts_oldest() {
        let mut b = BoundedBuffer::new(RingCaps {
            max_lines: 3,
            max_bytes: usize::MAX,
        });
        for i in 0..10 {
            b.push(text(&format!("line-{i}")));
        }
        assert_eq!(b.line_count(), 3);
        // Oldest 7 were evicted.
        assert_eq!(b.drain_evicted().len(), 7);
        // Draining is idempotent.
        assert!(b.drain_evicted().is_empty());
    }

    #[test]
    fn repeated_lines_collapse_without_growing() {
        let mut b = BoundedBuffer::new(RingCaps {
            max_lines: 100,
            max_bytes: usize::MAX,
        });
        for _ in 0..1000 {
            b.push(text("progress..."));
        }
        // 1000 identical lines collapse to a single entry.
        assert_eq!(b.line_count(), 1);
        assert_eq!(b.rendered_rows(), 1000);
        assert_eq!(b.total_ingested(), 1000);
        match b.iter().next().unwrap() {
            StoredLine::Repeated { count, .. } => assert_eq!(*count, 1000),
            other => panic!("expected Repeated, got {other:?}"),
        };
    }

    #[test]
    fn byte_cap_keeps_memory_flat_under_ten_million_lines() {
        // The core SUM-50 guarantee: resident bytes stay bounded regardless of volume.
        let caps = RingCaps {
            max_lines: 4096,
            max_bytes: 1024 * 1024,
        };
        let mut b = BoundedBuffer::new(caps);
        for i in 0..10_000_000u64 {
            // Vary content so collapse doesn't hide the eviction path.
            b.push(text(&format!("log entry number {i} with some payload")));
            if i % 50_000 == 0 {
                // Simulate the chunk store draining evicted lines.
                b.drain_evicted();
            }
        }
        b.drain_evicted();
        assert!(b.line_count() <= caps.max_lines);
        assert!(
            b.resident_bytes() <= caps.max_bytes + 512,
            "resident bytes {} exceeded cap",
            b.resident_bytes()
        );
        assert_eq!(b.total_ingested(), 10_000_000);
    }

    #[test]
    fn distinct_then_repeat_collapses_only_the_run() {
        let mut b = BoundedBuffer::new(RingCaps {
            max_lines: 100,
            max_bytes: usize::MAX,
        });
        b.push(text("a"));
        b.push(text("b"));
        b.push(text("b"));
        b.push(text("b"));
        b.push(text("c"));
        // a, b×3, c
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.rendered_rows(), 5);
    }
}
