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
        "antigravity" | "agy" | "gemini" => Box::new(AntigravityRecognizer),
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

/// Recognizer for the `codex` CLI (Ink TUI). Markers verified against a real
/// codex-cli 0.139.0 (gpt-5.5) session captured through winx:
///   * **Busy**: the footer shows `• Working (Ns • esc to interrupt)` while
///     generating — the same `esc to interrupt` anchor claude uses.
///   * **`AwaitingApproval`**: the command-confirmation dialog ("Would you like
///     to run the following command?", "Press enter to confirm or esc to
///     cancel") shown when codex must escalate (e.g. `-a untrusted`).
///   * **`AwaitingInput`**: the input box, whose prompt glyph is `›` (U+203A) —
///     distinct from claude's `❯` and from a bare `>`.
pub struct CodexRecognizer;

impl CodexRecognizer {
    fn is_approval(screen: &[String]) -> bool {
        // Phrase-based and localized to the bottom of the screen. The `›`
        // selection glyph also appears in this dialog, so it MUST be classified
        // as approval (checked before the input box), not as plain input.
        let joined = tail_lower(screen, 30);
        joined.contains("would you like to run")
            || joined.contains("press enter to confirm")
            || joined.contains("yes, proceed")
    }

    fn has_input_box(screen: &[String]) -> bool {
        // Codex's Ink prompt char is `›` (U+203A). Only consulted once Busy and
        // approval are ruled out, so a lingering `›` can't mask an active turn.
        screen.iter().any(|l| l.trim_start().starts_with('›'))
    }
}

