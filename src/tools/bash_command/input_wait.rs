//! Input-wait detection for budgeted long waits.
//!
//! A client-profile budget can stretch a requested one-second wait to most of
//! a minute. That must not turn a command blocked on a prompt into a silent
//! stall, so a budgeted wait also returns once the PTY has gone quiet with the
//! screen showing a pending line prompt or an idle full-screen program. The
//! check is a heuristic: a false positive only costs the extra poll an
//! unbudgeted wait would have cost anyway.

use std::time::{Duration, Instant};

use crate::state::pty::PtyShell;

/// PTY silence required before the screen is inspected for a prompt.
pub(super) const INPUT_QUIET: Duration = Duration::from_millis(750);

/// Case-insensitive fragments that only appear in interactive prompts.
const PROMPT_FRAGMENTS: [&str; 8] = [
    "password",
    "passphrase",
    "[y/n]",
    "(y/n)",
    "yes/no",
    "press enter",
    "press return",
    "press any key",
];

/// Tracks PTY quiescence across one wait and inspects the screen at most once
/// per quiet period.
pub(super) struct InputWaitWatch {
    last_activity: u64,
    quiet_since: Instant,
    inspected_activity: Option<u64>,
}

impl InputWaitWatch {
    pub(super) fn new(shell: &PtyShell, now: Instant) -> Self {
        Self { last_activity: shell.activity_seq(), quiet_since: now, inspected_activity: None }
    }

    /// True once the program has been silent for [`INPUT_QUIET`] and its screen
    /// looks like it is waiting for the operator.
    pub(super) fn awaiting_input(&mut self, shell: &PtyShell, now: Instant) -> bool {
        let activity = shell.activity_seq();
        if activity != self.last_activity {
            self.last_activity = activity;
            self.quiet_since = now;
            return false;
        }
        if now.duration_since(self.quiet_since) < INPUT_QUIET
            || self.inspected_activity == Some(activity)
        {
            return false;
        }
        self.inspected_activity = Some(activity);
        shell.live_in_alt_screen() || line_awaits_input(&shell.live_cursor_line_prefix())
    }
}

/// Whether the text left of the cursor reads like a line prompt. Ordinary
/// output ends in a newline, so the cursor sitting after text is itself the
/// main signal; the suffix check filters out stalled progress bars.
pub(super) fn line_awaits_input(prefix: &str) -> bool {
    let text = prefix.trim_end();
    if text.is_empty() {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    if PROMPT_FRAGMENTS.iter().any(|fragment| lower.contains(fragment)) {
        return true;
    }
    let mut tail = text.chars().rev();
    match tail.next() {
        Some(':' | '?' | '#' | '$') => true,
        // `>>>`, `> ` and `db> ` prompts, but not `==>` / `-->` progress arrows.
        Some('>') => !matches!(tail.next(), Some('=' | '-')),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::line_awaits_input;

    #[test]
    fn line_prompts_are_recognized() {
        for prompt in [
            "Password: ",
            "[sudo] password for user:",
            "Enter passphrase for key '/home/u/.ssh/id_ed25519': ",
            "Continue? [Y/n] ",
            "Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
            "Proceed (y/n)",
            "Press ENTER to continue",
            ">>> ",
            "> ",
            "postgres=# ",
            "user@host:~$ ",
            "Name: ",
        ] {
            assert!(line_awaits_input(prompt), "{prompt:?} should read as a prompt");
        }
    }

    #[test]
    fn progress_and_ordinary_output_are_not_prompts() {
        for line in [
            "",
            "   ",
            "Building [=======================> ] 450/451: winx-code-agent(bin)",
            "Downloading 45%",
            "[=====>",
            "progress -->",
            "   Compiling serde v1.0.0",
        ] {
            assert!(!line_awaits_input(line), "{line:?} should not read as a prompt");
        }
    }
}
