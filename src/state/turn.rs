//! Turn detection for interactive TUIs piloted through the PTY.
//!
//! "Has the program finished this turn and is it ready for the next input?" is
//! the core question when driving a full-screen TUI (the `claude` CLI, `codex`,
//! a REPL, ...) like a human operator. We answer it with a hybrid signal:
//!
//!   * a generic **quiescence** check (the rendered screen stopped changing),
//!     handled by the caller, plus
//!   * pluggable per-app **recognizers** here that read the rendered screen and
//!     classify it as [`TurnState::Busy`], [`TurnState::AwaitingInput`] or
//!     [`TurnState::AwaitingApproval`].
//!
//! Recognizers operate on already-rendered, ANSI-stripped screen lines, so they
//! never deal with escape codes — only with the visible text a human sees.

/// What an interactive program is doing right now, inferred from the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    /// Actively producing output (spinner / "esc to interrupt" / streaming).
    Busy,
    /// Idle at an input prompt, ready for the next message.
    AwaitingInput,
    /// Blocked on a confirmation/permission dialog needing an operator choice.
    AwaitingApproval,
    /// The recognizer can't tell from the screen alone (defer to quiescence).
    Unknown,
}

impl TurnState {
    /// A short, stable label for status footers.
    pub fn as_str(self) -> &'static str {
        match self {
            TurnState::Busy => "busy",
            TurnState::AwaitingInput => "awaiting_input",
            TurnState::AwaitingApproval => "awaiting_approval",
            TurnState::Unknown => "unknown",
        }
    }

    /// Whether this state means the turn is over and the operator may act
    /// (send the next prompt, or answer a prompt).
    pub fn is_ready(self) -> bool {
        matches!(self, TurnState::AwaitingInput | TurnState::AwaitingApproval)
    }
}

/// Reads a rendered screen (ANSI already stripped, one `String` per line) and
/// classifies the current turn state for a specific kind of TUI.
pub trait TurnRecognizer: Send + Sync {
    /// Stable identifier, surfaced in the status footer.
    fn name(&self) -> &'static str;
    /// Classify the current screen.
    fn detect(&self, screen: &[String]) -> TurnState;
}

/// Build the recognizer for a hint string (`auto` | `claude` | `codex` |
/// `generic`). Unknown hints fall back to `auto`.
pub fn recognizer_for(hint: &str) -> Box<dyn TurnRecognizer> {
    match hint.trim().to_ascii_lowercase().as_str() {
        "claude" => Box::new(ClaudeRecognizer),
        "codex" => Box::new(CodexRecognizer),
        "generic" | "none" | "off" => Box::new(GenericRecognizer),
        _ => Box::new(AutoRecognizer),
    }
}

// --- shared helpers ---------------------------------------------------------

/// Whether any line on the screen contains `needle` (case-insensitive).
///
/// Busy markers are scanned across the WHOLE screen, not just the tail: a long
/// response can push the footer's "esc to interrupt" out of the last lines, and
/// missing it would flip a still-generating turn to "ready" mid-stream and
/// corrupt the next input. A rare false-busy (the model literally printing the
/// phrase) only delays the turn — strictly the safer error.
fn any_line_contains(screen: &[String], needle: &str) -> bool {
    screen.iter().any(|l| l.to_lowercase().contains(needle))
}

/// Last `n` non-blank lines, lowercased and joined — used for the dialog
/// phrase scan ("do you want", "trust this folder", ...) which is naturally
/// localized to the bottom of the screen.
fn tail_lower(screen: &[String], n: usize) -> String {
    let mut lines: Vec<String> = screen
        .iter()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(n)
        .map(|l| l.to_lowercase())
        .collect();
    lines.reverse();
    lines.join("\n")
}

// --- Claude TUI -------------------------------------------------------------

/// Recognizer for the `claude` CLI (Ink/React TUI). Markers verified against a
/// real claude v2.1.x session captured through winx:
///   * **Busy**: the footer shows `esc to interrupt` while generating. We rely
///     on this and NOT on spinner glyphs — claude renders inline, so spent
///     spinner lines ("✽ Gusting… (3s)") linger in the scrollback after the
///     turn ends and would otherwise read as a false "busy".
///   * **`AwaitingApproval`**: a confirmation/permission dialog, recognized by its
///     prompt phrasing ("do you want", "trust this folder", "Esc to cancel", …).
///   * **`AwaitingInput`**: the `❯` input box is present and neither of the above.
pub struct ClaudeRecognizer;

