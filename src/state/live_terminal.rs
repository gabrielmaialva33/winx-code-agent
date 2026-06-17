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

/// Floor for the viewport dimensions. vt100 0.16.2 underflows (`grid.rs`:
/// `prev_pos.row -= scrolled` and `size.cols - width`) on a 1-row or 1-col grid
/// when output triggers a wrap+scroll — a debug panic, silent `u16` wrap in
/// release. A real PTY is never that small, but `resize` can be asked for it, so
/// we clamp to the smallest size vt100 handles safely.
const MIN_VIEWPORT: u16 = 2;

/// What changed on the live screen since the client last looked. Lets
/// `status_check` ship only the delta when piloting a TUI over many polls —
/// far fewer tokens than re-sending the whole frame each time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenUpdate {
    /// No baseline yet (first look) or too much changed — the full screen.
    Full(Vec<String>),
    /// Only these `(1-based row, new content)` lines changed since the last look.
    Diff(Vec<(usize, String)>),
    /// Nothing changed since the last look.
    Unchanged,
}

/// A continuously-fed terminal emulator with a fixed viewport. Wraps a
/// `vt100::Parser` and exposes just what the PTY live tap needs.
pub struct LiveTerminal {
    parser: Parser,
    /// Snapshot the client saw on its last look — the baseline the next
    /// [`LiveTerminal::snapshot_diff`] diffs against.
    last_snapshot: Option<Vec<String>>,
}

impl LiveTerminal {
    /// Create a live terminal with a fixed `rows`x`cols` viewport. Dimensions
    /// are floored at [`MIN_VIEWPORT`] (a 0/1-sized grid underflows vt100).
    /// Scrollback is left at 0: the snapshot only needs the visible viewport,
    /// and the PTY keeps its own raw `line_ring` for deeper scrollback.
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: Parser::new(rows.max(MIN_VIEWPORT), cols.max(MIN_VIEWPORT), 0),
            last_snapshot: None,
        }
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

    /// Resize the viewport (rows, cols), floored at [`MIN_VIEWPORT`].
    ///
    /// We deliberately do **not** call vt100's `set_size`: its column-shrink
    /// reflow underflows / `unwrap`s on perfectly ordinary content (fuzz-found),
    /// and since the release build is `panic = "abort"` such a panic would take
    /// the whole MCP server down. Instead we rebuild the parser at the new size
    /// and replay the visible text — plain rendered lines can't retrigger the
    /// reflow bug, and TUIs redraw on the SIGWINCH that accompanies a resize.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(MIN_VIEWPORT);
        let cols = cols.max(MIN_VIEWPORT);
        let preserved = self.snapshot(0);
        self.parser = Parser::new(rows, cols, 0);
        if !preserved.is_empty() {
            self.parser.process(preserved.join("\r\n").as_bytes());
        }
        // The baseline no longer matches the new geometry; force a full next diff.
        self.last_snapshot = None;
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

    /// Cursor position as `(row, col)`, 0-based, from the live screen — tells the
    /// agent where focus sits in a menu/form it's piloting.
    pub fn cursor_position(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    /// Diff the current screen against the snapshot the client last saw, update
    /// the baseline, and return only what changed. Positional (the terminal is a
    /// grid that redraws in place), 1-based rows to match the rest of winx.
    /// Returns the full screen on the first look or when more than `threshold`
    /// lines changed (a diff that big isn't worth the framing).
    pub fn snapshot_diff(&mut self, max_lines: usize, threshold: usize) -> ScreenUpdate {
        let current = self.snapshot(max_lines);
        let update = match self.last_snapshot.as_ref() {
            Some(previous) => {
                let changed = diff_lines(previous, &current);
                if changed.is_empty() {
                    ScreenUpdate::Unchanged
                } else if changed.len() <= threshold {
                    ScreenUpdate::Diff(changed)
                } else {
                    ScreenUpdate::Full(current.clone())
                }
            }
            None => ScreenUpdate::Full(current.clone()),
        };
        self.last_snapshot = Some(current);
        update
    }
}

