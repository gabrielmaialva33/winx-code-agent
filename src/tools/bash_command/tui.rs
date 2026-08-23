use std::fmt::Write as FmtWrite;
use std::time::{Duration, Instant};

use super::output::{render_status, status_state, truncate_to_token_budget, MAX_OUTPUT_TOKENS};
use super::{main_shell, runtime_rendered, SharedPtyShell, ShellDeliveryCursor};
use crate::errors::Result;
use crate::runtime::BashCommandRuntimeResult;
use crate::state::bash_state::BashState;
use crate::state::live_terminal::ScreenUpdate;
use crate::state::pty::PtyShell;

const SCREEN_DIFF_THRESHOLD: usize = 10;

#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)] // mirrors the MCP screen action without an allocation wrapper
pub(super) async fn execute_screen(
    bash_state: &BashState,
    background_shell: Option<SharedPtyShell>,
    _is_background: bool,
    background_id: Option<&str>,
    lines: Option<usize>,
    diff: bool,
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
    compact_output: bool,
) -> Result<BashCommandRuntimeResult> {
    let shell = background_shell.unwrap_or_else(|| main_shell(bash_state));
    let max_lines = lines.unwrap_or(0);

    if diff {
        let (update, running, in_alt, cursor, running_for, exit_code, cwd) = {
            let mut guard = shell.lock().await;
            match guard.as_mut() {
                Some(shell) => {
                    shell.poll_output_nonblocking();
                    let running = shell.command_running;
                    let update = if let Some(cursor) = delivery_cursor {
                        cursor.sync_generation(shell.command_generation());
                        cursor.screen_update(shell.live_snapshot(max_lines), SCREEN_DIFF_THRESHOLD)
                    } else {
                        shell.live_snapshot_diff(max_lines, SCREEN_DIFF_THRESHOLD)
                    };
                    (
                        update,
                        running,
                        shell.live_in_alt_screen(),
                        shell.live_cursor_position(),
                        running.then(|| shell.command_elapsed()).flatten(),
                        (!running).then_some(shell.last_exit_code).flatten(),
                        Some(shell.current_cwd().to_path_buf()),
                    )
                }
                None => (ScreenUpdate::Full(Vec::new()), false, false, (0, 0), None, None, None),
            }
        };
        let (cursor_row, cursor_column) = cursor;
        let alt = if in_alt { " [alt-screen]" } else { "" };
        let state = status_state(
            background_id,
            running,
            running_for,
            exit_code,
            cwd.as_deref().unwrap_or(&bash_state.cwd),
            None,
        );
        let status = render_status(bash_state, &state);
        let body = match update {
            ScreenUpdate::Unchanged => "(no change since last screen)".to_string(),
            ScreenUpdate::Diff(changed) => {
                let mut output = String::from("(changed lines only)\n");
                for (row, content) in changed {
                    let _ = writeln!(output, "{row:>4}: {content}");
                }
                output
            }
            ScreenUpdate::Full(snapshot) => render_snapshot(&snapshot),
        };
        return Ok(runtime_rendered(
            format!(
                "--- live screen{alt} [cursor row={cursor_row} col={cursor_column}] (diff) ---\n{body}"
            ),
            &status,
            state,
            compact_output,
            None,
            false,
        ));
    }

    let (snapshot, running, in_alt, cursor, running_for, exit_code, cwd) = {
        let mut guard = shell.lock().await;
        match guard.as_mut() {
            Some(shell) => {
                shell.poll_output_nonblocking();
                let running = shell.command_running;
                (
                    shell.live_snapshot(max_lines),
                    running,
                    shell.live_in_alt_screen(),
                    shell.live_cursor_position(),
                    running.then(|| shell.command_elapsed()).flatten(),
                    (!running).then_some(shell.last_exit_code).flatten(),
                    Some(shell.current_cwd().to_path_buf()),
                )
            }
            None => (Vec::new(), false, false, (0, 0), None, None, None),
        }
    };

    let body = render_snapshot(&snapshot);
    let alt = if in_alt { " [alt-screen]" } else { "" };
    let (cursor_row, cursor_column) = cursor;
    let state = status_state(
        background_id,
        running,
        running_for,
        exit_code,
        cwd.as_deref().unwrap_or(&bash_state.cwd),
        None,
    );
    let status = render_status(bash_state, &state);
    Ok(runtime_rendered(
        format!("--- live screen{alt} [cursor row={cursor_row} col={cursor_column}] ---\n{body}"),
        &status,
        state,
        compact_output,
        None,
        false,
    ))
}

