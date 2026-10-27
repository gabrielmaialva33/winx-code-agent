#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use super::*;
use crate::state::live_terminal::ScreenUpdate;
use std::sync::Arc;
use tokio::sync::Mutex;

async fn interactive(working: bool) -> (tempfile::TempDir, BashState, SharedPtyShell) {
    let temp = tempfile::TempDir::new().unwrap();
    let mut state = BashState::new();
    state.mode = Modes::Wcgw;
    state.cwd = temp.path().to_path_buf();
    let mut shell = PtyShell::new(temp.path(), false).unwrap();
    let work = if working {
        "if test \"$line\" != silent; then printf '\\033[2J\\033[Hesc to interrupt'; sleep 0.12; fi;"
    } else {
        ""
    };
    shell.send_command(&format!(
        "stty -echo; printf '\\033[2J\\033[H❯ '; n=0; while IFS= read -r line; do n=$((n+1)); if test \"$line\" = enable-echo; then stty echo; fi; {work} printf '\\033[2J\\033[Hreceived:%s:%s\\n❯ ' \"$n\" \"$line\"; done"
    )).unwrap();
    let shared = Arc::new(Mutex::new(Some(shell)));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let observed = execute_inspect(
            &state,
            Some(shared.clone()),
            None,
            Some("claude"),
            true,
            &ShellActionOptions::default(),
        )
        .await
        .unwrap();
        if observed.workflow.unwrap().explanation.state == TurnState::AwaitingInput {
            break;
        }
        assert!(Instant::now() < deadline, "interactive test command never became ready");
        tokio::time::sleep(POLL).await;
    }
    (temp, state, shared)
}

async fn prompt(
    state: &BashState,
    shell: &SharedPtyShell,
    id: &str,
    text: Option<&str>,
    timeout: f32,
) -> Result<BashCommandRuntimeResult> {
    execute_prompt(
        state,
        Some(shell.clone()),
        None,
        id,
        text,
        Some("claude"),
        Some(timeout),
        &ShellActionOptions::default(),
    )
    .await
}

#[tokio::test]
async fn native_workflow_stale_idle_and_echo_never_satisfy_activity() {
    let (_temp, state, shell) = interactive(false).await;
    let observation =
        prompt(&state, &shell, "stale", Some("hello"), 2.7).await.unwrap().workflow.unwrap();
    let receipt = observation.prompt_receipt.unwrap();
    assert_eq!(receipt.delivery, PromptDelivery::Delivered);
    assert_eq!(receipt.outcome, PromptOutcome::TimedOut);
    assert!(!receipt.activity_observed);
    assert!(
        observation.activity_seq > receipt.baseline_activity_seq,
        "real PTY echo/output was produced"
    );
}

#[tokio::test]
async fn native_workflow_busy_then_ready_is_observed_and_prompt_id_never_resends() {
    let (_temp, state, shell) = interactive(true).await;
    let first = prompt(&state, &shell, "once", Some("hello"), 2.0).await.unwrap().workflow.unwrap();
    let receipt = first.prompt_receipt.unwrap();
    assert!(receipt.activity_observed);
    assert_eq!(receipt.outcome, PromptOutcome::AwaitingInput);
    assert_eq!(receipt.delivery, PromptDelivery::Delivered);
    let duplicate =
        prompt(&state, &shell, "once", Some("hello"), 0.0).await.unwrap().workflow.unwrap();
    assert_eq!(duplicate.prompt_receipt.as_ref().unwrap().outcome, PromptOutcome::AwaitingInput);
    assert!(duplicate.output.contains("received:1"));
    assert!(!duplicate.output.contains("received:2"));
    assert!(prompt(&state, &shell, "once", Some("different"), 0.0).await.is_err());
    let observed = prompt(&state, &shell, "once", None, 0.0).await.unwrap().workflow.unwrap();
    assert!(!observed.output.contains("received:2"));
    assert!(prompt(&state, &shell, "missing", None, 0.0).await.is_err());
}

#[tokio::test]
async fn native_workflow_work_between_calls_is_not_lost() {
    let (_temp, state, shell) = interactive(true).await;
    prompt(&state, &shell, "between", Some("hello"), 0.0).await.unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    let observed = prompt(&state, &shell, "between", None, 1.0).await.unwrap().workflow.unwrap();
    let receipt = observed.prompt_receipt.unwrap();
    assert!(receipt.activity_observed, "reader must retain the busy frame seen between calls");
    assert_eq!(receipt.outcome, PromptOutcome::AwaitingInput);
}

