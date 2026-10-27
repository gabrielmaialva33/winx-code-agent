use regex::Regex;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Instant;

/// Maximum number of lines to keep in the screen buffer
pub const MAX_SCREEN_LINES: usize = 10000;
/// Default maximum number of lines to keep in the screen buffer
pub const DEFAULT_MAX_SCREEN_LINES: usize = 500;
/// Maximum number of columns for the screen. Must match the PTY width
/// (`pty::DEFAULT_COLS`) so emulator-rendered scrollback wraps exactly where the
/// real terminal did, instead of silently re-wrapping long lines at a narrower
/// width.
const DEFAULT_COLUMNS: usize = 200;
/// Maximum output size in bytes to prevent excessive memory usage
pub const MAX_OUTPUT_SIZE: usize = 500_000;
/// Maximum cache entry lifetime in seconds
const CACHE_TTL: u64 = 300; // 5 minutes

/// Plain terminal text and whether nonblank rows were discarded during replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalRender {
    pub(crate) lines: Vec<String>,
    pub(crate) output_truncated: bool,
}

/// Map from a 64-bit text hash to (rendered output, insertion time).
///
/// Keyed by a hash of the source text rather than the text itself: a terminal
/// dump can be hundreds of KB, and storing it as the map key (plus cloning it
/// on every eviction) was the dominant cost. A hash collision only causes a
/// stale render to be recomputed, never corruption.
type CacheEntryMap = HashMap<u64, (TerminalRender, Instant)>;