fn render_snapshot(snapshot: &[String]) -> String {
    let joined = snapshot.join("\n");
    if joined.trim().is_empty() {
        "(screen is empty)".to_string()
    } else {
        truncate_to_token_budget(&joined, MAX_OUTPUT_TOKENS).into_owned()
    }
}

#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
fn wait_turn_outcome(
    state: crate::state::turn::TurnState,
    alive: bool,
    activity: bool,
    seen_busy: bool,
    stable_for: Duration,
    busy_for: Duration,
    settle: Duration,
    quiet: Duration,
    timed_out: bool,
    wait_through_busy: bool,
) -> Option<&'static str> {
    use crate::state::turn::TurnState;
    if !alive {
        return Some("exited");
    }
    let ready = match state {
        TurnState::Busy => false,
        TurnState::AwaitingInput | TurnState::AwaitingApproval => {
            activity && stable_for >= if seen_busy { settle } else { quiet }
        }
        TurnState::Unknown => activity && stable_for >= quiet,
    };
    if ready {
        return Some("ready");
    }
    if !wait_through_busy && state == TurnState::Busy && busy_for >= settle {
        return Some("busy");
    }
    if timed_out {
        return Some("timeout");
    }
    None
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
pub(super) async fn execute_wait_for_turn(
    bash_state: &BashState,
    background_shell: Option<SharedPtyShell>,
    _is_background: bool,
    background_id: Option<&str>,
    recognizer_hint: Option<&str>,
    quiet_ms: Option<u64>,
    timeout_seconds: Option<f32>,
    lines: Option<usize>,
    wait_through_busy: bool,
    compact_output: bool,
) -> Result<BashCommandRuntimeResult> {
    use crate::state::turn::{recognizer_for, TurnState};

    let shell = background_shell.unwrap_or_else(|| main_shell(bash_state));
    let recognizer = recognizer_for(recognizer_hint.unwrap_or("auto"));
    let quiet = Duration::from_millis(quiet_ms.unwrap_or(600).clamp(50, 10_000));
    let settle = quiet.min(Duration::from_millis(300));
    let hard_cap =
        Duration::from_secs_f64(f64::from(timeout_seconds.unwrap_or(30.0)).clamp(0.5, 600.0));
    let max_lines = lines.unwrap_or(0);
    let poll = Duration::from_millis(120);
    let warmup = Duration::from_millis(2500);

    let start = Instant::now();
    let mut last_hash: Option<u64> = None;
    let mut initial_hash: Option<u64> = None;
    let mut stable_since = Instant::now();
    let mut seen_busy = false;
    let mut busy_since: Option<Instant> = None;

    loop {
        let (snapshot, in_alt, alive, running, running_for, exit_code, cwd) = {
            let mut guard = shell.lock().await;
            match guard.as_mut() {
                Some(shell) => {
                    shell.poll_output_nonblocking();
                    let running = shell.command_running;
                    (
                        shell.live_snapshot(max_lines),
                        shell.live_in_alt_screen(),
                        shell.is_alive(),
                        running,
                        running.then(|| shell.command_elapsed()).flatten(),
                        (!running).then_some(shell.last_exit_code).flatten(),
                        Some(shell.current_cwd().to_path_buf()),
                    )
                }
                None => (Vec::new(), false, false, false, None, None, None),
            }
        };

        let hash = PtyShell::fingerprint(&snapshot.join("\n"));
        if initial_hash.is_none() {
            initial_hash = Some(hash);
        }
        if Some(hash) != last_hash {
            last_hash = Some(hash);
            stable_since = Instant::now();
        }

        let state = recognizer.detect(&snapshot);
        if state == TurnState::Busy {
            seen_busy = true;
            if busy_since.is_none() {
                busy_since = Some(Instant::now());
            }
        } else {
            busy_since = None;
        }
        let busy_for = busy_since.map_or(Duration::ZERO, |since| since.elapsed());
        let stable_for = stable_since.elapsed();
        let activity = seen_busy || Some(hash) != initial_hash || start.elapsed() >= warmup;
        let timed_out = start.elapsed() >= hard_cap;
        if let Some(reason) = wait_turn_outcome(
            state,
            alive,
            activity,
            seen_busy,
            stable_for,
            busy_for,
            settle,
            quiet,
            timed_out,
            wait_through_busy,
        ) {
            let body = render_snapshot(&snapshot);
            let alt = if in_alt { " [alt-screen]" } else { "" };
            let header = format!(
                "--- turn: {} ({}, recognizer={}, waited {:.1}s){} ---",
                state.as_str(),
                reason,
                recognizer.name(),
                start.elapsed().as_secs_f64(),
                alt
            );
            let result_state = status_state(
                background_id,
                running,
                running_for,
                exit_code,
                cwd.as_deref().unwrap_or(&bash_state.cwd),
                Some(state),
            );
            let status = render_status(bash_state, &result_state);
            return Ok(runtime_rendered(
                format!("{header}\n{body}"),
                &status,
                result_state,
                compact_output,
                None,
                false,
            ));
        }

        tokio::time::sleep(poll).await;
    }
}