impl ClaudeRecognizer {
    fn is_approval(screen: &[String]) -> bool {
        // Phrase-based: these are the actual prompts claude shows for the
        // trust-folder check and tool-permission dialogs. We deliberately do
        // NOT key off a bare "1./2." numbered menu — ordinary numbered prose in
        // a normal answer ("1. Agile…", "2. Waterfall…") would false-positive.
        let joined = tail_lower(screen, 30);
        joined.contains("do you want")
            || joined.contains("would you like")
            || joined.contains("trust this folder")
            || joined.contains("enter to confirm")
            || joined.contains("esc to cancel")
    }

    fn has_input_box(screen: &[String]) -> bool {
        // The Ink prompt char. Only consulted once we've ruled out Busy/approval,
        // so a lingering `❯` from earlier prompts can't mask an active turn.
        screen.iter().any(|l| l.contains('❯'))
    }
}

impl TurnRecognizer for ClaudeRecognizer {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn detect(&self, screen: &[String]) -> TurnState {
        // Order matters: Busy and approval are checked before the input box so a
        // stale `❯` never shadows an active or blocked turn.
        if any_line_contains(screen, "esc to interrupt") {
            return TurnState::Busy;
        }
        if Self::is_approval(screen) {
            return TurnState::AwaitingApproval;
        }
        if Self::has_input_box(screen) {
            return TurnState::AwaitingInput;
        }
        TurnState::Unknown
    }
}

// --- Codex CLI --------------------------------------------------------------

/// Recognizer for the `codex` CLI. Best-effort: keys off the same canonical
/// footer/keyword signals; the exact UI is less stable than Claude's so it
/// leans on quiescence when unsure. TODO: calibrate against captured sessions.
pub struct CodexRecognizer;

impl TurnRecognizer for CodexRecognizer {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn detect(&self, screen: &[String]) -> TurnState {
        if any_line_contains(screen, "esc to interrupt")
            || any_line_contains(screen, "press esc to")
            || any_line_contains(screen, "working…")
            || any_line_contains(screen, "thinking…")
        {
            return TurnState::Busy;
        }
        if screen.iter().any(|l| l.contains('❯') || l.trim_start().starts_with('>')) {
            return TurnState::AwaitingInput;
        }
        TurnState::Unknown
    }
}

// --- Generic ----------------------------------------------------------------

/// No app knowledge: always [`TurnState::Unknown`] so the caller decides purely
/// by quiescence. Correct (if conservative) for any TUI.
pub struct GenericRecognizer;

impl TurnRecognizer for GenericRecognizer {
    fn name(&self) -> &'static str {
        "generic"
    }

    fn detect(&self, _screen: &[String]) -> TurnState {
        TurnState::Unknown
    }
}

// --- Auto -------------------------------------------------------------------

/// Runs every app-specific recognizer and combines their verdicts by priority:
/// `Busy` > `AwaitingApproval` > `AwaitingInput` > `Unknown`. A recognizer that
/// sees the app working therefore always wins over one that only sees a
/// (possibly stale) prompt glyph — so `auto` never reports a busy app as ready,
/// while still falling back to `Unknown` (quiescence) for an unknown TUI.
pub struct AutoRecognizer;

