//! Live terminal emulator for piloting interactive TUIs, backed by the `vt100`
//! crate.
//!
//! Unlike the hand-rolled [`crate::state::terminal::Screen`] (a buffer that
//! *grows* on every linefeed), this models a **fixed viewport of `rows` lines**
//! with proper scrolling — the model a real terminal uses. Inline TUIs that
//! redraw by moving the cursor up and rewriting with erase-line (the `agy` /
//! Antigravity CLI, which renders in the main screen rather than the alternate
//! one) consolidate cleanly here instead of stacking ghost frames. See the
//! `inline_redraw_*` test for the exact regression this fixes.
//!
//! Only the *live* PTY tap uses this. One-shot rendering of finished command
//! output (`render_terminal_output`) stays on the growing-buffer `Screen`,
//! which is the right model there: it must keep every line of output, not clip
//! to a viewport.

use vt100::Parser;

/// A continuously-fed terminal emulator with a fixed viewport. Wraps a
/// `vt100::Parser` and exposes just what the PTY live tap needs.
pub struct LiveTerminal {
    parser: Parser,
}

impl LiveTerminal {
    /// Create a live terminal with a fixed `rows`x`cols` viewport. Dimensions
    /// are floored at 1 (a 0-sized grid is meaningless and panics downstream).
    /// Scrollback is left at 0: the snapshot only needs the visible viewport,
    /// and the PTY keeps its own raw `line_ring` for deeper scrollback.
    pub fn new(rows: u16, cols: u16) -> Self {
        Self { parser: Parser::new(rows.max(1), cols.max(1), 0) }
    }

    /// Feed raw PTY bytes. The parser is persistent, so escape sequences split
    /// across chunk boundaries decode correctly.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    /// Whether the alternate screen buffer is currently active (vim, htop, and
    /// the Ink-based claude/codex TUIs use it).
    pub fn in_alt_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    /// Resize the viewport (rows, cols), floored at 1.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows.max(1), cols.max(1));
    }

    /// Snapshot the consolidated visible screen as plain-text lines (ANSI
    /// already resolved by the emulator), trailing blank lines trimmed.
    /// `max_lines` of 0 returns the whole viewport; otherwise the last N lines.
    pub fn snapshot(&self, max_lines: usize) -> Vec<String> {
        let contents = self.parser.screen().contents();
        let mut lines: Vec<String> = contents.lines().map(str::to_string).collect();
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        if max_lines > 0 && lines.len() > max_lines {
            lines = lines.split_off(lines.len() - max_lines);
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_redraw_with_cursor_up_consolidates_no_ghosting() {
        // The agy pattern: draw a frame, move the cursor back up, erase below
        // and redraw. A growing-buffer emulator stacks "Generating" and "DONE";
        // a fixed viewport consolidates to just the latest frame.
        let mut t = LiveTerminal::new(10, 40);
        t.feed(b"line one\r\nGenerating...\r\n"); // frame 1, cursor now on row 2
        t.feed(b"\x1b[2A"); // cursor up 2 -> back to "line one"
        t.feed(b"\x1b[Jline one\r\nDONE\r\n"); // erase-below + redraw frame 2
        let joined = t.snapshot(0).join("\n");
        assert!(joined.contains("DONE"), "missing latest frame: {joined:?}");
        assert!(!joined.contains("Generating"), "ghost frame leaked: {joined:?}");
    }

    #[test]
    fn viewport_is_fixed_old_lines_scroll_off() {
        let mut t = LiveTerminal::new(3, 40); // only 3 visible rows
        for i in 0..10 {
            t.feed(format!("row{i}\r\n").as_bytes());
        }
        let snap = t.snapshot(0);
        assert!(snap.len() <= 3, "viewport overflowed its row count: {snap:?}");
        assert!(snap.iter().any(|l| l.contains("row9")), "newest row missing: {snap:?}");
        assert!(
            !snap.iter().any(|l| l.contains("row0")),
            "oldest row should have scrolled off a fixed viewport: {snap:?}"
        );
    }

    #[test]
    fn detects_alternate_screen_toggle() {
        let mut t = LiveTerminal::new(10, 40);
        assert!(!t.in_alt_screen());
        t.feed(b"\x1b[?1049h");
        assert!(t.in_alt_screen());
        t.feed(b"\x1b[?1049l");
        assert!(!t.in_alt_screen());
    }

    #[test]
    fn snapshot_last_n_lines() {
        let mut t = LiveTerminal::new(10, 40);
        t.feed(b"a\r\nb\r\nc\r\nd\r\n");
        assert_eq!(t.snapshot(2), vec!["c".to_string(), "d".to_string()]);
    }
}