/// Positional line diff: each `(1-based row, new content)` where the two frames
/// differ. A blanked/removed line shows up as that row with empty content.
fn diff_lines(previous: &[String], current: &[String]) -> Vec<(usize, String)> {
    let rows = previous.len().max(current.len());
    (0..rows)
        .filter_map(|i| {
            let was = previous.get(i).map_or("", String::as_str);
            let now = current.get(i).map_or("", String::as_str);
            (was != now).then(|| (i + 1, now.to_string()))
        })
        .collect()
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

    #[test]
    fn cursor_position_tracks_feed() {
        let mut t = LiveTerminal::new(10, 40);
        t.feed(b"abc");
        assert_eq!(t.cursor_position(), (0, 3), "cursor should sit after 'abc' on row 0");
    }

    #[test]
    fn snapshot_diff_full_then_unchanged_then_one_line() {
        let mut t = LiveTerminal::new(5, 40);
        t.feed(b"alpha\r\nbeta\r\ngamma\r\n");
        // First look: no baseline -> full frame.
        assert!(matches!(t.snapshot_diff(0, 10), ScreenUpdate::Full(_)));
        // Nothing fed since -> Unchanged.
        assert_eq!(t.snapshot_diff(0, 10), ScreenUpdate::Unchanged);
        // Overwrite row 2 (1-based) in place -> a single-line diff.
        t.feed(b"\x1b[2;1Hbeta2");
        let update = t.snapshot_diff(0, 10);
        assert!(
            matches!(&update, ScreenUpdate::Diff(changed) if changed.len() == 1),
            "expected a one-line diff, got {update:?}"
        );
        if let ScreenUpdate::Diff(changed) = &update {
            assert_eq!(changed.first().map(|c| c.0), Some(2), "1-based row 2");
            assert!(changed.first().is_some_and(|c| c.1.contains("beta2")), "got {changed:?}");
        }
    }

    #[test]
    fn snapshot_diff_big_change_falls_back_to_full() {
        let mut t = LiveTerminal::new(10, 40);
        t.feed(b"seed\r\n");
        let _ = t.snapshot_diff(0, 1); // establish baseline
        for i in 0..8 {
            t.feed(format!("line{i}\r\n").as_bytes());
        }
        // More than `threshold` lines changed -> full frame, not a giant diff.
        assert!(matches!(t.snapshot_diff(0, 1), ScreenUpdate::Full(_)));
    }

    use proptest::prelude::*;

    proptest! {
        // The live tap is fed untrusted program output. Feeding arbitrary byte
        // chunks (split anywhere, so escape sequences straddle boundaries) must
        // never panic and must keep snapshot/cursor inside the fixed viewport.
        #[test]
        fn feed_arbitrary_bytes_never_panics_and_stays_bounded(
            rows in 1u16..40,
            cols in 1u16..120,
            chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..48), 0..24),
        ) {
            // Effective viewport after the MIN_VIEWPORT floor (see `new`).
            let eff_rows = rows.max(MIN_VIEWPORT);
            let eff_cols = cols.max(MIN_VIEWPORT);
            let mut t = LiveTerminal::new(rows, cols);
            for chunk in &chunks {
                t.feed(chunk);
            }
            // Snapshot never exceeds the viewport height.
            let full = t.snapshot(0);
            prop_assert!(full.len() <= eff_rows as usize, "snapshot {} rows > viewport {eff_rows}", full.len());
            // Cursor stays inside the grid (col may rest at `cols` on a pending wrap).
            let (cr, cc) = t.cursor_position();
            prop_assert!(cr < eff_rows, "cursor row {cr} >= rows {eff_rows}");
            prop_assert!(cc <= eff_cols, "cursor col {cc} > cols {eff_cols}");
            // `max_lines` is honored.
            prop_assert!(t.snapshot(3).len() <= 3);
            // Diffing never panics and respects the threshold cap.
            let _ = t.snapshot_diff(0, 4);
            if let ScreenUpdate::Diff(changed) = t.snapshot_diff(0, 4) {
                prop_assert!(changed.len() <= 4);
            }
        }

        // Resizing to arbitrary dimensions (including row/col swaps) after a
        // feed never panics and re-bounds the snapshot to the new viewport.
        #[test]
        fn resize_after_feed_never_panics(
            r1 in 1u16..30, c1 in 1u16..80,
            r2 in 1u16..30, c2 in 1u16..80,
            data in prop::collection::vec(any::<u8>(), 0..256),
        ) {
            let eff_r2 = r2.max(MIN_VIEWPORT) as usize;
            let mut t = LiveTerminal::new(r1, c1);
            t.feed(&data);
            t.resize(r2, c2);
            prop_assert!(t.snapshot(0).len() <= eff_r2);
            // Feeding again after the resize is still bounded by the new height.
            t.feed(&data);
            prop_assert!(t.snapshot(0).len() <= eff_r2);
        }
    }
}
