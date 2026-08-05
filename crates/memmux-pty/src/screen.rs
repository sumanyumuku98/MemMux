//! vt100 screen state with periodic checkpoints (SUM-51 / §8.2).
//!
//! Wraps a `vt100::Parser` so the daemon tracks the *current screen grid* (what the TUI renders)
//! independently of the scrollback history (which is bounded and paged elsewhere). A checkpoint
//! is a self-contained byte blob that reproduces the current screen, letting old raw output be
//! dropped without corrupting the live view.

/// Live terminal screen state driven by a vt100 parser.
pub struct Screen {
    parser: vt100::Parser,
}

impl std::fmt::Debug for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (rows, cols) = self.size();
        f.debug_struct("Screen")
            .field("rows", &rows)
            .field("cols", &cols)
            .finish()
    }
}

impl Screen {
    /// Create a screen of `rows`×`cols` with `scrollback` lines of parser-side scrollback.
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self {
            parser: vt100::Parser::new(rows.max(1), cols.max(1), scrollback),
        }
    }

    /// Feed raw terminal output bytes to the parser.
    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    /// Resize the screen grid.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows.max(1), cols.max(1));
    }

    /// Current `(rows, cols)`.
    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    /// Current cursor `(row, col)`.
    pub fn cursor(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    /// The current screen as plain text (one entry per row, trailing blanks trimmed).
    pub fn contents(&self) -> String {
        self.parser.screen().contents()
    }

    /// The current screen as text rows.
    pub fn rows(&self) -> Vec<String> {
        self.contents().lines().map(|s| s.to_string()).collect()
    }

    /// A self-contained checkpoint: escape sequences that reproduce the current screen when
    /// fed to a fresh parser. Old raw history can be discarded once a checkpoint is taken.
    pub fn checkpoint(&self) -> Vec<u8> {
        self.parser.screen().contents_formatted()
    }

    /// Restore a screen from a checkpoint produced by [`Screen::checkpoint`].
    pub fn from_checkpoint(rows: u16, cols: u16, scrollback: usize, checkpoint: &[u8]) -> Self {
        let mut screen = Self::new(rows, cols, scrollback);
        screen.process(checkpoint);
        screen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_appears_on_screen() {
        let mut s = Screen::new(4, 20, 0);
        s.process(b"hello world");
        assert!(s.contents().contains("hello world"));
        assert_eq!(s.size(), (4, 20));
    }

    #[test]
    fn cursor_tracks_writes() {
        let mut s = Screen::new(4, 20, 0);
        s.process(b"abc");
        let (row, col) = s.cursor();
        assert_eq!(row, 0);
        assert_eq!(col, 3);
    }

    #[test]
    fn resize_changes_grid() {
        let mut s = Screen::new(10, 40, 0);
        s.resize(24, 80);
        assert_eq!(s.size(), (24, 80));
    }

    #[test]
    fn checkpoint_round_trips_the_screen() {
        let mut s = Screen::new(4, 20, 0);
        s.process(b"line one\r\nline two");
        let checkpoint = s.checkpoint();
        assert!(!checkpoint.is_empty());

        let restored = Screen::from_checkpoint(4, 20, 0, &checkpoint);
        assert_eq!(restored.contents(), s.contents());
    }

    #[test]
    fn carriage_return_overwrites_line() {
        // A progress bar rewriting the same line must not accumulate rows on-screen.
        let mut s = Screen::new(2, 20, 0);
        s.process(b"50%\r100%");
        let contents = s.contents();
        assert!(contents.contains("100%"));
        assert!(
            !contents.contains("50%"),
            "carriage return should overwrite"
        );
    }
}