impl TurnRecognizer for AutoRecognizer {
    fn name(&self) -> &'static str {
        "auto"
    }

    fn detect(&self, screen: &[String]) -> TurnState {
        let states = [ClaudeRecognizer.detect(screen), CodexRecognizer.detect(screen)];
        for want in [TurnState::Busy, TurnState::AwaitingApproval, TurnState::AwaitingInput] {
            if states.contains(&want) {
                return want;
            }
        }
        TurnState::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_string()).collect()
    }

    // Fixtures below are taken from a real `claude` v2.1.159 session captured
    // through winx's PTY.

    #[test]
    fn claude_busy_via_esc_to_interrupt() {
        let screen = lines(&[
            "❯ responda exatamente uma palavra: PONG",
            "",
            "* Hatching…",
            "⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt",
        ]);
        assert_eq!(ClaudeRecognizer.detect(&screen), TurnState::Busy);
        assert_eq!(AutoRecognizer.detect(&screen), TurnState::Busy);
    }

    #[test]
    fn claude_long_response_busy_even_with_stale_prompt() {
        // Regression for the review finding: on a long response the footer can
        // be far from the bottom and an old `❯` lingers in the scrollback.
        // Scanning the whole screen for "esc to interrupt" keeps it Busy, so we
        // never send input mid-stream.
        let mut screen = vec![
            "❯ old prompt from a previous turn".to_string(),
            String::new(),
            "●─here is a long answer".to_string(),
        ];
        screen.push("⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt".to_string());
        for i in 0..40 {
            screen.push(format!("streamed answer line {i}"));
        }
        assert_eq!(ClaudeRecognizer.detect(&screen), TurnState::Busy);
        assert_eq!(AutoRecognizer.detect(&screen), TurnState::Busy);
    }

    #[test]
    fn claude_idle_after_answer_is_input_not_busy() {
        // claude renders inline, so spent spinner lines linger in the scrollback
        // after the turn finishes. Once the footer drops "esc to interrupt" and
        // the `❯` box is back, the turn IS over. (Real post-answer screen.)
        let screen = lines(&[
            "❯ Responda com exatamente uma palavra, em maiúsculas: PONG",
            "· Gusting…",
            "●─PONG",
            "✽ Gusting… (3s · ↓ 1 tokens)",
            "✻ Baked for 4s",
            "❯",
            "⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents",
        ]);
        assert_eq!(ClaudeRecognizer.detect(&screen), TurnState::AwaitingInput);
    }

    #[test]
    fn claude_numbered_prose_is_not_approval() {
        // Regression: a normal answer containing a numbered list whose text
        // happens to contain "allows"/"known" must NOT read as an approval.
        let screen = lines(&[
            "❯ compare the methodologies",
            "1. Agile - allows iterative delivery",
            "2. Waterfall - assumes requirements are known up front",
            "❯",
            "⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents",
        ]);
        assert_eq!(ClaudeRecognizer.detect(&screen), TurnState::AwaitingInput);
    }

    #[test]
    fn claude_awaiting_approval_trust_prompt() {
        // The real "trust this folder?" approval shown on a fresh directory.
        let screen = lines(&[
            "Quick safety check: Is this a project you created or one you trust?",
            "❯ 1. Yes, I trust this folder",
            "  2. No, exit",
            "Enter to confirm · Esc to cancel",
        ]);
        assert_eq!(ClaudeRecognizer.detect(&screen), TurnState::AwaitingApproval);
    }

    #[test]
    fn claude_awaiting_approval_tool_menu() {
        let screen = lines(&[
            "⏺ Bash(rm -rf /tmp/x)",
            "Do you want to proceed?",
            "❯ 1. Yes",
            "  2. No, and tell Claude what to do differently",
        ]);
        assert_eq!(ClaudeRecognizer.detect(&screen), TurnState::AwaitingApproval);
    }

    #[test]
    fn auto_prefers_busy_codex_over_stale_claude_prompt() {
        // Regression: a genuinely busy Codex screen that also has a stale `❯`
        // must NOT be reported as ready just because ClaudeRecognizer sees the
        // prompt glyph. Busy wins across recognizers.
        let screen = lines(&["❯ old prompt", "", "Reading files...", "working…"]);
        assert_eq!(ClaudeRecognizer.detect(&screen), TurnState::AwaitingInput);
        assert_eq!(CodexRecognizer.detect(&screen), TurnState::Busy);
        assert_eq!(AutoRecognizer.detect(&screen), TurnState::Busy);
    }

    #[test]
    fn generic_is_always_unknown() {
        let screen = lines(&["$ ls", "file.txt", "$"]);
        assert_eq!(GenericRecognizer.detect(&screen), TurnState::Unknown);
    }

    #[test]
    fn auto_falls_back_to_unknown_for_plain_shell() {
        let screen = lines(&["building...", "done", "user@host:~$"]);
        assert_eq!(AutoRecognizer.detect(&screen), TurnState::Unknown);
    }

    #[test]
    fn turn_state_ready_semantics() {
        assert!(TurnState::AwaitingInput.is_ready());
        assert!(TurnState::AwaitingApproval.is_ready());
        assert!(!TurnState::Busy.is_ready());
        assert!(!TurnState::Unknown.is_ready());
    }
}
