use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use regex::Regex;
use tokio::time::sleep;
use tracing::{debug, warn};

use super::{
    main_shell, runtime_rendered, BashCommandState, BashProcessStatus, SharedPtyShell,
    ShellDeliveryCursor,
};
use crate::errors::{Result, WinxError};
use crate::runtime::{lock_session_store, BashCommandRuntimeResult, ShellActionOptions};
use crate::state::bash_state::BashState;
use crate::state::pty::PtyShell;
use crate::state::terminal::{render_terminal_output, strip_ansi_codes};
use crate::state::turn::TurnState;
use crate::tools::background_shell::ExitedShellInfo;
use crate::types::BashCommandAction;

const OUTPUT_WAIT_PATIENCE: i32 = 3;
const POLL_INTERVAL_MS: u64 = 20;
const POST_PROMPT_DRAIN_MS: u64 = 100;
const MAX_OUTPUT_LENGTH: usize = 100_000;
pub(super) const MAX_OUTPUT_TOKENS: usize = 25_000;

fn char_safe_tail(text: &str, max_len: usize) -> &str {
    if text.len() <= max_len {
        return text;
    }
    let mut start = text.len() - max_len;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

pub(super) fn truncate_to_token_budget(text: &str, max_tokens: usize) -> std::borrow::Cow<'_, str> {
    if crate::utils::encoder::definitely_fits_token_budget(text, max_tokens) {
        return std::borrow::Cow::Borrowed(text);
    }

    let Some(tokens) = crate::utils::encoder::encode_ids(text) else {
        return std::borrow::Cow::Owned(format!(
            "(...truncated)\n{}",
            char_safe_tail(text, MAX_OUTPUT_LENGTH)
        ));
    };
    if tokens.len() <= max_tokens {
        return std::borrow::Cow::Borrowed(text);
    }

    let keep = max_tokens.saturating_sub(1);
    let tail = &tokens[tokens.len() - keep..];
    let decoded = crate::utils::encoder::decode_ids(tail)
        .unwrap_or_else(|| char_safe_tail(text, MAX_OUTPUT_LENGTH).to_string());
    std::borrow::Cow::Owned(format!("(...truncated)\n{decoded}"))
}

pub(super) fn status_state(
    background_id: Option<&str>,
    is_running: bool,
    running_for: Option<Duration>,
    exit_code: Option<i32>,
    cwd: &Path,
    turn_state: Option<TurnState>,
) -> BashCommandState {
    BashCommandState {
        process_status: if is_running {
            BashProcessStatus::Running
        } else {
            BashProcessStatus::Exited
        },
        background_id: background_id.map(ToString::to_string),
        running_for_seconds: running_for.map(|duration| duration.as_secs()),
        exit_code: (!is_running).then_some(exit_code).flatten(),
        cwd: cwd.to_path_buf(),
        turn_state,
    }
}

/// Render compatibility text from an already-authoritative state. Background
/// command summaries are deliberately placed before the final status block, but
/// they never participate in machine-readable orchestration.
pub(super) fn render_status(bash_state: &BashState, state: &BashCommandState) -> String {
    let mut status = String::new();
    if state.background_id.is_none() {
        let mut manager = lock_session_store();
        status.push_str("\n\nThis is the main shell. ");
        status.push_str(manager.get_running_info(&bash_state.current_thread_id).trim_end());
    }
    status.push_str("\n\n---\n\n");
    if let Some(id) = state.background_id.as_deref() {
        let _ = writeln!(status, "bg_command_id = {id}");
    }
    if state.is_running() {
        status.push_str("status = still running\n");
        if let Some(seconds) = state.running_for_seconds {
            let _ = writeln!(status, "running for = {seconds} seconds");
        }
    } else {
        status.push_str("status = process exited\n");
        if let Some(code) = state.exit_code {
            let _ = writeln!(status, "exit code = {code}");
        }
    }
    let _ = writeln!(status, "cwd = {}", state.cwd.display());
    status.trim_end().to_string()
}

/// Add a Winx-generated note before the compatibility status text without ever
/// deriving state from that text. If the human summary changed concurrently, the
/// note is appended instead; the authoritative `state` remains untouched.
pub(super) fn insert_note_before_status(
    bash_state: &BashState,
    result: &mut BashCommandRuntimeResult,
    note: &str,
) {
    if let Some(output) = result.compact_output.as_mut() {
        output.push_str(note);
        return;
    }
    let status = render_status(bash_state, &result.result.state);
    if result.result.output.ends_with(&status) {
        result.result.output.truncate(result.result.output.len() - status.len());
        result.result.output.push_str(note);
        result.result.output.push_str(&status);
    } else {
        result.result.output.push_str(note);
    }
}

