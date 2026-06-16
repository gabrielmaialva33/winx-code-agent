//! Golden regression suite for the terminal rendering surface.
//!
//! Captured against the hand-rolled engine BEFORE the vt100 consolidation so the
//! migration can prove behavioral parity. Every case locks in an observable
//! property of `render_terminal_output` / `strip_ansi_codes` — the only two
//! functions `terminal.rs` exposes to the rest of the crate.

use winx_code_agent::state::terminal::{render_terminal_output, strip_ansi_codes};

fn render(input: &str) -> Vec<String> {
    render_terminal_output(input)
}

// 1 — plain text passes through untouched.
#[test]
fn simple_text() {
    assert_eq!(render("Hello World"), vec!["Hello World".to_string()]);
}

// 2 — SGR color sequences are stripped, text stays in order.
#[test]
fn sgr_colors_stripped() {
    assert_eq!(render("\x1b[31mRed\x1b[32mGreen\x1b[0mNormal"), vec!["RedGreenNormal".to_string()]);
}

// 3 — CSI cursor-left overwrites in place (readline echo collapse).
#[test]
fn cursor_left_overwrite() {
    assert_eq!(render("ZZZZZZZZZ\x1b[5DQQ"), vec!["ZZZZQQZZZ".to_string()]);
}

// 4 — same mechanism, full overwrite of the moved-back region.
#[test]
fn cursor_left_5q() {
    assert_eq!(render("ZZZZZZZZZZ\x1b[5DQQQQQ"), vec!["ZZZZZQQQQQ".to_string()]);
}

// 5 — carriage return returns to column 0 and overwrites.
#[test]
fn cr_overwrite() {
    assert_eq!(render("ABCDE\rXY"), vec!["XYCDE".to_string()]);
}

// 6 — CRLF breaks lines.
#[test]
fn crlf_newline() {
    assert_eq!(render("Line1\r\nLine2"), vec!["Line1".to_string(), "Line2".to_string()]);
}

// 7 — a bare LF must start a new line at column 0 (NOT a stair-step).
//     This is the case that catches vt100's line-feed-only behavior.
#[test]
fn lf_only_is_newline() {
    assert_eq!(
        render("alpha\nbeta\ngamma"),
        vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
    );
}

// 8 — CHA moves to an absolute column, padding with spaces.
#[test]
fn cha_absolute_column() {
    assert_eq!(render("AAA\x1b[10GBBB"), vec!["AAA      BBB".to_string()]);
}

// 9 — erase-to-EOL clears from the cursor to the line end.
#[test]
fn erase_to_eol() {
    assert_eq!(render("ABCDEFGH\x1b[5G\x1b[K"), vec!["ABCD".to_string()]);
}

// 10 — bracketed-paste mode markers are consumed; cursor move + overwrite apply.
//      The second ">>> " stays and "42" overwrites only its first two columns,
//      so the trailing ">" survives. (vt100 reproduces this exactly.)
#[test]
fn bracketed_paste_consumed() {
    assert_eq!(render(">>> \x1b[?2004h>>> \x1b[?2004l\x1b[4D42"), vec![">>> 42>".to_string()]);
}

// 11 — OSC title sequence is stripped.
#[test]
fn osc_title_stripped() {
    assert_eq!(render("\x1b]0;user@host\x07prompt$ "), vec!["prompt$".to_string()]);
}

// 12 — 256-color SGR is processed and stripped.
#[test]
fn sgr_256color() {
    assert_eq!(render("\x1b[38;5;208mOrange\x1b[0mNormal"), vec!["OrangeNormal".to_string()]);
}

// 13 — alternate screen isolates: primary content survives the round-trip.
#[test]
fn alt_screen_isolates() {
    assert_eq!(render("primary\x1b[?1049hALT\x1b[?1049l"), vec!["primary".to_string()]);
}

// 14 — synchronized-output markers are consumed, content stays.
#[test]
fn sync_output_consumed() {
    assert_eq!(render("\x1b[?2026hHELLO\x1b[?2026l"), vec!["HELLO".to_string()]);
}

// 15 — strip_ansi_codes removes CSI/OSC but does not reflow or trim.
#[test]
fn strip_csi_osc() {
    assert_eq!(strip_ansi_codes("\x1b[1;35mhi\x1b[0m"), "hi");
    assert_eq!(strip_ansi_codes("\x1b]0;u@h\x07prompt$ "), "prompt$ ");
    assert_eq!(strip_ansi_codes("no escapes here"), "no escapes here");
}

// 16 — re-rendering already-rendered text (the wcgw combined path) is idempotent.
#[test]
fn idempotent_rerender() {
    let r1 = render("Line1\r\nWorld\r\nRed Text");
    let combined = r1.join("\n");
    let r2 = render(&combined);
    assert_eq!(r1, r2);
}

// 17 — line cap (DEFAULT_MAX_SCREEN_LINES): long output keeps the LAST ~500
//      lines, dropping the oldest. 600 rows -> 499 kept (row101..row599).
//      This locks in the existing cap, which the vt100 viewport must reproduce.
#[test]
fn line_cap_keeps_last_lines() {
    use std::fmt::Write as _;
    let mut input = String::new();
    for i in 0..600 {
        let _ = writeln!(input, "row{i}\r");
    }
    let out = render(&input);
    assert_eq!(out.len(), 499);
    assert_eq!(out[0], "row101");
    assert_eq!(out[out.len() - 1], "row599");
    assert!(!out.iter().any(|l| l == "row0"), "oldest lines must be dropped");
}
