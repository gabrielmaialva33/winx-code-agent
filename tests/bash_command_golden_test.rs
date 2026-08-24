use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio::time::sleep;

use winx_code_agent::errors::{Result, WinxError};
use winx_code_agent::runtime::{EmbeddedShellRuntime, ShellActionOptions, ShellRuntime};
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::tools;
use winx_code_agent::types::{
    BashCommand, BashCommandAction, Initialize, InitializeType, ModeName, SpecialKey,
};

async fn setup_bash_state(thread_id: &str) -> Result<(Arc<Mutex<Option<BashState>>>, TempDir)> {
    let temp_dir = TempDir::new()?;
    let state = Arc::new(Mutex::new(None));
    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: temp_dir.path().to_string_lossy().into_owned(),
        thread_id: thread_id.to_string(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };

    tools::initialize::handle_tool_call(&state, init).await?;
    Ok((state, temp_dir))
}

fn command(thread_id: &str, command: &str, is_background: bool, wait: f32) -> BashCommand {
    BashCommand {
        action_json: BashCommandAction::Command {
            command: command.to_string(),
            is_background,
            allow_multi: true,
        },
        wait_for_seconds: Some(wait),
        thread_id: thread_id.to_string(),
    }
}

fn status(thread_id: &str, bg_command_id: Option<String>, wait: f32) -> BashCommand {
    BashCommand {
        action_json: BashCommandAction::StatusCheck {
            status_check: true,
            bg_command_id,
            scrollback_lines: None,
            verbose: false,
        },
        wait_for_seconds: Some(wait),
        thread_id: thread_id.to_string(),
    }
}

fn ctrl_c(thread_id: &str, bg_command_id: Option<String>) -> BashCommand {
    BashCommand {
        action_json: BashCommandAction::SendSpecials {
            send_specials: vec![SpecialKey::CtrlC],
            bg_command_id,
            submit: false,
        },
        wait_for_seconds: Some(0.2),
        thread_id: thread_id.to_string(),
    }
}

fn screen_diff(thread_id: &str, bg_command_id: String) -> BashCommand {
    BashCommand {
        action_json: BashCommandAction::Screen {
            screen: true,
            bg_command_id: Some(bg_command_id),
            lines: None,
            diff: true,
        },
        wait_for_seconds: Some(0.0),
        thread_id: thread_id.to_string(),
    }
}

fn wait_for_turn(thread_id: &str, bg_command_id: String) -> BashCommand {
    BashCommand {
        action_json: BashCommandAction::WaitForTurn {
            wait_for_turn: true,
            bg_command_id: Some(bg_command_id),
            recognizer: Some("generic".to_string()),
            quiet_ms: Some(50),
            timeout_seconds: Some(3.5),
            lines: None,
            wait_through_busy: false,
        },
        wait_for_seconds: Some(0.0),
        thread_id: thread_id.to_string(),
    }
}

fn send_text(thread_id: &str, bg_command_id: String, text: &str) -> BashCommand {
    BashCommand {
        action_json: BashCommandAction::SendText {
            send_text: text.to_string(),
            bg_command_id: Some(bg_command_id),
            submit: true,
        },
        wait_for_seconds: Some(0.3),
        thread_id: thread_id.to_string(),
    }
}

fn extract_bg_command_id(response: &str) -> Result<String> {
    response
        .lines()
        .find_map(|line| line.strip_prefix("bg_command_id = "))
        .map(str::to_string)
        .ok_or_else(|| {
            WinxError::CommandExecutionError(format!(
                "background response did not contain bg_command_id: {response}"
            ))
        })
}

