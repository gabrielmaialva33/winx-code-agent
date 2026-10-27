//! `BashCommand` facade and action dispatcher.
//!
//! Execution, output/status rendering, interactive input, and TUI driving live
//! in focused submodules under `tools/bash_command/`.

mod execution;
mod interaction;
mod output;
mod tui;
mod workflow;

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, error, info};

pub use super::background_shell::{BackgroundShellManager, ExitedShellInfo};
use crate::errors::{Result, WinxError};
use crate::runtime::{
    lock_session_store, BashCommandRuntimeResult, EmbeddedShellRuntime, ShellActionOptions,
    ShellExecutionToken, ShellRuntime, ShellTarget,
};
use crate::state::bash_state::BashState;
use crate::state::live_terminal::ScreenUpdate;
use crate::state::pty::PtyShell;
use crate::types::{normalize_thread_id, BashCommand, BashCommandAction};

use execution::{execute_command, CommandExecutionContext};
use interaction::{execute_send_ascii, execute_send_specials, execute_send_text};
use output::{execute_status_check, finalize_tombstone};
use tui::{execute_screen, execute_wait_for_turn};

type SharedPtyShell = Arc<Mutex<Option<PtyShell>>>;

/// Authoritative process state produced by the shell runtime. It is transported
/// separately from human-readable terminal output, so command text cannot spoof
/// orchestration state by printing Winx-looking markers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashProcessStatus {
    Running,
    Exited,
}

/// Machine-readable state of one `BashCommand` action.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BashCommandState {
    pub process_status: BashProcessStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_for_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_state: Option<crate::state::turn::TurnState>,
}

impl BashCommandState {
    pub const fn is_running(&self) -> bool {
        matches!(self.process_status, BashProcessStatus::Running)
    }
}

/// Human-readable output plus runtime-owned state. Only `state` drives MCP
/// orchestration; `output` is presentation and may contain arbitrary child text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BashCommandResult {
    pub output: String,
    pub state: BashCommandState,
}

pub(crate) fn runtime_rendered(
    mut body: String,
    legacy_status: &str,
    state: BashCommandState,
    compact_requested: bool,
    command_generation: Option<u64>,
    output_truncated: bool,
) -> BashCommandRuntimeResult {
    let compact_output = if compact_requested {
        Some(std::mem::take(&mut body))
    } else {
        body.push_str(legacy_status);
        None
    };
    BashCommandRuntimeResult {
        result: BashCommandResult { output: body, state },
        compact_output,
        command_generation,
        execution_token: None,
        action_context: None,
        generation_bound_actions: true,
        dropped_output_file: None,
        output_truncated,
        workflow: None,
    }
}

/// Per-adapter delivery state used by daemon guardians. Embedded callers keep
/// using the cursor stored directly on `PtyShell`, preserving legacy behavior.
#[derive(Debug, Default)]
pub(crate) struct ShellDeliveryCursor {
    generation: Option<u64>,
    delivered_output: String,
    last_returned_hash: Option<u64>,
    screen_snapshot: Option<Vec<String>>,
}

impl ShellDeliveryCursor {
    fn sync_generation(&mut self, generation: u64) {
        if self.generation != Some(generation) {
            self.generation = Some(generation);
            self.delivered_output.clear();
            self.last_returned_hash = None;
            self.screen_snapshot = None;
        }
    }

    fn screen_update(&mut self, current: Vec<String>, threshold: usize) -> ScreenUpdate {
        let update = match self.screen_snapshot.as_ref() {
            Some(previous) => {
                let rows = previous.len().max(current.len());
                let changed = (0..rows)
                    .filter_map(|index| {
                        let before = previous.get(index).map_or("", String::as_str);
                        let after = current.get(index).map_or("", String::as_str);
                        (before != after).then(|| (index + 1, after.to_string()))
                    })
                    .collect::<Vec<_>>();
                if changed.is_empty() {
                    ScreenUpdate::Unchanged
                } else if changed.len() <= threshold {
                    ScreenUpdate::Diff(changed)
                } else {
                    ScreenUpdate::Full(current.clone())
                }
            }
            None => ScreenUpdate::Full(current.clone()),
        };
        self.screen_snapshot = Some(current);
        update
    }
}