/// Hash a chunk of terminal text into the cache key space.
fn hash_terminal_text(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Inner cache state behind a single lock so the map and the eviction queue can
/// never drift out of sync.
#[derive(Debug, Default)]
struct CacheInner {
    map: CacheEntryMap,
    /// Keys in insertion order, for O(1) FIFO eviction (no O(n log n) sort).
    order: VecDeque<u64>,
}

/// Caching system for terminal output rendering
#[derive(Debug, Clone)]
struct TerminalCache {
    inner: Arc<RwLock<CacheInner>>,
    /// Maximum number of entries in the cache
    max_entries: usize,
    /// Time-to-live for cache entries in seconds
    ttl: u64,
}

impl TerminalCache {
    /// Create a new terminal cache
    fn new(max_entries: usize, ttl: u64) -> Self {
        Self { inner: Arc::new(RwLock::new(CacheInner::default())), max_entries, ttl }
    }

    /// Get a cached value if available and not expired
    fn get(&self, text: &str) -> Option<TerminalRender> {
        let key = hash_terminal_text(text);
        let inner = self.inner.read().ok()?;
        let (value, timestamp) = inner.map.get(&key)?;
        if timestamp.elapsed().as_secs() < self.ttl {
            Some(value.clone())
        } else {
            None
        }
    }

    /// Insert a value into the cache, evicting the oldest entries if over cap.
    fn insert(&self, text: &str, value: TerminalRender) {
        let key = hash_terminal_text(text);
        let Ok(mut inner) = self.inner.write() else {
            return;
        };
        if inner.map.insert(key, (value, Instant::now())).is_none() {
            inner.order.push_back(key);
        }
        while inner.order.len() > self.max_entries {
            let Some(old) = inner.order.pop_front() else {
                break;
            };
            inner.map.remove(&old);
        }
    }

    /// Clear expired entries from the cache
    fn cleanup(&self) {
        let Ok(mut inner) = self.inner.write() else {
            return;
        };
        let ttl = self.ttl;
        let CacheInner { map, order } = &mut *inner;
        map.retain(|_, (_, timestamp)| timestamp.elapsed().as_secs() < ttl);
        order.retain(|key| map.contains_key(key));
    }
}

// Initialize the global terminal cache.
static TERMINAL_CACHE: LazyLock<TerminalCache> =
    LazyLock::new(|| TerminalCache::new(100, CACHE_TTL));

/// Render terminal output with line wrapping
pub fn render_terminal_output(text: &str) -> Vec<String> {
    render_terminal_output_detailed(text).lines
}

pub(crate) fn render_terminal_output_detailed(text: &str) -> TerminalRender {
    // Check cache first.
    if let Some(cached) = TERMINAL_CACHE.get(text) {
        return cached;
    }

    let result = render_via_vt100(text);

    // Cache the result for future use (only if reasonably sized).
    if text.len() < MAX_OUTPUT_SIZE {
        TERMINAL_CACHE.insert(text, result.clone());
    }

    // Periodically clean up expired cache entries.
    if rand::random::<u32>().is_multiple_of(100) {
        TERMINAL_CACHE.cleanup();
    }

    result
}

#[derive(Default)]
struct RenderCallbacks {
    dropped_rows: usize,
    dropped_text: bool,
}

impl vt100::Callbacks for RenderCallbacks {
    fn on_scroll(&mut self, contents: vt100::capture::RowContents<'_>, _alternate_screen: bool) {
        self.dropped_rows = self.dropped_rows.saturating_add(1);
        if !self.dropped_text {
            // Cursor movement can scroll an empty row without losing any text.
            // Inspect only until the first nonblank row is lost in this pass.
            let mut row = String::new();
            let _ = contents.write_formatted_basic(
                &mut row,
                &mut vt100::capture::BasicFormattedCaptureState::default(),
            );
            self.dropped_text =
                vt100::capture::basic_formatted_to_plain(&row).any(|part| !part.trim().is_empty());
        }
    }
}

/// Render finished command output to plain-text lines via vt100, applying cursor
/// movements so readline echo / in-place redraws collapse exactly as a real
/// terminal would. One-shot — a fresh parser per call. This is NOT the live PTY
/// emulator (that is [`crate::state::live_terminal::LiveTerminal`]).
fn render_via_vt100(text: &str) -> TerminalRender {
    // vt100 treats a bare LF as line-feed-only (the cursor keeps its column, so
    // output stair-steps). The old hand-rolled engine treated LF as CR+LF, and
    // the wcgw incremental path (`bash_command.rs`) re-feeds already-rendered
    // text joined by '\n'. Normalize so multi-line output doesn't stair-step and
    // a re-render stays idempotent.
    let bytes = normalize_lf_to_crlf(text);

    // Start small for ordinary output. Newline count alone underestimates wrapped
    // lines (JSON, compiler diagnostics, Unicode), so let vt100 detect overflow.
    // Replaying from scratch preserves cursor and clear-screen semantics; merely
    // resizing after a lossy pass cannot recover those operations.
    let mut row_count = text.lines().count().saturating_add(2).clamp(1, DEFAULT_MAX_SCREEN_LINES);
    let cols = u16::try_from(DEFAULT_COLUMNS).unwrap_or(u16::MAX);
    let cols = std::num::NonZeroU16::new(cols).unwrap_or(std::num::NonZeroU16::MIN);

    loop {
        let rows = u16::try_from(row_count).unwrap_or(u16::MAX);
        let rows = std::num::NonZeroU16::new(rows).unwrap_or(std::num::NonZeroU16::MIN);
        let mut parser =
            vt100::Parser::new_with_callbacks(rows, cols, 0, RenderCallbacks::default());
        parser.process(&bytes);
        let dropped_rows = parser.callbacks().dropped_rows;
        if dropped_rows > 0 && row_count < DEFAULT_MAX_SCREEN_LINES {
            row_count = row_count
                .saturating_mul(2)
                .max(row_count.saturating_add(dropped_rows))
                .min(DEFAULT_MAX_SCREEN_LINES);
            continue;
        }

        // Only the final pass determines loss. Keep the flag even if a later
        // clear-screen or alternate-screen exit hides the surviving text.
        let output_truncated = parser.callbacks().dropped_text;
        let mut lines: Vec<String> =
            parser.screen().contents().lines().map(|line| line.trim_end().to_string()).collect();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        return TerminalRender { lines, output_truncated };
    }
}

/// Insert a CR before any LF that lacks one. PTY data already arrives as CRLF;
/// this only fixes rendered text re-fed through [`render_terminal_output`].
fn normalize_lf_to_crlf(text: &str) -> Vec<u8> {
    let src = text.as_bytes();
    let mut out = Vec::with_capacity(src.len() + 16);
    let mut prev = 0u8;
    for &byte in src {
        if byte == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        out.push(byte);
        prev = byte;
    }
    out
}

/// Strip ANSI escape codes from a string using a robust regex
pub fn strip_ansi_codes(input: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

    // Fast path: no ESC byte means nothing to strip.
    if !input.contains('\u{1b}') {
        return input.to_string();
    }

    // Cover the full escape grammar, not just SGR colors. Interactive programs
    // (python/node REPLs, psql, readline) emit far more than colors:
    //   - CSI: ESC [ <params> <intermediates> <final> (cursor moves, `?2004h`
    //          bracketed-paste, `2K` erase, `0m` reset, ...)
    //   - OSC: ESC ] ... (BEL | ST)                    (window-title sets)
    //   - 2-byte ESC sequences (ESC =, ESC >, ESC M, ...)
    // Without OSC/CSI-cursor coverage the scrollback leaked raw bracketed-paste
    // and `]0;user@host` noise into the model's view. Cached so we don't
    // recompile the pattern on every rendered line.
    // `.expect`, not `.ok()`: a compile-time-literal regex that fails is a dev
    // bug. The old `OnceLock<Option<Regex>>` + `.ok()` froze `None` on first use
    // and then silently stopped stripping CSI/OSC params for the rest of the
    // process — leaking raw escape sequences into the model's view. This is the
    // production strip used by bash_command/pty; fail loud instead.
    #[allow(clippy::expect_used)]
    let re = RE.get_or_init(|| {
        Regex::new(
            r"\x1b\[[0-9;:?<>=!]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[=>MNcD78]",
        )
        .expect("ANSI strip regex must compile")
    });
    let cleaned = re.replace_all(input, "").into_owned();
    // Defensive: drop any stray ESC the pattern didn't consume (partial seqs).
    cleaned.replace('\u{1b}', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_cache_round_trips() {
        let cache = TerminalCache::new(10, 60);
        let rendered = TerminalRender {
            lines: vec!["line1".to_string(), "line2".to_string()],
            output_truncated: true,
        };
        cache.insert("test", rendered.clone());
        assert_eq!(cache.get("test"), Some(rendered));
        assert_eq!(cache.get("unknown"), None);
    }

    #[test]
    fn visual_row_limit_reports_only_actual_overflow() {
        let exact = "A".repeat(DEFAULT_MAX_SCREEN_LINES * DEFAULT_COLUMNS);
        let rendered = render_terminal_output_detailed(&exact);
        assert_eq!(rendered.lines.join("\n"), exact);
        assert!(!rendered.output_truncated);

        for suffix in ["B", "\n"] {
            let rendered = render_terminal_output_detailed(&format!("{exact}{suffix}"));
            assert!(rendered.output_truncated, "suffix {suffix:?} must scroll one row");
            assert!(rendered.lines.join("\n").len() <= DEFAULT_MAX_SCREEN_LINES * DEFAULT_COLUMNS);
        }
    }

    #[test]
    fn scrolling_blank_rows_does_not_report_lost_text() {
        let rendered = render_terminal_output_detailed("\x1b[999B\nFINAL");
        assert_eq!(rendered.lines.last().map(String::as_str), Some("FINAL"));
        assert!(!rendered.output_truncated);
    }

    #[test]
    fn discarded_text_is_reported_even_after_screen_replacement() {
        let overflowing = "row\n".repeat(DEFAULT_MAX_SCREEN_LINES + 1);
        for (input, expected) in [
            (format!("{overflowing}\x1b[2J\x1b[HFINAL"), "FINAL"),
            (format!("PRIMARY\x1b[?1049h{overflowing}\x1b[?1049l"), "PRIMARY"),
        ] {
            let rendered = render_terminal_output_detailed(&input);
            assert_eq!(rendered.lines, vec![expected]);
            assert!(rendered.output_truncated);
        }
    }

    #[test]
    fn normalize_lf_inserts_cr_only_when_missing() {
        assert_eq!(normalize_lf_to_crlf("a\nb"), b"a\r\nb");
        assert_eq!(normalize_lf_to_crlf("a\r\nb"), b"a\r\nb");
        assert_eq!(normalize_lf_to_crlf("plain"), b"plain");
    }

    use proptest::prelude::*;

    proptest! {
        // Any input — including malformed/partial escape sequences and raw ESC
        // bytes — must be stripped to leave no ESC behind, and never panic.
        #[test]
        fn strip_ansi_codes_leaves_no_esc(text in prop::collection::vec(any::<char>(), 0..256)) {
            let s: String = text.into_iter().collect();
            let out = strip_ansi_codes(&s);
            prop_assert!(!out.contains('\u{1b}'), "ESC survived stripping: {out:?}");
        }

        // ESC-free input is returned verbatim (the fast path must not mangle).
        #[test]
        fn strip_ansi_codes_is_identity_without_esc(
            text in prop::collection::vec(
                any::<char>().prop_filter("no esc", |c| *c != '\u{1b}'),
                0..256,
            ),
        ) {
            let s: String = text.into_iter().collect();
            prop_assert_eq!(strip_ansi_codes(&s), s);
        }
    }
}
