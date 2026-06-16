use regex::Regex;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
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

/// Map from a 64-bit text hash to (rendered output, insertion time).
///
/// Keyed by a hash of the source text rather than the text itself: a terminal
/// dump can be hundreds of KB, and storing it as the map key (plus cloning it
/// on every eviction) was the dominant cost. A hash collision only causes a
/// stale render to be recomputed, never corruption.
type CacheEntryMap = HashMap<u64, (Vec<String>, Instant)>;

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
    fn get(&self, text: &str) -> Option<Vec<String>> {
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
    fn insert(&self, text: &str, value: Vec<String>) {
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

// Initialize the global terminal cache
lazy_static::lazy_static! {
    static ref TERMINAL_CACHE: TerminalCache = TerminalCache::new(100, CACHE_TTL);
}

/// Render terminal output with line wrapping
pub fn render_terminal_output(text: &str) -> Vec<String> {
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
    if rand::random::<u32>() % 100 == 0 {
        TERMINAL_CACHE.cleanup();
    }

    result
}

/// Render finished command output to plain-text lines via vt100, applying cursor
/// movements so readline echo / in-place redraws collapse exactly as a real
/// terminal would. One-shot — a fresh parser per call. This is NOT the live PTY
/// emulator (that is [`crate::state::live_terminal::LiveTerminal`]).
fn render_via_vt100(text: &str) -> Vec<String> {
    // vt100 treats a bare LF as line-feed-only (the cursor keeps its column, so
    // output stair-steps). The old hand-rolled engine treated LF as CR+LF, and
    // the wcgw incremental path (`bash_command.rs`) re-feeds already-rendered
    // text joined by '\n'. Normalize so multi-line output doesn't stair-step and
    // a re-render stays idempotent.
    let bytes = normalize_lf_to_crlf(text);

    // Size the viewport to the input so short output (the overwhelming majority
    // of calls) doesn't pay for a giant grid allocation. Cap at
    // DEFAULT_MAX_SCREEN_LINES: longer output keeps its LAST lines and drops the
    // oldest — the same cap the old Screen enforced, now via vt100 scroll with
    // scrollback disabled.
    let line_estimate = text.lines().count().saturating_add(2);
    let rows = u16::try_from(line_estimate.clamp(1, DEFAULT_MAX_SCREEN_LINES)).unwrap_or(u16::MAX);
    let cols = u16::try_from(DEFAULT_COLUMNS).unwrap_or(u16::MAX);

    let mut parser = vt100::Parser::new(rows, cols, 0);
    parser.process(&bytes);

    // vt100's `contents()` already yields plain text with escapes resolved, so no
    // post-strip is needed. Trim trailing whitespace per line and drop trailing
    // blank lines, matching the old `Screen::display()` shape.
    let mut lines: Vec<String> =
        parser.screen().contents().lines().map(|line| line.trim_end().to_string()).collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
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
    static RE: std::sync::OnceLock<Option<Regex>> = std::sync::OnceLock::new();

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
    let re = RE.get_or_init(|| {
        Regex::new(
            r"\x1b\[[0-9;:?<>=!]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[=>MNcD78]",
        )
        .ok()
    });
    let cleaned = match re {
        Some(re) => re.replace_all(input, "").into_owned(),
        None => input.to_string(),
    };
    // Defensive: drop any stray ESC the pattern didn't consume (partial seqs).
    cleaned.replace('\u{1b}', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_cache_round_trips() {
        let cache = TerminalCache::new(10, 60);
        cache.insert("test", vec!["line1".to_string(), "line2".to_string()]);
        assert_eq!(cache.get("test"), Some(vec!["line1".to_string(), "line2".to_string()]));
        assert_eq!(cache.get("unknown"), None);
    }

    #[test]
    fn normalize_lf_inserts_cr_only_when_missing() {
        assert_eq!(normalize_lf_to_crlf("a\nb"), b"a\r\nb");
        assert_eq!(normalize_lf_to_crlf("a\r\nb"), b"a\r\nb");
        assert_eq!(normalize_lf_to_crlf("plain"), b"plain");
    }
}
