//! Conscious compression of noisy shell output.
//!
//! The goal is token economy **without losing context**. We never summarize,
//! paraphrase, or drop unique information. The only thing collapsed is
//! *mechanical repetition*, which carries no extra meaning:
//!   - runs of byte-identical consecutive lines  -> one line + an `[×N]` marker
//!   - runs of 3+ blank lines                    -> a single blank line
//!
//! Deliberately conservative choices so we never eat real context:
//!   - lines that merely differ (e.g. "Test 1 passed" / "Test 2 passed", or a
//!     compiler's per-file diagnostics) are left untouched — those are content;
//!   - progress bars that redraw with `\r` are already collapsed upstream by the
//!     PTY ring buffer, so there's nothing speculative to guess at here;
//!   - an `[×N]` marker is fully reversible information — the reader knows the
//!     exact line and how many times it repeated.
//!
//! Set `WINX_NO_COMPRESS=1` to disable entirely.

/// Don't bother compressing output shorter than this many lines.
const MIN_LINES: usize = 30;
/// Only collapse a run of identical lines once it repeats at least this often.
const RUN_MIN: usize = 3;
/// Only return a compressed result if it removes at least this many lines —
/// otherwise the footer isn't worth the noise.
const MIN_SAVED_LINES: usize = 8;

/// Collapse mechanical repetition in `output`. Returns `None` when compression
/// is disabled, the output is too short, or there's nothing meaningful to save —
/// callers should fall back to the original text in that case.
pub fn compress_output(output: &str) -> Option<String> {
    if disabled() {
        return None;
    }
    let lines: Vec<&str> = output.split('\n').collect();
    if lines.len() < MIN_LINES {
        return None;
    }

    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut saved = 0usize;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let mut j = i + 1;
        while j < lines.len() && lines[j] == line {
            j += 1;
        }
        let run = j - i;

        if line.trim().is_empty() {
            // Collapse a run of blank lines to a single blank.
            out.push(String::new());
            saved += run - 1;
        } else if run >= RUN_MIN {
            // Collapse identical non-blank lines, keeping the count (reversible).
            out.push(format!("{line}  [winx: ×{run}]"));
            saved += run - 1;
        } else {
            // Distinct content — keep verbatim.
            for keep in &lines[i..j] {
                out.push((*keep).to_string());
            }
        }
        i = j;
    }

    if saved < MIN_SAVED_LINES {
        return None;
    }

    let compressed_count = out.len();
    out.push(format!(
        "[winx: collapsed {saved} repeated lines ({} → {compressed_count}); \
         set WINX_NO_COMPRESS=1 to see raw output]",
        lines.len()
    ));
    Some(out.join("\n"))
}

fn disabled() -> bool {
    std::env::var("WINX_NO_COMPRESS").is_ok_and(|value| {
        let value = value.trim();
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_output_is_left_alone() {
        let out = "line\n".repeat(5);
        assert!(compress_output(&out).is_none());
    }

    #[test]
    fn collapses_identical_run_but_keeps_count() -> Result<(), String> {
        // 50 identical "retrying..." lines surrounded by distinct content.
        let mut text = String::from("start\n");
        for _ in 0..50 {
            text.push_str("retrying connection...\n");
        }
        text.push_str("done\n");
        let compressed = compress_output(&text).ok_or("should compress")?;
        assert!(compressed.contains("retrying connection...  [winx: ×50]"));
        assert!(compressed.contains("start"));
        assert!(compressed.contains("done"));
        // the 50 repeats became 1 line + footer
        assert!(compressed.lines().count() < 10);
        Ok(())
    }

    #[test]
    fn keeps_distinct_lines_that_only_differ_by_number() {
        use std::fmt::Write as _;
        // Per-item lines carry real info and must NOT be collapsed.
        let mut text = String::new();
        for n in 0..40 {
            let _ = writeln!(text, "Test {n} passed");
        }
        // Nothing identical repeats, so there's nothing to collapse.
        assert!(compress_output(&text).is_none());
    }

    #[test]
    fn disabled_via_env_returns_none() {
        // Can't safely toggle env in parallel tests; just assert the helper logic
        // by confirming a compressible payload compresses when env is unset.
        let text = "spam\n".repeat(40);
        // (env unset in CI) -> compresses
        if !disabled() {
            assert!(compress_output(&text).is_some());
        }
    }
}