fn normalize_volatile(output: &str, workspace: &str, bg_command_ids: &[&str]) -> String {
    let mut normalized = output.replace(workspace, "<WORKSPACE>");
    for id in bg_command_ids {
        normalized = normalized.replace(id, "<BG_ID>");
    }

    normalized
        .lines()
        .map(|line| {
            if line.starts_with("running for = ") && line.ends_with(" seconds") {
                "running for = <SECONDS> seconds".to_string()
            } else if line.len() >= 128 && line.bytes().all(|byte| byte == b'x') {
                "<LONG_X_PAYLOAD>".to_string()
            } else if line.starts_with("<WORKSPACE>/.winx/scratch/bash-output-")
                && std::path::Path::new(line)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
            {
                "<WORKSPACE>/.winx/scratch/<SCRATCH_FILE>".to_string()
            } else {
                let line = normalize_prompt_nonce(line);
                let line = normalize_wait_duration(&line);
                let line = remove_optional_terminal_protocol(&line);
                let line = visible_control_characters(&line);
                normalize_prompt_spacing(&line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_prompt_nonce(line: &str) -> String {
    let Some(nonce_start) = line.find("──➤").map(|index| index + "──➤".len()) else {
        return line.to_string();
    };
    let Some(nonce_len) = line[nonce_start..].find(':') else {
        return line.to_string();
    };
    let nonce_end = nonce_start + nonce_len;
    let nonce = &line[nonce_start..nonce_end];
    if nonce.len() != 16 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return line.to_string();
    }

    format!("{}<PROMPT_NONCE>{}", &line[..nonce_start], &line[nonce_end..])
}

fn normalize_wait_duration(line: &str) -> String {
    let Some(start) = line.find(", waited ") else {
        return line.to_string();
    };
    let value_start = start + ", waited ".len();
    let Some(value_len) = line[value_start..].find("s)") else {
        return line.to_string();
    };
    let value_end = value_start + value_len;
    if line[value_start..value_end].parse::<f64>().is_err() {
        return line.to_string();
    }

    format!("{}<SECONDS>{}", &line[..value_start], &line[value_end..])
}

fn remove_optional_terminal_protocol(line: &str) -> String {
    let mut normalized =
        line.replace("\x1b[?2004l\r", "").replace("\x1b[?2004l", "").replace("\x1b[?2004h", "");

    while let Some(start) = normalized.find("\x1b]0;") {
        let payload_start = start + "\x1b]0;".len();
        let tail = &normalized[payload_start..];
        let terminator = tail
            .find('\x07')
            .map(|offset| (offset, 1))
            .or_else(|| tail.find("\x1b\\").map(|offset| (offset, 2)));
        let Some((offset, terminator_len)) = terminator else { break };
        normalized.replace_range(start..payload_start + offset + terminator_len, "");
    }

    normalized
}

fn visible_control_characters(line: &str) -> String {
    line.chars().fold(String::new(), |mut visible, character| {
        match character {
            '\x1b' => visible.push_str("\\x1b"),
            '\r' => visible.push_str("\\r"),
            '\x07' => visible.push_str("\\x07"),
            _ => visible.push(character),
        }
        visible
    })
}

fn normalize_prompt_spacing(line: &str) -> String {
    if line.starts_with("◉ <WORKSPACE>──➤<PROMPT_NONCE>:") {
        line.trim_end().to_string()
    } else {
        line.to_string()
    }
}

#[test]
fn terminal_protocol_noise_does_not_change_golden_output() {
    let semantic = "dead-final\n◉ <WORKSPACE>──➤<PROMPT_NONCE>:1";
    let variants = [
        "dead-final\n◉ /tmp/winx-golden──➤0123456789abcdef:1 ",
        "\x1b[?2004l\rdead-final\n◉ /tmp/winx-golden──➤0123456789abcdef:1 \x1b[?2004h",
        "\x1b[?2004l\rdead-final\n◉ /tmp/winx-golden──➤0123456789abcdef:1 \x1b]0;bash:/tmp/winx-golden\x07\x1b[?2004h",
    ];

    for variant in variants {
        assert_eq!(normalize_volatile(variant, "/tmp/winx-golden", &[]), semantic);
    }
}

fn assert_golden(actual: &str, expected: &str) {
    assert_eq!(actual, expected.trim_end_matches('\n'));
}

fn error_text(result: Result<String>) -> Result<String> {
    match result {
        Err(error) => Ok(error.to_string()),
        Ok(output) => Err(WinxError::CommandExecutionError(format!(
            "expected BashCommand to fail, got: {output}"
        ))),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn status_is_incremental_and_deduplicated() -> Result<()> {
    let thread_id = "golden-incremental-status";
    let (state, temp_dir) = setup_bash_state(thread_id).await?;
    let workspace = temp_dir.path().canonicalize()?;

    let launch = tools::bash_command::handle_tool_call(
        &state,
        command(
            thread_id,
            "printf 'initial-marker\\n'; sleep 0.8; printf 'late-marker\\n'; sleep 5",
            false,
            0.2,
        ),
    )
    .await?;

    sleep(Duration::from_millis(900)).await;
    let incremental =
        tools::bash_command::handle_tool_call(&state, status(thread_id, None, 0.2)).await?;
    let deduplicated =
        tools::bash_command::handle_tool_call(&state, status(thread_id, None, 0.1)).await?;

    let _ = tools::bash_command::handle_tool_call(&state, ctrl_c(thread_id, None)).await;

    let transcript = format!(
        "=== launch ===\n{launch}\n=== incremental ===\n{incremental}\n=== deduplicated ===\n{deduplicated}"
    );
    let normalized = normalize_volatile(&transcript, workspace.to_string_lossy().as_ref(), &[]);
    assert_golden(&normalized, include_str!("goldens/bash_command/incremental_status.golden"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn finished_background_becomes_repeatable_tombstone() -> Result<()> {
    let thread_id = "golden-repeatable-tombstone";
    let (state, temp_dir) = setup_bash_state(thread_id).await?;
    let workspace = temp_dir.path().canonicalize()?;

    let launch = tools::bash_command::handle_tool_call(
        &state,
        command(thread_id, "printf 'tombstone-final\\n'; false", true, 0.2),
    )
    .await?;
    let bg_id = extract_bg_command_id(&launch)?;

    sleep(Duration::from_millis(600)).await;
    let first =
        tools::bash_command::handle_tool_call(&state, status(thread_id, Some(bg_id.clone()), 0.2))
            .await?;
    let second =
        tools::bash_command::handle_tool_call(&state, status(thread_id, Some(bg_id.clone()), 0.2))
            .await?;

    let transcript = format!(
        "=== launch ===\n{launch}\n=== first read ===\n{first}\n=== second read ===\n{second}"
    );
    let normalized =
        normalize_volatile(&transcript, workspace.to_string_lossy().as_ref(), &[&bg_id]);
    assert_golden(&normalized, include_str!("goldens/bash_command/background_tombstone.golden"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn foreground_reports_and_persists_cwd_and_exit_code() -> Result<()> {
    let thread_id = "golden-cwd-exit-code";
    let (state, temp_dir) = setup_bash_state(thread_id).await?;
    let workspace = temp_dir.path().canonicalize()?;
    let nested = workspace.join("nested");
    std::fs::create_dir(&nested)?;

    let change_and_fail = tools::bash_command::handle_tool_call(
        &state,
        command(thread_id, &format!("cd {}; false", nested.display()), false, 1.0),
    )
    .await?;
    let persisted =
        tools::bash_command::handle_tool_call(&state, command(thread_id, "pwd", false, 1.0))
            .await?;

    let transcript = format!(
        "=== change cwd and fail ===\n{change_and_fail}\n=== next command ===\n{persisted}"
    );
    let normalized = normalize_volatile(&transcript, workspace.to_string_lossy().as_ref(), &[]);
    assert_golden(&normalized, include_str!("goldens/bash_command/cwd_and_exit_code.golden"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn screen_diff_transitions_full_unchanged_changed() -> Result<()> {
    let thread_id = "golden-screen-diff";
    let (state, temp_dir) = setup_bash_state(thread_id).await?;
    let workspace = temp_dir.path().canonicalize()?;

    let launch = tools::bash_command::handle_tool_call(
        &state,
        command(
            thread_id,
            "printf '\\033[2J\\033[Hscreen-alpha\\n'; stty -echo; read -r line; stty echo; printf 'screen-beta:%s\\n' \"$line\"; sleep 5",
            true,
            0.0,
        ),
    )
    .await?;
    let bg_id = extract_bg_command_id(&launch)?;

    let _ =
        tools::bash_command::handle_tool_call(&state, status(thread_id, Some(bg_id.clone()), 0.5))
            .await?;
    let full = tools::bash_command::handle_tool_call(&state, screen_diff(thread_id, bg_id.clone()))
        .await?;
    let unchanged =
        tools::bash_command::handle_tool_call(&state, screen_diff(thread_id, bg_id.clone()))
            .await?;
    let _ = tools::bash_command::handle_tool_call(
        &state,
        send_text(thread_id, bg_id.clone(), "payload"),
    )
    .await?;
    let changed =
        tools::bash_command::handle_tool_call(&state, screen_diff(thread_id, bg_id.clone()))
            .await?;

    let _ =
        tools::bash_command::handle_tool_call(&state, ctrl_c(thread_id, Some(bg_id.clone()))).await;

    let transcript =
        format!("=== full ===\n{full}\n=== unchanged ===\n{unchanged}\n=== changed ===\n{changed}");
    let normalized =
        normalize_volatile(&transcript, workspace.to_string_lossy().as_ref(), &[&bg_id]);
    assert_golden(&normalized, include_str!("goldens/bash_command/screen_diff.golden"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn wait_for_turn_reports_generic_ready() -> Result<()> {
    let thread_id = "golden-wait-for-turn";
    let (state, temp_dir) = setup_bash_state(thread_id).await?;
    let workspace = temp_dir.path().canonicalize()?;

    let launch = tools::bash_command::handle_tool_call(
        &state,
        command(thread_id, "printf '\\033[2J\\033[Hturn-ready\\n'; sleep 5", true, 0.0),
    )
    .await?;
    let bg_id = extract_bg_command_id(&launch)?;
    let turn =
        tools::bash_command::handle_tool_call(&state, wait_for_turn(thread_id, bg_id.clone()))
            .await?;

    let _ =
        tools::bash_command::handle_tool_call(&state, ctrl_c(thread_id, Some(bg_id.clone()))).await;

    let normalized = normalize_volatile(&turn, workspace.to_string_lossy().as_ref(), &[&bg_id]);
    assert_golden(&normalized, include_str!("goldens/bash_command/wait_for_turn.golden"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn large_output_is_truncated_with_scratch_pointer() -> Result<()> {
    let thread_id = "golden-output-truncation";
    let (state, temp_dir) = setup_bash_state(thread_id).await?;
    let workspace = temp_dir.path().canonicalize()?;

    let launch = tools::bash_command::handle_tool_call(
        &state,
        command(thread_id, "head -c 1100000 /dev/zero | tr '\\0' x; printf '\\n'", true, 0.0),
    )
    .await?;
    let bg_id = extract_bg_command_id(&launch)?;

    sleep(Duration::from_millis(800)).await;
    let mut compact_status = String::new();
    let mut completed = false;
    let mut runtime_truncated = false;
    let mut dropped_output_file = None;
    for _ in 0..10 {
        let outcome = EmbeddedShellRuntime
            .run_action_detailed(
                &state,
                status(thread_id, Some(bg_id.clone()), 0.2),
                ShellActionOptions { compact_output: true, ..ShellActionOptions::default() },
            )
            .await?;
        completed = !outcome.result.state.is_running();
        runtime_truncated = outcome.output_truncated;
        dropped_output_file = outcome.dropped_output_file.clone();
        assert!(outcome.result.output.is_empty(), "compact wire path built legacy output");
        compact_status = outcome.compact_output.unwrap_or_default();
        if completed {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    if !completed {
        return Err(WinxError::CommandExecutionError(format!(
            "large-output command did not finish: {compact_status}"
        )));
    }
    assert!(runtime_truncated, "truncation must come from runtime metadata");
    let dropped_output_file = dropped_output_file.ok_or_else(|| {
        WinxError::CommandExecutionError("runtime omitted the spill path".to_string())
    })?;
    assert!(dropped_output_file.is_file(), "{dropped_output_file:?}");
    assert!(
        dropped_output_file.starts_with(workspace.join(".winx/scratch")),
        "{dropped_output_file:?}"
    );

    let final_status =
        tools::bash_command::handle_tool_call(&state, status(thread_id, Some(bg_id.clone()), 0.0))
            .await?;

    let normalized =
        normalize_volatile(&final_status, workspace.to_string_lossy().as_ref(), &[&bg_id]);
    assert_golden(&normalized, include_str!("goldens/bash_command/truncation.golden"));
    assert!(compact_status.contains("Output was truncated"), "{compact_status}");
    assert!(!compact_status.contains("status = process exited"), "{compact_status}");
    assert!(!compact_status.contains("cwd ="), "{compact_status}");

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn error_messages_are_stable() -> Result<()> {
    let thread_id = "golden-error-messages";
    let (state, temp_dir) = setup_bash_state(thread_id).await?;
    let workspace = temp_dir.path().canonicalize()?;

    let idle_status = error_text(
        tools::bash_command::handle_tool_call(&state, status(thread_id, None, 0.0)).await,
    )?;
    let unknown_background = error_text(
        tools::bash_command::handle_tool_call(
            &state,
            status(thread_id, Some("missing-bg-id".to_string()), 0.0),
        )
        .await,
    )?;
    let empty_send_text = error_text(
        tools::bash_command::handle_tool_call(
            &state,
            BashCommand {
                action_json: BashCommandAction::SendText {
                    send_text: String::new(),
                    bg_command_id: None,
                    submit: false,
                },
                wait_for_seconds: Some(0.0),
                thread_id: thread_id.to_string(),
            },
        )
        .await,
    )?;

    let launch = tools::bash_command::handle_tool_call(
        &state,
        command(thread_id, "printf 'dead-final\\n'; false", true, 0.0),
    )
    .await?;
    let bg_id = extract_bg_command_id(&launch)?;
    sleep(Duration::from_millis(600)).await;
    let _ =
        tools::bash_command::handle_tool_call(&state, status(thread_id, Some(bg_id.clone()), 0.2))
            .await?;
    let dead_background = error_text(
        tools::bash_command::handle_tool_call(
            &state,
            send_text(thread_id, bg_id.clone(), "cannot-be-delivered"),
        )
        .await,
    )?;

    let transcript = format!(
        "=== idle status ===\n{idle_status}\n=== unknown background ===\n{unknown_background}\n=== empty send_text ===\n{empty_send_text}\n=== dead background input ===\n{dead_background}"
    );
    let normalized =
        normalize_volatile(&transcript, workspace.to_string_lossy().as_ref(), &[&bg_id]);
    assert_golden(&normalized, include_str!("goldens/bash_command/error_messages.golden"));

    Ok(())
}