fn wcgw_incremental_text(text: &str, last_pending_output: &str) -> String {
    let truncated = truncate_to_token_budget(text, MAX_OUTPUT_TOKENS);
    let text = truncated.as_ref();

    if last_pending_output.is_empty() {
        let rendered = render_terminal_output(text);
        return rstrip_lines(&rendered).trim_start().to_string();
    }

    let last_rendered = render_terminal_output(last_pending_output);
    if last_rendered.is_empty() {
        return rstrip_lines(&render_terminal_output(text));
    }

    let text_after_last = if text.len() > last_pending_output.len() {
        let cut = crate::utils::floor_char_boundary(text, last_pending_output.len());
        &text[cut..]
    } else {
        text
    };
    let combined = format!("{}\n{}", last_rendered.join("\n"), text_after_last);
    let new_rendered = render_terminal_output(&combined);
    rstrip_lines(&get_incremental_output(&last_rendered, &new_rendered))
}

fn extract_prompt_cwd(output: &str) -> Option<PathBuf> {
    static PROMPT_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    #[allow(clippy::expect_used)]
    let prompt_regex = PROMPT_RE
        .get_or_init(|| Regex::new(r"◉ (?P<cwd>[^\r\n]*?)──➤").expect("prompt regex must compile"));
    let stripped = strip_ansi_codes(output);
    prompt_regex
        .captures_iter(&stripped)
        .filter_map(|captures| captures.name("cwd").map(|cwd| cwd.as_str().trim()))
        .filter(|cwd| !cwd.is_empty())
        .last()
        .map(PathBuf::from)
}

fn rstrip_lines(lines: &[String]) -> String {
    lines.iter().map(|line| line.trim_end()).collect::<Vec<_>>().join("\n")
}

fn get_incremental_output(old_output: &[String], new_output: &[String]) -> Vec<String> {
    if old_output.is_empty() {
        return new_output.to_vec();
    }

    let old_len = old_output.len();
    let new_len = new_output.len();
    for index in (0..new_len).rev() {
        if new_output[index] != old_output[old_len - 1] {
            continue;
        }

        let mut matched = true;
        for candidate in (0..index).rev() {
            let distance = index - candidate;
            if distance >= old_len {
                break;
            }
            let old_index = old_len - 1 - distance;
            if new_output[candidate] != old_output[old_index] {
                matched = false;
                break;
            }
        }
        if matched {
            return new_output[index + 1..].to_vec();
        }
    }
    new_output.to_vec()
}

async fn poll_shell(shell: &SharedPtyShell) -> bool {
    let mut guard = shell.lock().await;
    match guard.as_mut() {
        Some(shell) => shell.poll_output_nonblocking(),
        None => true,
    }
}

async fn poll_shell_generation(
    shell: &SharedPtyShell,
    expected_generation: Option<u64>,
) -> Result<bool> {
    let mut guard = shell.lock().await;
    match guard.as_mut() {
        Some(shell) => {
            ensure_expected_generation(shell.command_generation(), expected_generation)?;
            Ok(shell.poll_output_nonblocking())
        }
        None => Ok(true),
    }
}

async fn snapshot_shell_generation(
    shell: &SharedPtyShell,
    expected_generation: Option<u64>,
) -> Result<String> {
    let mut guard = shell.lock().await;
    let Some(shell) = guard.as_mut() else { return Ok(String::new()) };
    ensure_expected_generation(shell.command_generation(), expected_generation)?;
    Ok(shell.output_snapshot())
}

fn ensure_expected_generation(generation: u64, expected_generation: Option<u64>) -> Result<()> {
    if let Some(expected) = expected_generation.filter(|expected| *expected != generation) {
        return Err(WinxError::InvalidInput(format!(
            "Foreground command generation {generation} no longer matches Task generation {expected}. The Task will not read or control a newer command."
        )));
    }
    Ok(())
}