#[tokio::test]
async fn native_workflow_cancellation_preserves_uncertain_receipt_without_resend() {
    let (_temp, state, shell) = interactive(false).await;
    assert!(tokio::time::timeout(
        Duration::from_millis(10),
        prompt(&state, &shell, "cancelled", Some("hello"), 2.0)
    )
    .await
    .is_err());
    let replay =
        prompt(&state, &shell, "cancelled", Some("hello"), 0.0).await.unwrap().workflow.unwrap();
    assert_eq!(replay.prompt_receipt.unwrap().delivery, PromptDelivery::Uncertain);
    assert!(!replay.output.contains("received:1:"));
    shell.lock().await.as_mut().unwrap().send_special_key("Enter").unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let observed = execute_inspect(
        &state,
        Some(shell.clone()),
        None,
        Some("claude"),
        true,
        &ShellActionOptions::default(),
    )
    .await
    .unwrap()
    .workflow
    .unwrap();
    assert!(observed.output.contains("received:1:hello"));
    assert!(!observed.output.contains("received:1:hellohello"));
}

#[tokio::test]
async fn native_workflow_an_old_receipt_does_not_adopt_a_new_prompts_busy_frame() {
    let (_temp, state, shell) = interactive(true).await;
    prompt(&state, &shell, "old", Some("silent"), 0.0).await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    prompt(&state, &shell, "new", Some("work"), 0.0).await.unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;
    let old = prompt(&state, &shell, "old", None, 0.0).await.unwrap().workflow.unwrap();
    assert!(!old.prompt_receipt.unwrap().activity_observed);
}

#[tokio::test]
async fn native_workflow_typed_echo_markers_do_not_count_as_work() {
    let (_temp, state, shell) = interactive(false).await;
    prompt(&state, &shell, "enable", Some("enable-echo"), 0.0).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let echoed = prompt(&state, &shell, "echo", Some("esc to interrupt"), 0.3)
        .await
        .unwrap()
        .workflow
        .unwrap();
    let receipt = echoed.prompt_receipt.unwrap();
    assert_eq!(receipt.outcome, PromptOutcome::TimedOut);
    assert!(!receipt.activity_observed, "typed busy text is terminal echo, not observed work");
}

#[tokio::test]
async fn native_workflow_inspect_preserves_output_and_screen_delivery() {
    let (_temp, state, shell) = interactive(false).await;
    let mut guard = shell.lock().await;
    let pty = guard.as_mut().unwrap();
    pty.mark_output_delivered("original cursor");
    pty.last_returned_hash = Some(123);
    let initial_screen = pty.live_snapshot(0);
    let _ = pty.live_snapshot_diff(0, 10);
    drop(guard);
    let observed = execute_inspect(
        &state,
        Some(shell.clone()),
        None,
        Some("claude"),
        true,
        &ShellActionOptions::default(),
    )
    .await
    .unwrap()
    .workflow
    .unwrap();
    assert_eq!(observed.screen, initial_screen);
    let mut guard = shell.lock().await;
    let pty = guard.as_mut().unwrap();
    assert_eq!(pty.delivered_output(), "original cursor");
    assert_eq!(pty.last_returned_hash, Some(123));
    assert!(matches!(pty.live_snapshot_diff(0, 10), ScreenUpdate::Unchanged));
}

#[tokio::test]
async fn native_workflow_output_omission_is_explicit_and_does_not_remove_retained_output() {
    let (_temp, state, shell) = interactive(false).await;
    let omitted = execute_inspect(
        &state,
        Some(shell.clone()),
        None,
        Some("claude"),
        false,
        &ShellActionOptions::default(),
    )
    .await
    .unwrap()
    .workflow
    .unwrap();
    assert!(!omitted.output_included);
    assert_eq!(omitted.output, "");
    let included = execute_inspect(
        &state,
        Some(shell.clone()),
        None,
        Some("claude"),
        true,
        &ShellActionOptions::default(),
    )
    .await
    .unwrap()
    .workflow
    .unwrap();
    assert!(included.output_included);
    assert_ne!(included.output, "");
}

