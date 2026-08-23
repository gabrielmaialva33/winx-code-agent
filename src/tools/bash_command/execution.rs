use std::time::Duration;

use tokio::time::sleep;
use tracing::{debug, error, warn};

use super::output::{clear_to_run_async, render_status, status_state, wait_for_output};
use super::{
    main_shell, runtime_rendered, send_utf8_in_byte_chunks, ShellDeliveryCursor, DEFAULT_TIMEOUT,
};
use crate::errors::{Result, WinxError};
use crate::runtime::{lock_session_store, BashCommandRuntimeResult, ShellActionOptions};
use crate::state::bash_state::BashState;
use crate::state::pty::PtyShell;

const COMMAND_CHUNK_SIZE: usize = 64;

fn spawn_background_reaper(owner_thread_id: String, background_id: String) {
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_millis(100)).await;
            let shell = {
                let manager = lock_session_store();
                manager.get_shell(&owner_thread_id, &background_id)
            };
            let Some(shell) = shell else { return };

            let finished = {
                let mut guard = shell.lock().await;
                match guard.as_mut() {
                    Some(shell) => shell.poll_output_nonblocking() || !shell.is_alive(),
                    None => true,
                }
            };
            if finished {
                drop(shell);
                let removed = {
                    let mut manager = lock_session_store();
                    manager.prune_finished_shells();
                    manager.get_shell(&owner_thread_id, &background_id).is_none()
                };
                if removed {
                    return;
                }
            }
        }
    });
}

fn strip_tail_pipe(command: &str) -> String {
    strip_tail_pipe_impl(command, keep_tail_pipe())
}

fn strip_tail_pipe_impl(command: &str, keep: bool) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    if keep {
        return command.to_string();
    }
    #[allow(clippy::expect_used)]
    let regex = RE.get_or_init(|| {
        regex::Regex::new(r"\|\s*tail(?:\s+(?:-n\s*|-)?(\d+))?\s*$")
            .expect("tail-pipe regex must compile")
    });
    match regex.find(command) {
        Some(matched) => command[..matched.start()].trim_end().to_string(),
        None => command.to_string(),
    }
}

fn keep_tail_pipe() -> bool {
    crate::config::env_flag("WINX_KEEP_TAIL_PIPE")
}

#[allow(clippy::too_many_lines)]
pub(super) async fn execute_command(
    bash_state: &mut BashState,
    command: &str,
    is_background: bool,
    allow_multi: bool,
    timeout_secs: f64,
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
    options: &ShellActionOptions,
) -> Result<BashCommandRuntimeResult> {
    let stripped_command = strip_tail_pipe(command);
    let command = stripped_command.as_str();
    debug!(bytes = command.len(), allow_multi, "Processing Command action");

    if !bash_state.is_command_allowed(command) {
        error!(bytes = command.len(), "Command not allowed in current mode");
        return Err(WinxError::CommandNotAllowed(
            "Error: BashCommand not allowed in current mode".to_string(),
        ));
    }

    let command = command.trim();
    if !allow_multi {
        let allow_shell_probe = matches!(bash_state.mode, crate::types::Modes::Wcgw);
        crate::utils::bash_parser::assert_single_statement(command, allow_shell_probe)?;
    }
    if options.is_launch_cancelled() {
        return Err(WinxError::CommandExecutionError(
            "task was cancelled before command launch".to_string(),
        ));
    }
    if is_background {
        return execute_in_background(
            bash_state,
            command,
            timeout_secs,
            delivery_cursor,
            options.compact_output,
        )
        .await;
    }

    let foreground_gate = bash_state.foreground_command_gate.clone();
    let _foreground_guard = foreground_gate.lock_owned().await;
    if options.is_launch_cancelled() {
        return Err(WinxError::CommandExecutionError(
            "task was cancelled before command launch".to_string(),
        ));
    }
    let shell = main_shell(bash_state);
    {
        let guard = shell.lock().await;
        if let Some(shell) = guard.as_ref().filter(|shell| shell.command_running) {
            return Err(WinxError::CommandAlreadyRunning {
                current_command: shell.last_command.clone(),
                duration_seconds: shell
                    .command_elapsed()
                    .map_or(0.0, |elapsed| elapsed.as_secs_f64()),
            });
        }
    }

    if shell.lock().await.is_none() {
        bash_state.init_pty_shell().await.map_err(|error| {
            WinxError::CommandExecutionError(format!("Failed to init bash: {error}"))
        })?;
    }

    let needs_reset = if shell.lock().await.is_some() {
        let cleared = clear_to_run_async(&shell, DEFAULT_TIMEOUT).await;
        #[cfg(test)]
        let cleared = cleared && !options.force_clear_to_run_failure;
        if cleared {
            false
        } else {
            warn!("clear_to_run: shell still busy after Ctrl-C, resetting it");
            true
        }
    } else {
        false
    };
    if needs_reset {
        if let Err(error) = bash_state.init_pty_shell().await {
            warn!("Failed to reset shell after clear_to_run: {error}");
        }
    }

    let scratch_root = bash_state.workspace_root.clone();
    {
        let mut guard = shell.lock().await;
        let shell = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
        shell.output_buffer.clear();
        shell.output_truncated = false;
        shell.reset_scratch();
        shell.set_scratch_root(&scratch_root);
        shell.last_exit_code = None;
        shell.last_returned_hash = None;
        shell.mark_output_delivered("");
        send_utf8_in_byte_chunks(shell, command, COMMAND_CHUNK_SIZE)?;
        shell.send_special_key("Enter").map_err(|error| {
            WinxError::CommandExecutionError(format!("Failed to send newline: {error}"))
        })?;
        shell.last_command = command.to_string();
        shell.command_running = true;
        shell.mark_command_started();
    }

    wait_for_output(
        bash_state,
        &shell,
        timeout_secs,
        false,
        None,
        false,
        delivery_cursor,
        options.compact_output,
        None,
    )
    .await
}

