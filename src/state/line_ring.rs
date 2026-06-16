//! Bounded, line-oriented scrollback buffer fed raw PTY chunks.
//!
//! The PTY reader appends decoded chunks; this keeps the most recent
//! `capacity` complete lines plus the in-flight (unterminated) partial line.
//! Crucially it *remembers* what it had to throw away — older lines evicted
//! once the ring filled, and bytes clipped off a pathologically long
//! newline-less line — so [`LineRing::scrollback_notice`] can tell the caller
//! their scrollback is incomplete instead of silently handing back a truncated
//! view. Extracted from `PtyShell` so the ring logic is unit-testable without
//! spawning a real shell.

use std::collections::VecDeque;

/// A ring of complete lines plus a pending partial line, with drop accounting.
#[derive(Debug)]
pub struct LineRing {
    /// Fully-emitted lines, newest at the back. Capped at `capacity`.
    lines: VecDeque<String>,
    /// Carries the unterminated tail across chunks so a partial line isn't
    /// double-counted when the rest of it arrives.
    partial: String,
    /// Max complete lines retained before the oldest is evicted.
    capacity: usize,
    /// Max bytes retained in `partial` before its head is clipped.
    max_partial_bytes: usize,
    /// How many complete lines have been evicted over this ring's lifetime.
    dropped_lines: u64,
    /// Whether the partial line ever had its head clipped (a long, unterminated
    /// stream: a `\r`-only progress bar, a binary blob, `yes | tr -d '\n'`).
    partial_clipped: bool,
}

impl LineRing {
    pub fn new(capacity: usize, max_partial_bytes: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(capacity),
            partial: String::new(),
            capacity,
            max_partial_bytes,
            dropped_lines: 0,
            partial_clipped: false,
        }
    }

    /// Append a freshly-arrived chunk, splitting on newlines into complete
    /// lines and retaining the unterminated tail as `partial`.
    pub fn push_chunk(&mut self, chunk: &str) {
        let combined = if self.partial.is_empty() {
            chunk.to_string()
        } else {
            let mut s = std::mem::take(&mut self.partial);
            s.push_str(chunk);
            s
        };

        let mut last_nl_end: Option<usize> = None;
        for (idx, ch) in combined.char_indices() {
            if ch == '\n' {
                let end = idx + ch.len_utf8();
                let start = last_nl_end.unwrap_or(0);
                // Keep the raw line (CR/cursor moves intact); the emulator
                // replays them on collect. Only drop a trailing CR (CRLF).
                let line = combined[start..idx].trim_end_matches('\r').to_string();
                if self.lines.len() == self.capacity {
                    self.lines.pop_front();
                    self.dropped_lines += 1;
                }
                self.lines.push_back(line);
                last_nl_end = Some(end);
            }
        }

        if let Some(end) = last_nl_end {
            self.partial = combined[end..].to_string();
        } else {
            self.partial = combined;
        }

        // A stream that never emits a newline would grow `partial` without
        // bound. Keep only the tail; older bytes of an unterminated line aren't
        // useful as scrollback. Snap to a char boundary so we never split a
        // code point.
        if self.partial.len() > self.max_partial_bytes {
            let cut = self.partial.len() - self.max_partial_bytes;
            let cut = crate::utils::floor_char_boundary(&self.partial, cut);
            self.partial.drain(..cut);
            self.partial_clipped = true;
        }
    }

    /// The last `lines` complete lines plus the partial, oldest first, joined
    /// with `\n`. Raw — no terminal rendering, no truncation notice.
    pub fn raw(&self, lines: usize) -> String {
        if lines == 0 {
            return String::new();
        }
        let start = self.lines.len().saturating_sub(lines);
        let mut out = String::new();
        for line in self.lines.iter().skip(start) {
            out.push_str(line);
            out.push('\n');
        }
        if !self.partial.is_empty() {
            out.push_str(&self.partial);
        }
        out
    }

    /// A one-line notice when the scrollback handed back for a request of
    /// `lines` lines is incomplete — older lines were evicted and/or the
    /// partial line was clipped. `None` when nothing relevant was lost.
    pub fn scrollback_notice(&self, lines: usize) -> Option<String> {
        let mut notes: Vec<String> = Vec::new();
        // Only flag dropped lines if the request actually reaches the oldest
        // line we still hold — a request for the last N (< ring size) loses
        // nothing the caller asked for.
        if self.dropped_lines > 0 && lines >= self.lines.len() {
            notes.push(format!(
                "{} earlier line(s) dropped — scrollback keeps only the most recent {}",
                self.dropped_lines, self.capacity
            ));
        }
        if self.partial_clipped && lines > 0 {
            notes.push(format!(
                "a long unterminated line was clipped to its last {} bytes",
                self.max_partial_bytes
            ));
        }
        if notes.is_empty() {
            None
        } else {
            Some(format!("[winx: {}]", notes.join("; ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_lines(ring: &mut LineRing, n: usize) {
        for i in 0..n {
            ring.push_chunk(&format!("line {i}\n"));
        }
    }

    #[test]
    fn push_chunk_splits_lines_and_keeps_partial() {
        let mut ring = LineRing::new(10, 1024);
        ring.push_chunk("a\nb\nc"); // "c" is the unterminated partial
        assert_eq!(ring.raw(10), "a\nb\nc");
        ring.push_chunk("cc\n"); // partial completes -> "ccc"
        assert_eq!(ring.raw(10), "a\nb\nccc\n");
    }

    #[test]
    fn no_notice_when_within_capacity() {
        let mut ring = LineRing::new(2000, 1024);
        feed_lines(&mut ring, 50);
        assert_eq!(ring.scrollback_notice(2000), None);
    }

    #[test]
    fn notice_when_older_lines_were_dropped() {
        // Ring holds 100; we feed 250 and ask for everything. 150 were evicted,
        // so the caller must be told their scrollback is incomplete.
        let mut ring = LineRing::new(100, 1024);
        feed_lines(&mut ring, 250);
        let notice = ring.scrollback_notice(100).unwrap_or_default();
        assert!(notice.contains("150"), "notice should report 150 dropped lines: {notice}");
        assert!(notice.to_lowercase().contains("scrollback"), "notice text: {notice}");
    }

    #[test]
    fn notice_reports_clipped_partial_line() {
        let mut ring = LineRing::new(100, 64);
        // A newline-less stream larger than max_partial_bytes gets head-clipped.
        ring.push_chunk(&"x".repeat(200));
        let notice = ring.scrollback_notice(100).unwrap_or_default();
        assert!(notice.to_lowercase().contains("clip"), "notice text: {notice}");
    }
}