#[cfg(test)]
mod tests {
    use super::wait_turn_outcome;
    use crate::state::turn::TurnState;
    use std::time::Duration;

    const SETTLE: Duration = Duration::from_millis(300);
    const QUIET: Duration = Duration::from_millis(600);

    fn call(
        state: TurnState,
        busy_for: Duration,
        stable_for: Duration,
        seen_busy: bool,
        timed_out: bool,
        wait_through_busy: bool,
    ) -> Option<&'static str> {
        wait_turn_outcome(
            state,
            true,
            true,
            seen_busy,
            stable_for,
            busy_for,
            SETTLE,
            QUIET,
            timed_out,
            wait_through_busy,
        )
    }

    #[test]
    fn confirmed_busy_returns_early_instead_of_blocking_to_timeout() {
        assert_eq!(call(TurnState::Busy, SETTLE, Duration::ZERO, true, false, false), Some("busy"));
    }

    #[test]
    fn busy_not_yet_confirmed_keeps_waiting() {
        assert_eq!(
            call(TurnState::Busy, Duration::from_millis(100), Duration::ZERO, true, false, false,),
            None
        );
    }

    #[test]
    fn wait_through_busy_blocks_through_busy_until_timeout() {
        assert_eq!(call(TurnState::Busy, SETTLE * 10, Duration::ZERO, true, false, true), None);
        assert_eq!(
            call(TurnState::Busy, SETTLE * 10, Duration::ZERO, true, true, true),
            Some("timeout")
        );
    }

    #[test]
    fn awaiting_input_after_busy_is_ready_on_short_settle() {
        assert_eq!(
            call(TurnState::AwaitingInput, Duration::ZERO, SETTLE, true, false, false),
            Some("ready")
        );
    }

    #[test]
    fn awaiting_input_without_prior_busy_needs_full_quiet() {
        assert_eq!(
            call(TurnState::AwaitingInput, Duration::ZERO, SETTLE, false, false, false),
            None
        );
        assert_eq!(
            call(TurnState::AwaitingInput, Duration::ZERO, QUIET, false, false, false),
            Some("ready")
        );
    }

    #[test]
    fn dead_shell_reports_exited_even_if_busy() {
        assert_eq!(
            wait_turn_outcome(
                TurnState::Busy,
                false,
                true,
                true,
                Duration::ZERO,
                SETTLE,
                SETTLE,
                QUIET,
                false,
                false,
            ),
            Some("exited")
        );
    }

    #[test]
    fn nothing_happening_keeps_waiting() {
        assert_eq!(
            call(TurnState::Unknown, Duration::ZERO, Duration::ZERO, false, false, false,),
            None
        );
    }
}