async fn execute_in_background(
    bash_state: &mut BashState,
    command: &str,
    timeout_secs: f64,
    delivery_cursor: Option<&mut ShellDeliveryCursor>,
    compact_output: bool,
) -> Result<BashCommandRuntimeResult> {
    debug!(bytes = command.len(), "Executing command in background");
    let restricted_mode =
        matches!(bash_state.bash_command_mode.bash_mode, crate::types::BashMode::RestrictedMode);

    let background_id = {
        let cwd = bash_state.cwd.clone();
        let shell = tokio::task::spawn_blocking(move || PtyShell::new(&cwd, restricted_mode))
            .await
            .map_err(|error| {
                WinxError::CommandExecutionError(format!("bg shell init task failed: {error}"))
            })?
            .map_err(|error| {
                WinxError::CommandExecutionError(format!(
                    "Failed to start background shell: {error}"
                ))
            })?;
        lock_session_store().register_shell(&bash_state.current_thread_id, shell)?
    };

    let shell = {
        let manager = lock_session_store();
        manager.get_shell(&bash_state.current_thread_id, &background_id).ok_or_else(|| {
            WinxError::CommandExecutionError("Failed to get background shell".to_string())
        })?
    };

    let scratch_root = bash_state.workspace_root.clone();
    let send_result = {
        let mut guard = shell.lock().await;
        guard.as_mut().map(|shell| {
            shell.set_scratch_root(&scratch_root);
            shell.send_command(command)
        })
    };
    let Some(send_result) = send_result else {
        lock_session_store().remove_shell(&background_id);
        return Err(WinxError::BashStateNotInitialized);
    };
    if let Err(error) = send_result {
        lock_session_store().remove_shell(&background_id);
        return Err(WinxError::CommandExecutionError(format!(
            "Failed to send bg command: {error}"
        )));
    }
    debug!("bg[{background_id}]: send_command returned, replying with bg_command_id");
    spawn_background_reaper(bash_state.current_thread_id.clone(), background_id.clone());

    let _ = (timeout_secs, delivery_cursor);
    let generation = {
        let guard = shell.lock().await;
        guard.as_ref().map(PtyShell::command_generation)
    };
    let state = status_state(Some(&background_id), true, None, None, &bash_state.cwd, None);
    let output = render_status(bash_state, &state);
    Ok(runtime_rendered(String::new(), &output, state, compact_output, generation, false))
}

#[cfg(test)]
mod tests {
    use super::strip_tail_pipe_impl;

    #[test]
    fn strips_trailing_tail_by_default() {
        assert_eq!(strip_tail_pipe_impl("seq 1 5 | tail -2", false), "seq 1 5");
        assert_eq!(strip_tail_pipe_impl("cat log | tail -n 20", false), "cat log");
        assert_eq!(strip_tail_pipe_impl("cat log | tail", false), "cat log");
        assert_eq!(strip_tail_pipe_impl("ls -la|tail -5", false), "ls -la");
    }

    #[test]
    fn keeps_command_without_trailing_tail() {
        assert_eq!(strip_tail_pipe_impl("tail -f log | grep err", false), "tail -f log | grep err");
        assert_eq!(strip_tail_pipe_impl("echo hi", false), "echo hi");
        assert_eq!(
            strip_tail_pipe_impl("cat a | tail -5 | wc -l", false),
            "cat a | tail -5 | wc -l"
        );
    }

    #[test]
    fn keep_mode_preserves_tail_pipe() {
        assert_eq!(strip_tail_pipe_impl("seq 1 5 | tail -2", true), "seq 1 5 | tail -2");
        assert_eq!(strip_tail_pipe_impl("cat log | tail -n 20", true), "cat log | tail -n 20");
    }
}