#[tokio::test]
async fn native_workflow_receipt_capacity_preserves_earliest_id() {
    let (_temp, state, shell) = interactive(false).await;
    for n in 0..MAX_PROMPT_RECEIPTS {
        let id = if n == 0 { "first".to_string() } else { format!("filled-{n}") };
        prompt(&state, &shell, &id, Some("hello"), 0.0).await.unwrap();
        // Wait for the fixture's actual redraw before the next submission. A
        // transient empty screen is correctly rejected by the production guard.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let observed = execute_inspect(
                &state,
                Some(shell.clone()),
                None,
                Some("claude"),
                false,
                &ShellActionOptions::default(),
            )
            .await
            .unwrap()
            .workflow
            .unwrap();
            if observed.explanation.state == TurnState::AwaitingInput
                && observed.screen.join("\n").contains(&format!("received:{}:hello", n + 1))
            {
                break;
            }
            assert!(Instant::now() < deadline, "fixture did not acknowledge prompt {}", n + 1);
            tokio::time::sleep(POLL).await;
        }
    }
    assert!(prompt(&state, &shell, "overflow", Some("no write"), 0.0).await.is_err());
    let replay =
        prompt(&state, &shell, "first", Some("hello"), 0.0).await.unwrap().workflow.unwrap();
    assert_eq!(replay.prompt_receipt.unwrap().prompt_id, "first");
    assert!(replay.output.contains("received:64"));
    assert!(!replay.output.contains("received:65"));
    assert_eq!(shell.lock().await.as_ref().unwrap().prompt_receipts.len(), MAX_PROMPT_RECEIPTS);
}

#[tokio::test]
async fn native_workflow_rejects_replaced_generation_without_reading_replacement() {
    let (_temp, state, shell) = interactive(false).await;
    let original_generation = shell.lock().await.as_ref().unwrap().command_generation();
    shell.lock().await.as_mut().unwrap().send_interrupt().unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    shell
        .lock()
        .await
        .as_mut()
        .unwrap()
        .send_command("printf '\\033[2J\\033[Hreplacement'; sleep 5")
        .unwrap();
    let options = ShellActionOptions {
        expected_generation: Some(original_generation),
        ..ShellActionOptions::default()
    };
    let error = execute_inspect(&state, Some(shell.clone()), None, Some("claude"), true, &options)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("replaced command generation"));
}

#[tokio::test]
async fn native_workflow_wait_stays_bound_to_original_generation() {
    let (_temp, state, shell) = interactive(false).await;
    let prompt_future = prompt(&state, &shell, "waiting", Some("hello"), 2.0);
    let replace = async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let mut guard = shell.lock().await;
        let pty = guard.as_mut().unwrap();
        pty.send_interrupt().unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        pty.poll_output_nonblocking();
        pty.send_command("printf '\\033[2J\\033[Hreplacement'; sleep 5").unwrap();
    };
    let (result, ()) = tokio::join!(prompt_future, replace);
    assert!(result.unwrap_err().to_string().contains("generation changed"));
}

#[tokio::test]
async fn native_workflow_bounds_and_mode_are_checked_before_writes() {
    let (_temp, mut state, shell) = interactive(false).await;
    assert!(prompt(&state, &shell, "", Some("hello"), 0.0).await.is_err());
    assert!(prompt(&state, &shell, &"x".repeat(129), Some("hello"), 0.0).await.is_err());
    assert!(prompt(&state, &shell, "nul\0id", Some("hello"), 0.0).await.is_err());
    assert!(prompt(&state, &shell, "nul-text", Some("hello\0world"), 0.0).await.is_err());
    assert!(prompt(&state, &shell, "large", Some(&"x".repeat(MAX_PROMPT_BYTES + 1)), 0.0)
        .await
        .is_err());
    assert!(prompt(&state, &shell, "long-wait", Some("hello"), 30.1).await.is_err());
    assert!(prompt(&state, &shell, "nan", Some("hello"), f32::NAN).await.is_err());
    prompt(&state, &shell, "before-mode", Some("hello"), 0.0).await.unwrap();
    state.mode = Modes::Architect;
    assert!(prompt(&state, &shell, "restricted", Some("hello"), 0.0).await.is_err());
    assert!(prompt(&state, &shell, "before-mode", None, 0.0).await.is_ok());
}

