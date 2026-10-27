#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use super::*;
use crate::state::pty::PtyShell;
use crate::state::turn::{recognizer_for, TurnState};
use crate::types::{BashCommand, BashCommandAction, Modes};

async fn ready_state() -> (tempfile::TempDir, BashState) {
    let temp = tempfile::TempDir::new().unwrap();
    let mut state = BashState::new();
    state.current_thread_id = crate::types::normalize_thread_id(&format!(
        "native-guardian-authority-{:016x}",
        rand::random::<u64>()
    ));
    state.cwd = temp.path().to_path_buf();
    state.workspace_root = temp.path().to_path_buf();
    state.mode = Modes::Wcgw;
    let mut pty = PtyShell::new(temp.path(), false).unwrap();
    pty.send_command("stty -echo; printf '\\033[2J\\033[H❯ '; IFS= read -r line; printf '\\nNATIVE_INPUT_WRITTEN\\n'; sleep 5").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        pty.poll_output_nonblocking();
        if recognizer_for("claude").detect(&pty.live_snapshot(0)) == TurnState::AwaitingInput {
            break;
        }
        assert!(Instant::now() < deadline, "fixture did not become input-ready");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    state.pty_shell = Arc::new(Mutex::new(Some(pty)));
    (temp, state)
}

fn params(
    snapshot: BashStateSnapshot,
    thread_id: String,
    action_json: BashCommandAction,
) -> RunActionParams {
    RunActionParams {
        snapshot,
        command: BashCommand { thread_id, wait_for_seconds: Some(0.0), action_json },
        request_key: format!("native-request-{:016x}", rand::random::<u64>()),
        consumer_id: "native-authority-test".to_string(),
        options: crate::runtime::ShellActionOptions::default(),
    }
}

async fn configured_architect(state: BashState) -> Arc<DaemonSession> {
    let mut changed = state.clone();
    changed.mode = Modes::Architect;
    let thread_id = state.current_thread_id.clone();
    let session = Arc::new(DaemonSession::new());
    *session.state.lock().await = Some(state);
    let sessions = Arc::new(Mutex::new(HashMap::from([(thread_id, session.clone())])));
    configure_session(
        &sessions,
        ConfigureSessionParams {
            snapshot: changed.snapshot(),
            transition: ConfigureSessionTransition::ModeChange,
        },
    )
    .await
    .unwrap();
    assert_eq!(session.state.lock().await.as_ref().unwrap().mode, Modes::Architect);
    session
}

#[tokio::test]
async fn native_workflow_stale_adapter_snapshot_cannot_restore_wcgw_authority() {
    let (_temp, state) = ready_state().await;
    // The adapter captured this snapshot before a completed mode transition.
    let stale_wcgw = state.snapshot();
    let thread_id = state.current_thread_id.clone();
    let pty = state.pty_shell.clone();
    let session = configured_architect(state).await;
    let request = params(
        stale_wcgw,
        thread_id,
        BashCommandAction::Prompt {
            bg_command_id: None,
            prompt_id: "stale-permission".to_string(),
            text: Some("must not send".to_string()),
            recognizer: Some("claude".to_string()),
            timeout_seconds: Some(0.0),
        },
    );
    let result = execute_action(&session, request, "guardian-authority-test").await.unwrap();
    assert!(result.error.is_some(), "configured Architect mode must reject a stale wcgw prompt");
    assert_eq!(session.state.lock().await.as_ref().unwrap().mode, Modes::Architect);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut guard = pty.lock().await;
    let pty = guard.as_mut().unwrap();
    pty.poll_output_nonblocking();
    assert!(!pty.output_snapshot().lines().any(|line| line.trim() == "NATIVE_INPUT_WRITTEN"));
}

#[tokio::test]
async fn native_workflow_inspect_preserves_guardian_authority_and_never_creates_missing_state() {
    let (_temp, state) = ready_state().await;
    let stale_wcgw = state.snapshot();
    let thread_id = state.current_thread_id.clone();
    let session = configured_architect(state).await;
    let request = params(
        stale_wcgw.clone(),
        thread_id.clone(),
        BashCommandAction::Inspect {
            bg_command_id: None,
            recognizer: Some("claude".to_string()),
            include_output: Some(false),
        },
    );
    let result = execute_action(&session, request, "guardian-authority-test").await.unwrap();
    assert!(result.error.is_none());
    assert_eq!(session.state.lock().await.as_ref().unwrap().mode, Modes::Architect);
    let missing = Arc::new(DaemonSession::new());
    for action in [
        BashCommandAction::Inspect { bg_command_id: None, recognizer: None, include_output: None },
        BashCommandAction::Prompt {
            bg_command_id: None,
            prompt_id: "missing".to_string(),
            text: None,
            recognizer: None,
            timeout_seconds: Some(0.0),
        },
    ] {
        let result = execute_action(
            &missing,
            params(stale_wcgw.clone(), thread_id.clone(), action),
            "guardian-authority-test",
        )
        .await;
        assert!(matches!(result, Err(WinxError::BashStateNotInitialized)));
        assert!(
            missing.state.lock().await.is_none(),
            "observations never recreate a missing configured guardian state"
        );
    }
}

#[tokio::test]
async fn native_workflow_intervening_legacy_status_cannot_restore_old_prompt_authority() {
    let (_temp, state) = ready_state().await;
    let stale_wcgw = state.snapshot();
    let thread_id = state.current_thread_id.clone();
    let pty = state.pty_shell.clone();
    let session = configured_architect(state).await;
    let status = params(
        stale_wcgw.clone(),
        thread_id.clone(),
        BashCommandAction::StatusCheck {
            status_check: true,
            bg_command_id: None,
            scrollback_lines: None,
            verbose: false,
        },
    );
    let result = execute_action(&session, status, "guardian-authority-test").await.unwrap();
    assert!(result.error.is_none(), "legacy observation continues to work after mode transition");
    assert_eq!(
        session.state.lock().await.as_ref().unwrap().mode,
        Modes::Architect,
        "a stale legacy observation must not roll back configured authority"
    );
    let request = params(
        stale_wcgw,
        thread_id,
        BashCommandAction::Prompt {
            bg_command_id: None,
            prompt_id: "after-legacy".to_string(),
            text: Some("must not send".to_string()),
            recognizer: Some("claude".to_string()),
            timeout_seconds: Some(0.0),
        },
    );
    let result = execute_action(&session, request, "guardian-authority-test").await.unwrap();
    assert!(result.error.is_some());
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut guard = pty.lock().await;
    let pty = guard.as_mut().unwrap();
    pty.poll_output_nonblocking();
    assert!(!pty.output_snapshot().lines().any(|line| line.trim() == "NATIVE_INPUT_WRITTEN"));
}