pub(crate) fn main_shell(bash_state: &BashState) -> SharedPtyShell {
    let mut store = lock_session_store();
    store.bind_main(&bash_state.current_thread_id, &bash_state.pty_shell);
    store
        .resolve(&bash_state.current_thread_id, &ShellTarget::Main)
        .unwrap_or_else(|| bash_state.pty_shell.clone())
}

// Default block window when the client omits `wait_for_seconds`. Usage logs
// showed remote clients omit it and then burn one full LLM round-trip per 5 s
// re-check on long commands (runs of up to 18 consecutive polls); a larger
// window resolves most commands in a single call while staying far below the
// 120 s HTTP request timeout.
const DEFAULT_TIMEOUT: f64 = 15.0;
fn effective_wait_for_seconds(wait_for_seconds: Option<f32>) -> f64 {
    wait_for_seconds.map_or(DEFAULT_TIMEOUT, |seconds| f64::from(seconds).max(0.0))
}

/// Guardian foreground preflight. The caller owns the session operation
/// writer, so this never races token validation and runs before delivery cursor
/// or foreground-command gates are acquired.
pub(crate) async fn guardian_foreground_shell_needs_reset(
    bash_state: &mut BashState,
    options: &ShellActionOptions,
) -> anyhow::Result<bool> {
    let shell = main_shell(bash_state);
    let (missing, running) = {
        let guard = shell.lock().await;
        (guard.is_none(), guard.as_ref().is_some_and(|shell| shell.command_running))
    };
    if running {
        return Ok(false);
    }
    if missing {
        bash_state.init_pty_shell().await?;
        return Ok(false);
    }
    let cleared = output::clear_to_run_async(&shell, DEFAULT_TIMEOUT).await;
    #[cfg(test)]
    let cleared = cleared && !options.force_clear_to_run_failure;
    #[cfg(not(test))]
    let _ = options;
    Ok(!cleared)
}

fn send_utf8_in_byte_chunks(shell: &mut PtyShell, text: &str, chunk_size: usize) -> Result<()> {
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + chunk_size).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            end =
                text[start..].char_indices().nth(1).map_or(text.len(), |(index, _)| start + index);
        }
        shell.send_text(&text[start..end]).map_err(|error| {
            WinxError::CommandExecutionError(format!("Failed to write PTY input: {error}"))
        })?;
        start = end;
    }
    Ok(())
}

/// Handle a `BashCommand` using the embedded runtime.
pub async fn handle_tool_call(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
) -> Result<String> {
    handle_tool_call_with_runtime(&EmbeddedShellRuntime, bash_state, command).await
}

/// Backwards-compatible text-only API used by direct library callers.
pub async fn handle_tool_call_with_runtime(
    runtime: &dyn ShellRuntime,
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
) -> Result<String> {
    Ok(handle_tool_call_with_runtime_detailed(runtime, bash_state, command).await?.output)
}

/// Execute a `BashCommand` while preserving the runtime-owned typed state.
pub async fn handle_tool_call_with_runtime_detailed(
    runtime: &dyn ShellRuntime,
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
) -> Result<BashCommandResult> {
    runtime.run_action(bash_state, command).await
}

pub(crate) async fn handle_embedded_tool_call(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
) -> Result<BashCommandResult> {
    Ok(handle_embedded_tool_call_inner(
        bash_state,
        command,
        None,
        ShellActionOptions::default(),
        false,
    )
    .await?
    .result)
}

pub(crate) async fn handle_embedded_tool_call_detailed(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
    options: ShellActionOptions,
) -> Result<BashCommandRuntimeResult> {
    handle_embedded_tool_call_inner(bash_state, command, None, options, false).await
}

pub(crate) async fn handle_embedded_tool_call_with_cursor_detailed(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
    delivery_cursor: &Arc<Mutex<ShellDeliveryCursor>>,
    options: ShellActionOptions,
    foreground_preflight_complete: bool,
) -> Result<BashCommandRuntimeResult> {
    let mut delivery_cursor = delivery_cursor.lock().await;
    handle_embedded_tool_call_inner(
        bash_state,
        command,
        Some(&mut delivery_cursor),
        options,
        foreground_preflight_complete,
    )
    .await
}

async fn capture_embedded_state(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    thread_id: &str,
) -> Result<(BashState, tokio::sync::OwnedRwLockReadGuard<()>)> {
    let guard = bash_state.lock().await;
    let state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
    let operation_barrier = lock_session_store().operation_barrier(thread_id);
    let operation_guard = operation_barrier.read_owned().await;
    Ok((state.clone(), operation_guard))
}