async fn drain_until_prompt(shell: &SharedPtyShell, budget_secs: f64) -> bool {
    let start = Instant::now();
    loop {
        if poll_shell(shell).await {
            return true;
        }
        if start.elapsed().as_secs_f64() >= budget_secs {
            return false;
        }
        sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

pub(super) async fn clear_to_run_async(shell: &SharedPtyShell, max_wait_secs: f64) -> bool {
    if drain_until_prompt(shell, max_wait_secs.min(0.5)).await {
        return true;
    }
    {
        let mut guard = shell.lock().await;
        if let Some(shell) = guard.as_mut() {
            if let Err(error) = shell.send_interrupt() {
                warn!("clear_to_run: failed to send Ctrl-C: {error}");
            }
        }
    }
    drain_until_prompt(shell, max_wait_secs).await
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)] // shell/rendering controls are intentionally kept explicit
pub(super) async fn wait_for_output(
    bash_state: &mut BashState,
    shell: &SharedPtyShell,
    timeout_secs: f64,
    is_background: bool,
    background_id: Option<&str>,
    is_status_check: bool,
    mut delivery_cursor: Option<&mut ShellDeliveryCursor>,
    compact_output: bool,
    expected_generation: Option<u64>,
) -> Result<BashCommandRuntimeResult> {
    let start = Instant::now();
    let (generation, legacy_delivered) = {
        let guard = shell.lock().await;
        match guard.as_ref() {
            Some(shell) => {
                ensure_expected_generation(shell.command_generation(), expected_generation)?;
                (shell.command_generation(), shell.delivered_output())
            }
            None => (0, String::new()),
        }
    };
    let mut previously_delivered = match delivery_cursor.as_deref_mut() {
        Some(cursor) => {
            cursor.sync_generation(generation);
            cursor.delivered_output.clone()
        }
        None => legacy_delivered,
    };
    let mut complete = false;

    loop {
        if start.elapsed().as_secs_f64() >= timeout_secs {
            break;
        }
        complete = poll_shell_generation(shell, expected_generation).await?;
        if complete {
            break;
        }
        sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
    if complete {
        sleep(Duration::from_millis(POST_PROMPT_DRAIN_MS)).await;
        poll_shell_generation(shell, expected_generation).await?;
    }
    let mut output = snapshot_shell_generation(shell, expected_generation).await?;

    if let Some(cursor) = delivery_cursor.as_deref_mut() {
        let generation = {
            let guard = shell.lock().await;
            let generation = guard.as_ref().map_or(0, PtyShell::command_generation);
            ensure_expected_generation(generation, expected_generation)?;
            generation
        };
        if cursor.generation != Some(generation) {
            cursor.sync_generation(generation);
            previously_delivered.clear();
        }
    }

    if !complete && is_status_check {
        let mut patience = OUTPUT_WAIT_PATIENCE;
        let mut last_incremental = wcgw_incremental_text(&output, &previously_delivered);
        if last_incremental.is_empty() {
            patience -= 1;
        }

        while start.elapsed().as_secs_f64() < timeout_secs && patience > 0 {
            let remaining = (timeout_secs - start.elapsed().as_secs_f64()).max(0.0);
            if remaining < 0.1 {
                break;
            }
            sleep(Duration::from_secs_f64(0.5_f64.min(remaining))).await;
            let done = poll_shell_generation(shell, expected_generation).await?;
            let new_output = snapshot_shell_generation(shell, expected_generation).await?;
            if done {
                complete = true;
                output = new_output;
                break;
            }

            let new_incremental = wcgw_incremental_text(&new_output, &previously_delivered);
            if new_incremental == last_incremental {
                patience -= 1;
            } else {
                patience = OUTPUT_WAIT_PATIENCE;
            }
            last_incremental = new_incremental;
            output = new_output;
        }
    }

    if complete && !is_background {
        if let Some(cwd) = extract_prompt_cwd(&output) {
            bash_state.cwd = cwd;
        }
    }

    let rendered = wcgw_incremental_text(&output, &previously_delivered);
    if let Some(cursor) = delivery_cursor {
        cursor.delivered_output.clone_from(&output);
    } else {
        let mut guard = shell.lock().await;
        if let Some(shell) = guard.as_mut() {
            ensure_expected_generation(shell.command_generation(), expected_generation)?;
            shell.mark_output_delivered(&output);
        }
    }

    let rendered = {
        let before_lines = rendered.lines().count();
        match crate::utils::output_compress::compress_output(&rendered) {
            Some(compressed) => {
                debug!(
                    from_lines = before_lines,
                    to_lines = compressed.lines().count(),
                    "winx collapsed mechanical repetition in shell output"
                );
                compressed
            }
            None => rendered,
        }
    };
    let rendered = truncate_to_token_budget(&rendered, MAX_OUTPUT_TOKENS).into_owned();
    let (running_for, exit_code, shell_cwd, scratch, output_truncated) =
        read_status_extras(shell, complete, expected_generation).await?;
    let cwd = shell_cwd.as_deref().unwrap_or(&bash_state.cwd);
    let state = status_state(background_id, !complete, running_for, exit_code, cwd, None);
    let status = render_status(bash_state, &state);
    Ok(runtime_rendered(
        format!("{rendered}{scratch}"),
        &status,
        state,
        compact_output,
        Some(generation),
        output_truncated,
    ))
}

async fn read_status_extras(
    shell: &SharedPtyShell,
    complete: bool,
    expected_generation: Option<u64>,
) -> Result<(Option<Duration>, Option<i32>, Option<PathBuf>, String, bool)> {
    let guard = shell.lock().await;
    let Some(shell) = guard.as_ref() else {
        return Ok((None, None, None, String::new(), false));
    };
    ensure_expected_generation(shell.command_generation(), expected_generation)?;
    let running_for = if complete { None } else { shell.command_elapsed() };
    let exit_code = if complete { shell.last_exit_code } else { None };
    let cwd = Some(shell.current_cwd().to_path_buf());
    let pointer = scratch_pointer(shell.output_truncated, shell.scratch_path());
    Ok((running_for, exit_code, cwd, pointer, shell.output_truncated))
}

fn scratch_pointer(output_truncated: bool, scratch_path: Option<&Path>) -> String {
    match (output_truncated, scratch_path) {
        (true, Some(path)) => format!(
            "\n\n---\n[Output was truncated to fit context. The earlier (dropped) output was \
             saved to:\n{}\nRead it with ReadFiles or grep it via BashCommand.]\n---",
            path.display()
        ),
        _ => String::new(),
    }
}

pub(super) fn finalize_tombstone(
    bash_state: &BashState,
    id: &str,
    tombstone: ExitedShellInfo,
    action: &BashCommandAction,
    compact_output: bool,
) -> Result<BashCommandRuntimeResult> {
    let ExitedShellInfo {
        last_command,
        final_output,
        exit_code,
        cwd,
        output_truncated,
        scratch_path,
        ..
    } = tombstone;
    match action {
        BashCommandAction::StatusCheck { .. }
        | BashCommandAction::Screen { .. }
        | BashCommandAction::WaitForTurn { .. } => {
            let rendered = wcgw_incremental_text(final_output.as_ref(), "");
            let rendered = truncate_to_token_budget(&rendered, MAX_OUTPUT_TOKENS).into_owned();
            let state = status_state(Some(id), false, None, exit_code, &cwd, None);
            let pointer = scratch_pointer(output_truncated, scratch_path.as_deref());
            let status = render_status(bash_state, &state);
            Ok(runtime_rendered(
                format!("{rendered}{pointer}"),
                &status,
                state,
                compact_output,
                None,
                output_truncated,
            ))
        }
        BashCommandAction::SendText { .. }
        | BashCommandAction::SendSpecials { .. }
        | BashCommandAction::SendAscii { .. } => Err(WinxError::InvalidInput(format!(
            "Background shell {id} already exited (last command: {last_command}).\nFinal captured output:\n{final_output}"
        ))),
        BashCommandAction::Command { .. } => {
            unreachable!("finalize_tombstone called for non-bg action")
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) async fn execute_status_check(
    bash_state: &mut BashState,
    background_shell: Option<SharedPtyShell>,
    is_background: bool,
    background_id: Option<&str>,
    timeout_secs: f64,
    scrollback_lines: Option<usize>,
    verbose: bool,
    mut delivery_cursor: Option<&mut ShellDeliveryCursor>,
    options: ShellActionOptions,
) -> Result<BashCommandRuntimeResult> {
    debug!("Processing StatusCheck action (verbose={verbose}, scrollback={scrollback_lines:?})");
    let shell = background_shell.unwrap_or_else(|| main_shell(bash_state));
    let _generation_gate = if options.expected_generation.is_some() && !is_background {
        Some(bash_state.foreground_command_gate.clone().lock_owned().await)
    } else {
        None
    };
    let (is_running, generation) = {
        let guard = shell.lock().await;
        guard
            .as_ref()
            .map_or((false, 0), |shell| (shell.command_running, shell.command_generation()))
    };

    if options.expected_generation.is_some_and(|expected| expected != generation) {
        return Err(WinxError::InvalidInput(format!(
            "Foreground command generation {generation} no longer matches Task generation {}. The Task will not read or control a newer command.",
            options.expected_generation.unwrap_or_default()
        )));
    }

    if !is_running && !is_background && options.expected_generation.is_none() {
        let mut manager = lock_session_store();
        let error = format!(
            "No command is currently running, so there's nothing to check. The previous \
             command already finished and its output was returned when it completed. Start a \
             new command, or pass a bg_command_id if you launched one in the background.\n{}",
            manager.get_running_info(&bash_state.current_thread_id)
        );
        return Err(WinxError::InvalidInput(error));
    }

    let response = wait_for_output(
        bash_state,
        &shell,
        timeout_secs,
        is_background,
        background_id,
        true,
        delivery_cursor.as_deref_mut(),
        options.compact_output,
        options.expected_generation,
    )
    .await?;

    if !verbose && scrollback_lines.is_none() {
        let (fingerprint, running_for, running, exit_code, cwd) = {
            let guard = shell.lock().await;
            let Some(shell) = guard.as_ref() else {
                return Err(WinxError::BashStateNotInitialized);
            };
            (
                PtyShell::fingerprint(&shell.output_snapshot()),
                shell.command_elapsed(),
                shell.command_running,
                (!shell.command_running).then_some(shell.last_exit_code).flatten(),
                shell.current_cwd().to_path_buf(),
            )
        };
        let previous_hash = if let Some(cursor) = delivery_cursor.as_mut() {
            cursor.last_returned_hash.replace(fingerprint)
        } else {
            let mut guard = shell.lock().await;
            guard.as_mut().and_then(|shell| shell.last_returned_hash.replace(fingerprint))
        };
        if previous_hash == Some(fingerprint) {
            let state = status_state(background_id, running, running_for, exit_code, &cwd, None);
            let status = render_status(bash_state, &state);
            return Ok(runtime_rendered(
                "no new output since last check".to_string(),
                &status,
                state,
                options.compact_output,
                Some(generation),
                false,
            ));
        }
    } else if !verbose {
        let fingerprint = {
            let guard = shell.lock().await;
            guard.as_ref().map(|shell| PtyShell::fingerprint(&shell.output_snapshot()))
        };
        if let Some(fingerprint) = fingerprint {
            if let Some(cursor) = delivery_cursor {
                cursor.last_returned_hash = Some(fingerprint);
            } else {
                let mut guard = shell.lock().await;
                if let Some(shell) = guard.as_mut() {
                    shell.last_returned_hash = Some(fingerprint);
                }
            }
        }
    }

    if let Some(lines) = scrollback_lines {
        if lines > 0 {
            let scrollback = {
                let guard = shell.lock().await;
                guard.as_ref().map(|shell| shell.collect_scrollback(lines)).unwrap_or_default()
            };
            if !scrollback.is_empty() {
                let count = scrollback.lines().count();
                let mut response = response;
                if let Some(output) = response.compact_output.as_mut() {
                    *output = format!(
                        "--- scrollback ({count} lines) ---\n{scrollback}\n--- latest ---\n{output}"
                    );
                } else {
                    response.result.output = format!(
                        "--- scrollback ({count} lines) ---\n{scrollback}\n--- latest ---\n{}",
                        response.result.output
                    );
                }
                return Ok(response);
            }
        }
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::truncate_to_token_budget;

    #[test]
    fn token_budget_applies_below_the_byte_fallback_cap() {
        let input = "alpha beta gamma delta ".repeat(100);
        let rendered = truncate_to_token_budget(&input, 20);
        assert!(rendered.starts_with("(...truncated)\n"));
        assert_ne!(rendered.as_ref(), input);
        let tail = rendered.strip_prefix("(...truncated)\n").unwrap_or_default();
        let tail_tokens = crate::utils::encoder::encode_ids(tail);
        assert!(tail_tokens.as_ref().is_some_and(|tokens| tokens.len() <= 19));
    }
}