#[tokio::test]
async fn native_workflow_reaped_background_preserves_identity_output_and_receipts() {
    let (_temp, state, original) = interactive(false).await;
    let pty = original.lock().await.take().unwrap();
    let mut manager = crate::tools::background_shell::BackgroundShellManager::new();
    let id = manager.register_shell("workflow-owner", pty).unwrap();
    let shared = manager.get_shell("workflow-owner", &id).unwrap();
    let generation = shared.lock().await.as_ref().unwrap().command_generation();
    prompt(&state, &shared, "retained", Some("hello"), 0.0).await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    shared.lock().await.as_mut().unwrap().send_eof().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if shared.lock().await.as_mut().unwrap().poll_output_nonblocking() {
            break;
        }
        assert!(Instant::now() < deadline, "test command did not exit after EOF");
        tokio::time::sleep(POLL).await;
    }
    drop(shared);
    manager.prune_finished_shells();
    let tombstone = manager.peek_tombstone("workflow-owner", &id).unwrap();
    let inspect = crate::types::BashCommandAction::Inspect {
        bg_command_id: Some(id.clone()),
        recognizer: Some("claude".to_string()),
        include_output: Some(true),
    };
    let options = ShellActionOptions {
        expected_generation: Some(generation),
        ..ShellActionOptions::default()
    };
    validate_tombstone(&tombstone, &options).unwrap();
    let observed = inspect_tombstone(&state, &id, &tombstone, &inspect, false).unwrap();
    assert_eq!(observed.command_generation, Some(generation));
    assert!(!observed.result.state.is_running());
    assert!(observed.workflow.unwrap().output.contains("received:1:hello"));
    let existing = crate::types::BashCommandAction::Prompt {
        bg_command_id: Some(id.clone()),
        prompt_id: "retained".to_string(),
        text: None,
        recognizer: Some("claude".to_string()),
        timeout_seconds: Some(0.0),
    };
    let receipt = inspect_tombstone(&state, &id, &tombstone, &existing, false)
        .unwrap()
        .workflow
        .unwrap()
        .prompt_receipt
        .unwrap();
    assert_eq!(receipt.prompt_id, "retained");
    assert_eq!(receipt.delivery, PromptDelivery::Delivered);
    assert_eq!(receipt.outcome, PromptOutcome::Exited);
    assert!(manager.peek_tombstone("workflow-owner", &id).is_some());
    let wrong = ShellActionOptions {
        expected_generation: Some(generation + 1),
        ..ShellActionOptions::default()
    };
    assert!(validate_tombstone(&tombstone, &wrong).is_err());
}

#[tokio::test]
async fn native_workflow_submitted_text_echo_markers_do_not_count_as_work() {
    // ECHO stays disabled: the fixture displays the line only after read accepts Enter.
    for text in ["esc to interrupt", "would you like"] {
        let (_temp, state, shell) = interactive(false).await;
        let observed = prompt(&state, &shell, "submitted-echo", Some(text), 0.4)
            .await
            .unwrap()
            .workflow
            .unwrap();
        let receipt = observed.prompt_receipt.unwrap();
        assert_eq!(receipt.delivery, PromptDelivery::Delivered);
        assert!(observed.output.contains(&format!("received:1:{text}")));
        assert!(observed.activity_seq > receipt.baseline_activity_seq);
        assert!(
            !receipt.activity_observed,
            "submitted text echoed only after Enter is not observed work: {text}"
        );
        assert_eq!(receipt.outcome, PromptOutcome::TimedOut);
    }
}

#[tokio::test]
async fn native_workflow_embedded_runtime_rejects_a_foreign_guardian_before_writing() {
    use crate::runtime::{EmbeddedShellRuntime, ShellExecutionToken, ShellRuntime};
    use crate::types::{BashCommand, BashCommandAction};

    let (_temp, mut state, shell) = interactive(false).await;
    let generation = shell.lock().await.as_ref().unwrap().command_generation();
    let thread_id = crate::types::normalize_thread_id(&format!(
        "foreign-guardian-{:016x}",
        rand::random::<u64>()
    ));
    state.current_thread_id = thread_id.clone();
    state.pty_shell = shell.clone();
    let slot = Arc::new(Mutex::new(Some(state)));
    for options in [
        ShellActionOptions {
            expected_execution: Some(ShellExecutionToken {
                guardian_epoch: "previous-daemon-guardian".to_string(),
                session_epoch: "previous-daemon-session".to_string(),
                generation,
            }),
            require_generation_binding: true,
            ..ShellActionOptions::default()
        },
        ShellActionOptions {
            expected_guardian_epoch: Some("previous-daemon-guardian".to_string()),
            expected_generation: Some(generation),
            require_generation_binding: true,
            ..ShellActionOptions::default()
        },
    ] {
        let result = EmbeddedShellRuntime
            .run_action_detailed(
                &slot,
                BashCommand {
                    thread_id: thread_id.clone(),
                    wait_for_seconds: Some(0.0),
                    action_json: BashCommandAction::Prompt {
                        bg_command_id: None,
                        prompt_id: "foreign-guardian".to_string(),
                        text: Some("must not send".to_string()),
                        recognizer: Some("claude".to_string()),
                        timeout_seconds: Some(0.0),
                    },
                },
                options,
            )
            .await;
        assert!(
            result.is_err(),
            "a coincident generation cannot authorize a foreign runtime token"
        );
        assert!(shell.lock().await.as_ref().unwrap().prompt_receipts.is_empty());
    }
}