#[tracing::instrument(level = "info", skip(bash_state, command, delivery_cursor))]
async fn handle_embedded_tool_call_inner(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    command: BashCommand,
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
    options: ShellActionOptions,
    foreground_preflight_complete: bool,
) -> Result<BashCommandRuntimeResult> {
    let action_kind = match &command.action_json {
        BashCommandAction::Command { .. } => "command",
        BashCommandAction::StatusCheck { .. } => "status_check",
        BashCommandAction::SendText { .. } => "send_text",
        BashCommandAction::SendSpecials { .. } => "send_specials",
        BashCommandAction::SendAscii { .. } => "send_ascii",
        BashCommandAction::Screen { .. } => "screen",
        BashCommandAction::WaitForTurn { .. } => "wait_for_turn",
        BashCommandAction::Inspect { .. } => "inspect",
        BashCommandAction::Prompt { .. } => "prompt",
    };
    info!(thread_id = %command.thread_id, action = action_kind, "BashCommand tool called");

    let thread_id = normalize_thread_id(&command.thread_id);
    if thread_id.is_empty() {
        error!("Empty thread_id provided in BashCommand");
        return Err(WinxError::ThreadIdMismatch(
            "Error: No saved bash state found for thread ID \"\". Please initialize first with this ID."
                .to_string(),
        ));
    }

    let mut local_state = {
        let guard = bash_state.lock().await;
        let Some(state) = &*guard else {
            error!("BashState not initialized");
            return Err(WinxError::BashStateNotInitialized);
        };
        state.clone()
    };

    if thread_id != local_state.current_thread_id {
        let loaded = local_state.load_state_from_disk(&thread_id).map_err(|error| {
            WinxError::CommandExecutionError(format!(
                "Failed to load saved bash state for thread_id `{thread_id}`: {error}"
            ))
        })?;
        if !loaded {
            return Err(WinxError::ThreadIdMismatch(format!(
                "Error: No saved bash state found for thread_id `{thread_id}`. Please initialize first with this ID."
            )));
        }
        if let Some(state) = bash_state.lock().await.as_mut() {
            *state = local_state.clone();
        }
    }

    // BashState -> session barrier is the same order used by
    // configure_session. Keep BashState locked until the owned read guard is
    // acquired so reset cannot replace the incarnation after capture but
    // before token validation/action execution.
    let (captured_state, operation_guard) = capture_embedded_state(bash_state, &thread_id).await?;
    local_state = captured_state;

    if let Some(expected) = options.expected_execution.as_ref() {
        if expected.guardian_epoch == "embedded" {
            let current_epoch = local_state
                .pty_shell
                .lock()
                .await
                .as_ref()
                .map(|shell| format!("{:016x}", shell.incarnation()));
            if Some(expected.session_epoch.as_str()) != current_epoch.as_deref() {
                return Err(WinxError::InvalidInput(
                    "execution token belongs to a previous embedded shell incarnation".to_string(),
                ));
            }
        }
    }

    lock_session_store().bind_main(&local_state.current_thread_id, &local_state.pty_shell);
    let timeout_secs = effective_wait_for_seconds(command.wait_for_seconds);
    let mut result = execute_bash_action(
        &mut local_state,
        &command.action_json,
        timeout_secs,
        delivery_cursor,
        options,
        foreground_preflight_complete,
    )
    .await;

    let completed_session_epoch = local_state.pty_shell.lock().await.as_ref().map_or_else(
        || "uninitialized".to_string(),
        |shell| format!("{:016x}", shell.incarnation()),
    );
    if let Ok(outcome) = &mut result {
        outcome.action_context = Some(crate::runtime::ShellActionContext {
            thread_id: local_state.current_thread_id.clone(),
            workspace_root: local_state.workspace_root.clone(),
        });
        if let Some(generation) = outcome.command_generation {
            outcome.execution_token = Some(ShellExecutionToken {
                guardian_epoch: "embedded".to_string(),
                session_epoch: completed_session_epoch.clone(),
                generation,
            });
        }
        if let BashCommandAction::Command { ref command, .. } = command.action_json {
            strip_submitted_command_echo(outcome, command);
        }
    }

    drop(operation_guard);
    sync_embedded_cwd_if_current(bash_state, &local_state, &completed_session_epoch).await;

    result
}