impl TurnRecognizer for CodexRecognizer {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn detect(&self, screen: &[String]) -> TurnState {
        // Order matters: Busy and approval before the input box so a stale `›`
        // never shadows an active or blocked turn.
        //
        // `preparing to execute` covers the transient where codex is running a
        // tool call: it briefly drops the `esc to interrupt` footer while the
        // submitted prompt's `›` still lingers in the history — without this it
        // would read as a false AwaitingInput mid-execution.
        if any_line_contains(screen, "esc to interrupt")
            || any_line_contains(screen, "preparing to execute")
        {
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

// --- Antigravity CLI --------------------------------------------------------

/// Recognizer for the `agy` (Antigravity CLI, Gemini) TUI. **Best-effort.**
/// Unlike claude/codex (Ink, alt-screen + in-place redraw), agy renders in
/// append mode inside winx's PTY: spent `Generating…` / `esc to cancel` frames
/// ghost into later screens and are never cleared. We therefore lead with the
/// idle hint `? for shortcuts`, which only sits at the *bottom* of the screen
/// when input is actually ready — a busy turn pushes it out of the tail — and
/// only then fall back to the busy markers. Markers verified against a real
/// agy 1.0.8 (Gemini 3.5 Flash) session captured through winx.
///
/// Deliberately NOT part of [`AutoRecognizer`]: its markers (`esc to cancel`,
/// `generating`) are generic enough to pollute claude/codex detection, so it is
/// opt-in only via the `agy`/`antigravity` hint — mirroring [`GenericRecognizer`].
pub struct AntigravityRecognizer;

impl TurnRecognizer for AntigravityRecognizer {
    fn name(&self) -> &'static str {
        "antigravity"
    }

    fn detect(&self, screen: &[String]) -> TurnState {
        // Idle hint first: `? for shortcuts` only sits at the bottom when the
        // input box is ready. A busy turn pushes it out of the tail, and under
        // append-mode rendering spent busy markers ghost into the idle screen —
        // so leading with the idle hint (scoped to the tail) is what survives
        // the ghosting. Only then fall back to the busy markers.
        if tail_lower(screen, 4).contains("? for shortcuts") {
            return TurnState::AwaitingInput;
        }
        if any_line_contains(screen, "generating") || any_line_contains(screen, "esc to cancel") {
            return TurnState::Busy;
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
    fn auto_surfaces_codex_input_when_claude_is_blind() {
        // A real codex idle screen: claude's recognizer doesn't know the `›`
        // glyph and returns Unknown, but auto must still surface codex's
        // AwaitingInput rather than collapsing to Unknown.
        let screen = lines(&["• PONG", "› Run /review on my current changes"]);
        assert_eq!(ClaudeRecognizer.detect(&screen), TurnState::Unknown);
        assert_eq!(CodexRecognizer.detect(&screen), TurnState::AwaitingInput);
        assert_eq!(AutoRecognizer.detect(&screen), TurnState::AwaitingInput);
    }

    #[test]
    fn auto_stays_busy_with_stale_prompt_glyphs() {
        // Both a stale claude `❯` and a codex `›` can linger while a turn
        // streams; the `esc to interrupt` footer must keep auto Busy, never
        // letting a leftover prompt glyph flip it to ready.
        let screen = lines(&["❯ old", "› old", "• Working (2s • esc to interrupt)"]);
        assert_eq!(AutoRecognizer.detect(&screen), TurnState::Busy);
    }

    // Codex fixtures below are from a real `codex` (codex-cli 0.139.0, gpt-5.5)
    // session captured through winx's PTY.

    #[test]
    fn codex_busy_via_working_footer() {
        let screen = lines(&[
            "› escreva um poema longo sobre o oceano",
            "",
            "• Working (1s • esc to interrupt)",
            "",
            "  gpt-5.5 high · ~/Documents/projects/winx-code-agent",
        ]);
        assert_eq!(CodexRecognizer.detect(&screen), TurnState::Busy);
        assert_eq!(AutoRecognizer.detect(&screen), TurnState::Busy);
    }

    #[test]
    fn codex_idle_input_box_uses_angle_prompt() {
        // Regression: codex's Ink prompt glyph is `›` (U+203A), not claude's `❯`
        // nor a bare `>`. The old recognizer missed it and fell back to Unknown,
        // forcing pure quiescence. Now it reports AwaitingInput explicitly.
        let screen = lines(&[
            "• PONG",
            "",
            "› Run /review on my current changes",
            "",
            "  gpt-5.5 high · ~/Documents/projects/winx-code-agent",
        ]);
        assert_eq!(CodexRecognizer.detect(&screen), TurnState::AwaitingInput);
    }

    #[test]
    fn codex_awaiting_approval_command_dialog() {
        // The real approval dialog codex shows under `-a untrusted`. Note the
        // `›` selection glyph also appears here, so approval MUST be checked
        // before the input box or it would read as AwaitingInput.
        let screen = lines(&[
            "  Would you like to run the following command?",
            "",
            "  $ touch /tmp/x.txt",
            "",
            "› 1. Yes, proceed (y)",
            "  2. Yes, and don't ask again (p)",
            "  3. No, and tell Codex what to do differently (esc)",
            "",
            "  Press enter to confirm or esc to cancel",
        ]);
        assert_eq!(CodexRecognizer.detect(&screen), TurnState::AwaitingApproval);
        assert_eq!(AutoRecognizer.detect(&screen), TurnState::AwaitingApproval);
    }

    #[test]
    fn codex_preparing_to_execute_is_busy_not_input() {
        // Regression from a live session: while codex prepares/runs a tool call
        // it briefly shows no `esc to interrupt` footer, and the just-submitted
        // prompt's `›` lingers in the history above. That transient must read as
        // Busy, not a false AwaitingInput, or a pilot would inject mid-execution.
        let screen = lines(&[
            "› execute o comando de shell 'touch /tmp/x.txt'",
            "• Vou executar exatamente esse comando",
            "• Preparing to execute shell command  9",
        ]);
        assert_eq!(CodexRecognizer.detect(&screen), TurnState::Busy);
    }

    #[test]
    fn codex_busy_wins_over_stale_angle_prompt() {
        // A busy codex still shows the `›` input box; "esc to interrupt" must
        // keep it Busy so we never send input mid-stream.
        let screen = lines(&["› an earlier prompt", "• Working (3s • esc to interrupt)"]);
        assert_eq!(CodexRecognizer.detect(&screen), TurnState::Busy);
    }

    // Antigravity (`agy` 1.0.8, Gemini 3.5 Flash) fixtures captured through
    // winx. agy renders in append mode, so these intentionally include the ghost
    // `Generating…` / `esc to cancel` frames that linger after the turn — the
    // recognizer must see through them.

    #[test]
    fn antigravity_busy_via_generating_and_esc_to_cancel() {
        let screen = lines(&[
            "> responda com uma palavra: PONG    Gemini 3.5 Flash (Medium)",
            "⣾  Generating...",
            "  PONG",
            "esc to cancel    Gemini 3.5 Flash (Medium)",
        ]);
        assert_eq!(AntigravityRecognizer.detect(&screen), TurnState::Busy);
    }

    #[test]
    fn antigravity_idle_via_shortcuts_hint_despite_ghost_frames() {
        // The real idle screen still carries ghost `Generating…`/`esc to cancel`
        // lines from the finished turn, but `? for shortcuts` sits at the bottom
        // only when input is ready — so it must win over the ghosts.
        let screen = lines(&[
            "  PONG",
            "⣯  Generating..",
            "esc to cancel    Gemini 3.5 Flash (Medium)",
            "? for shortcuts",
        ]);
        assert_eq!(AntigravityRecognizer.detect(&screen), TurnState::AwaitingInput);
    }

    #[test]
    fn antigravity_is_opt_in_not_part_of_auto() {
        // agy's best-effort markers would pollute claude/codex, so it is opt-in
        // only and auto must stay blind to its idle hint.
        let screen = lines(&["? for shortcuts"]);
        assert_eq!(AntigravityRecognizer.detect(&screen), TurnState::AwaitingInput);
        assert_eq!(AutoRecognizer.detect(&screen), TurnState::Unknown);
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