fn strip_submitted_command_echo(outcome: &mut BashCommandRuntimeResult, command: &str) {
    let command = command.trim();
    if outcome.result.output.starts_with(command) {
        outcome.result.output = outcome.result.output[command.len()..].to_string();
    }
    if outcome.compact_output.as_ref().is_some_and(|output| output.starts_with(command)) {
        let output = outcome.compact_output.take().unwrap_or_default();
        outcome.compact_output = Some(output[command.len()..].to_string());
    }
}

async fn sync_embedded_cwd_if_current(
    bash_state: &Arc<Mutex<Option<BashState>>>,
    local_state: &BashState,
    completed_session_epoch: &str,
) {
    // configure_session callers own BashState before taking the operation
    // barrier. Acquire BashState only after releasing the read side above and
    // never let a completed old incarnation overwrite reset's cwd.
    if let Some(state) = bash_state.lock().await.as_mut() {
        let current_session_epoch = state.pty_shell.lock().await.as_ref().map_or_else(
            || "uninitialized".to_string(),
            |shell| format!("{:016x}", shell.incarnation()),
        );
        if current_session_epoch == completed_session_epoch {
            state.cwd.clone_from(&local_state.cwd);
        }
    }
}

/// Persist accepted launch evidence while the foreground command gate is held.
/// Backend acceptance and later in-TUI conversation switches remain unverified.
fn record_foreground_agent_launch(
    bash_state: &BashState,
    command: &str,
    launch_cwd: &std::path::Path,
) {
    use crate::state::agent_resume;
    let thread_id = &bash_state.current_thread_id;
    match agent_resume::detect_agent_launch(command) {
        Some(agent) => {
            let record = agent_resume::AgentSessionRecord::from_launch(
                agent,
                command,
                launch_cwd,
                &bash_state.workspace_root,
                thread_id,
                // The final guardian execution token is assigned outside this boundary.
                None,
            );
            if let Err(error) = agent_resume::save_agent_session(thread_id, &record) {
                debug!(%error, agent, "failed to persist agent-session record");
                if let Err(clear_error) = agent_resume::clear_agent_session(thread_id) {
                    debug!(%clear_error, agent, "failed to invalidate the previous agent-session record");
                }
            }
        }
        None => {
            if let Err(error) = agent_resume::clear_agent_session(thread_id) {
                debug!(%error, "failed to clear agent-session record");
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn execute_bash_action(
    bash_state: &mut BashState,
    action: &BashCommandAction,
    timeout_secs: f64,
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
    options: ShellActionOptions,
    foreground_preflight_complete: bool,
) -> Result<BashCommandRuntimeResult> {
    let mut is_background = false;
    let mut background_id: Option<String> = None;

    let background_shell = match action {
        BashCommandAction::Command { .. } => None,
        BashCommandAction::StatusCheck { bg_command_id, .. }
        | BashCommandAction::SendText { bg_command_id, .. }
        | BashCommandAction::SendSpecials { bg_command_id, .. }
        | BashCommandAction::SendAscii { bg_command_id, .. }
        | BashCommandAction::Screen { bg_command_id, .. }
        | BashCommandAction::WaitForTurn { bg_command_id, .. }
        | BashCommandAction::Inspect { bg_command_id, .. }
        | BashCommandAction::Prompt { bg_command_id, .. } => {
            if let Some(id) = bg_command_id {
                let mut manager = lock_session_store();
                manager.prune_finished_shells();
                if let Some(shell) = manager.get_shell(&bash_state.current_thread_id, id) {
                    is_background = true;
                    background_id = Some(id.clone());
                    Some(shell)
                } else if let Some(tombstone) =
                    manager.peek_tombstone(&bash_state.current_thread_id, id)
                {
                    workflow::validate_tombstone(&tombstone, &options)?;
                    drop(manager);
                    return finalize_tombstone(
                        bash_state,
                        id,
                        tombstone,
                        action,
                        options.compact_output,
                    );
                } else {
                    let error = format!(
                        "No shell found running with command id {}.\n{}",
                        id,
                        manager.get_running_info(&bash_state.current_thread_id)
                    );
                    return Err(WinxError::InvalidInput(error));
                }
            } else {
                None
            }
        }
    };

    match action {
        BashCommandAction::Command { command, is_background, allow_multi } => {
            execute_command(
                bash_state,
                command,
                *is_background,
                *allow_multi,
                timeout_secs,
                delivery_cursor,
                CommandExecutionContext { options: &options, foreground_preflight_complete },
            )
            .await
        }
        BashCommandAction::StatusCheck { scrollback_lines, verbose, .. } => {
            execute_status_check(
                bash_state,
                background_shell,
                is_background,
                background_id.as_deref(),
                timeout_secs,
                *scrollback_lines,
                *verbose,
                delivery_cursor,
                options,
            )
            .await
        }
        BashCommandAction::SendText { send_text, submit, .. } => {
            execute_send_text(
                bash_state,
                send_text,
                *submit,
                background_shell,
                is_background,
                background_id.as_deref(),
                timeout_secs,
                delivery_cursor,
                options.compact_output,
            )
            .await
        }
        BashCommandAction::SendSpecials { send_specials, submit, .. } => {
            execute_send_specials(
                bash_state,
                send_specials,
                *submit,
                background_shell,
                is_background,
                background_id.as_deref(),
                timeout_secs,
                delivery_cursor,
                options.compact_output,
            )
            .await
        }
        BashCommandAction::SendAscii { send_ascii, submit, .. } => {
            execute_send_ascii(
                bash_state,
                send_ascii,
                *submit,
                background_shell,
                is_background,
                background_id.as_deref(),
                timeout_secs,
                delivery_cursor,
                options.compact_output,
            )
            .await
        }
        BashCommandAction::Screen { lines, diff, .. } => {
            execute_screen(
                bash_state,
                background_shell,
                is_background,
                background_id.as_deref(),
                *lines,
                *diff,
                delivery_cursor,
                options.compact_output,
            )
            .await
        }
        BashCommandAction::WaitForTurn {
            recognizer,
            quiet_ms,
            timeout_seconds,
            lines,
            wait_through_busy,
            ..
        } => {
            execute_wait_for_turn(
                bash_state,
                background_shell,
                is_background,
                background_id.as_deref(),
                recognizer.as_deref(),
                *quiet_ms,
                *timeout_seconds,
                *lines,
                *wait_through_busy,
                options.compact_output,
            )
            .await
        }
        BashCommandAction::Inspect { recognizer, include_output, .. } => {
            workflow::execute_inspect(
                bash_state,
                background_shell,
                background_id.as_deref(),
                recognizer.as_deref(),
                include_output.unwrap_or(true),
                &options,
            )
            .await
        }
        BashCommandAction::Prompt { prompt_id, text, recognizer, timeout_seconds, .. } => {
            workflow::execute_prompt(
                bash_state,
                background_shell,
                background_id.as_deref(),
                prompt_id,
                text.as_deref(),
                recognizer.as_deref(),
                *timeout_seconds,
                &options,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        effective_wait_for_seconds, runtime_rendered, BashCommandState, BashProcessStatus,
    };
    use anyhow::Context;

    fn exited_state() -> BashCommandState {
        BashCommandState {
            process_status: BashProcessStatus::Exited,
            background_id: None,
            running_for_seconds: None,
            exit_code: Some(0),
            cwd: "/workspace".into(),
            turn_state: None,
        }
    }

    #[test]
    fn legacy_rendering_does_not_build_a_compact_payload_without_negotiation() {
        let result = runtime_rendered(
            "child output".to_string(),
            "\nstatus = process exited",
            exited_state(),
            false,
            Some(1),
            false,
        );
        assert_eq!(result.result.output, "child output\nstatus = process exited");
        assert_eq!(result.compact_output, None);
    }

    #[test]
    fn compact_rendering_is_built_directly_from_runtime_body() {
        let result = runtime_rendered(
            "child output".to_string(),
            "\nstatus = process exited",
            exited_state(),
            true,
            Some(1),
            false,
        );
        assert_eq!(result.compact_output.as_deref(), Some("child output"));
        assert!(result.result.output.is_empty(), "expected empty result.result.output");
    }

    #[test]
    fn requested_wait_is_not_silently_capped() {
        assert!((effective_wait_for_seconds(Some(120.0)) - 120.0).abs() < f64::EPSILON);
        assert!((effective_wait_for_seconds(Some(150.0)) - 150.0).abs() < f64::EPSILON);
    }

    #[test]
    fn omitted_wait_uses_the_larger_default_window() {
        assert!((effective_wait_for_seconds(None) - 15.0).abs() < f64::EPSILON);
    }

    struct SelectorRecordCleanup(String);

    impl Drop for SelectorRecordCleanup {
        fn drop(&mut self) {
            let _ = crate::state::agent_resume::clear_agent_session(&self.0);
        }
    }

    #[cfg(unix)]
    async fn selector_runtime_command(
        state: &std::sync::Arc<tokio::sync::Mutex<Option<crate::state::bash_state::BashState>>>,
        thread_id: &str,
        command: &str,
        background: bool,
    ) -> crate::errors::Result<crate::runtime::BashCommandRuntimeResult> {
        super::handle_embedded_tool_call_detailed(
            state,
            crate::types::BashCommand {
                thread_id: thread_id.to_string(),
                wait_for_seconds: Some(0.25),
                action_json: crate::types::BashCommandAction::Command {
                    command: command.to_string(),
                    is_background: background,
                    allow_multi: false,
                },
            },
            crate::runtime::ShellActionOptions::default(),
        )
        .await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn selector_recovery_new_conversation_launch_persists_its_target_across_reload(
    ) -> anyhow::Result<()> {
        let workspace = tempfile::tempdir().context("workspace")?;
        let thread_id =
            format!("selector_capture_{}_{}", std::process::id(), rand::random::<u64>());
        let _cleanup = SelectorRecordCleanup(thread_id.clone());
        let mut state = crate::state::bash_state::BashState::new();
        state.current_thread_id = thread_id.clone();
        state.workspace_root = workspace.path().canonicalize().context("canonical workspace")?;
        state.cwd = state.workspace_root.clone();
        state.initialized = true;
        state.init_pty_shell().await.context("PTY")?;
        let state = std::sync::Arc::new(tokio::sync::Mutex::new(Some(state)));
        selector_runtime_command(
            &state,
            &thread_id,
            "claude() { printf 'fake backend accepted selector: %s\\n' \"$*\"; }",
            false,
        )
        .await
        .context("install controlled shell fixture")?;
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let outcome = selector_runtime_command(
            &state,
            &thread_id,
            &format!("claude --session-id {id}"),
            false,
        )
        .await
        .context("accepted launch")?;
        assert!(outcome.command_generation.is_some(), "fixture must actually start a command");
        let recorded = crate::state::agent_resume::load_agent_session(&thread_id)
            .context("reload persisted record")?
            .context("accepted agent recorded")?;
        assert_eq!(recorded.resume_command(), format!("claude --resume {id}"));
        let encoded = serde_json::to_value(&recorded).context("serialize")?;
        assert_eq!(encoded["conversation_selector"]["source"], "launch_argument");
        assert_eq!(encoded["conversation_selector"]["id"], id);
        assert_eq!(
            encoded["workspace_root"],
            workspace.path().canonicalize().context("workspace")?.to_string_lossy().as_ref()
        );
        assert_eq!(encoded["scoped_thread_id"], thread_id);
        // This controls argv only. The fixture makes no claim about a real backend's session.
        selector_runtime_command(
            &state,
            &thread_id,
            "printf 'accepted non-agent command\\n'",
            false,
        )
        .await
        .context("accepted non-agent command")?;
        assert!(crate::state::agent_resume::load_agent_session(&thread_id)
            .context("cleared record")?
            .is_none());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn selector_recovery_busy_and_background_requests_preserve_the_foreground_record(
    ) -> anyhow::Result<()> {
        let workspace = tempfile::tempdir().context("workspace")?;
        let thread_id = format!("selector_busy_{}_{}", std::process::id(), rand::random::<u64>());
        let _cleanup = SelectorRecordCleanup(thread_id.clone());
        let mut state = crate::state::bash_state::BashState::new();
        state.current_thread_id = thread_id.clone();
        state.workspace_root = workspace.path().canonicalize().context("canonical workspace")?;
        state.cwd = state.workspace_root.clone();
        state.initialized = true;
        state.init_pty_shell().await.context("PTY")?;
        let state = std::sync::Arc::new(tokio::sync::Mutex::new(Some(state)));
        selector_runtime_command(&state, &thread_id, "claude() { sleep 1; }", false)
            .await
            .context("install controlled shell fixture")?;
        let id = "550e8400-e29b-41d4-a716-446655440000";
        selector_runtime_command(&state, &thread_id, &format!("claude --session-id {id}"), false)
            .await
            .context("agent launch")?;
        let before = crate::state::agent_resume::load_agent_session(&thread_id)
            .context("record")?
            .context("foreground record")?;
        let _ = selector_runtime_command(
            &state,
            &thread_id,
            "printf 'must not replace a busy record'",
            false,
        )
        .await;
        assert_eq!(
            crate::state::agent_resume::load_agent_session(&thread_id)
                .context("record after busy request")?,
            Some(before.clone())
        );
        let _ =
            selector_runtime_command(&state, &thread_id, "printf 'independent background'", true)
                .await;
        assert_eq!(
            crate::state::agent_resume::load_agent_session(&thread_id)
                .context("record after background request")?,
            Some(before)
        );
        Ok(())
    }
    #[test]
    fn selector_recovery_failed_accepted_launch_persistence_invalidates_older_target(
    ) -> anyhow::Result<()> {
        let workspace = tempfile::tempdir().context("workspace")?;
        let thread_id =
            format!("selector_save_fail_{}_{}", std::process::id(), rand::random::<u64>());
        let _cleanup = SelectorRecordCleanup(thread_id.clone());
        let mut state = crate::state::bash_state::BashState::new();
        state.current_thread_id = thread_id.clone();
        state.workspace_root = workspace.path().canonicalize().context("workspace")?;
        state.cwd = state.workspace_root.clone();
        let id = "550e8400-e29b-41d4-a716-446655440000";
        // This private writer is invoked only at the accepted foreground boundary.
        super::record_foreground_agent_launch(
            &state,
            &format!("claude --session-id {id}"),
            &state.cwd,
        );
        let prior = crate::state::agent_resume::load_agent_session(&thread_id)
            .context("prior record")?
            .context("accepted selector")?;
        assert_eq!(prior.resume_command(), format!("claude --resume {id}"));
        let oversized = format!("claude {}", "x".repeat(9 * 1024));
        super::record_foreground_agent_launch(&state, &oversized, &state.cwd);
        assert!(
            crate::state::agent_resume::load_agent_session(&thread_id)
                .context("record after failed persistence")?
                .is_none(),
            "a newer accepted launch must not leave the previous executable target"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn selector_recovery_launch_cwd_comes_from_the_shared_pty_after_a_prior_cd(
    ) -> anyhow::Result<()> {
        let workspace = tempfile::tempdir().context("workspace")?;
        let root = workspace.path().canonicalize().context("canonical workspace")?;
        let new_cwd = root.join("new");
        std::fs::create_dir(&new_cwd).context("new working directory")?;
        let thread_id =
            format!("selector_stale_cwd_{}_{}", std::process::id(), rand::random::<u64>());
        let _cleanup = SelectorRecordCleanup(thread_id.clone());
        let mut state = crate::state::bash_state::BashState::new();
        state.current_thread_id = thread_id.clone();
        state.workspace_root = root.clone();
        state.cwd = root.clone();
        state.initialized = true;
        state.init_pty_shell().await.context("PTY")?;
        let state = std::sync::Arc::new(tokio::sync::Mutex::new(Some(state)));
        selector_runtime_command(&state, &thread_id, "claude() { :; }", false)
            .await
            .context("controlled shell fixture")?;
        // Queued calls already hold BashState clones sharing this PTY and gate.
        // Keep that snapshot stale while another accepted call changes the real cwd.
        let queued_snapshot = state.lock().await.as_ref().context("initialized state")?.clone();
        let changed = selector_runtime_command(
            &state,
            &thread_id,
            &format!("cd '{}'", new_cwd.display()),
            false,
        )
        .await
        .context("prior cd")?;
        assert_eq!(changed.result.state.cwd, new_cwd, "PTY must report the completed cd");
        assert_eq!(queued_snapshot.cwd, root, "the queued snapshot must remain stale");
        let queued_slot = std::sync::Arc::new(tokio::sync::Mutex::new(Some(queued_snapshot)));
        let id = "550e8400-e29b-41d4-a716-446655440000";
        selector_runtime_command(
            &queued_slot,
            &thread_id,
            &format!("claude --session-id {id}"),
            false,
        )
        .await
        .context("launch through stale snapshot")?;
        let recorded = crate::state::agent_resume::load_agent_session(&thread_id)
            .context("reload")?
            .context("accepted selector record")?;
        assert_eq!(
            std::path::Path::new(&recorded.cwd),
            new_cwd.as_path(),
            "launch evidence must use cwd at PTY submission, not the queued snapshot"
        );
        assert_eq!(recorded.resume_command(), format!("claude --resume {id}"));
        Ok(())
    }
}
