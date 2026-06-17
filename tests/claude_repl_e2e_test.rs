//! End-to-end test of the anchor use case: piloting the real `claude` TUI
//! through winx's new interactive-terminal actions.
//!
//! It launches `claude` in a background PTY, waits for each turn with the
//! `claude` recognizer, answers the "trust this folder?" approval, sends a
//! prompt and reads the response off the stable live screen — the exact REPL
//! loop the feature was built for. It runs the real CLI (auth + quota), so it
//! is `#[ignore]`d; run it manually with:
//!   cargo test --test `claude_repl_e2e_test` -- --ignored --nocapture

// Manual diagnostic e2e harness: println! traces the live turns under
// --nocapture, and expect() turns a missing bg id into an obvious failure.
#![allow(clippy::expect_used, clippy::print_stdout)]

use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Mutex;

use winx_code_agent::errors::Result;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::tools::bash_command::handle_tool_call;
use winx_code_agent::types::{
    BashCommand, BashCommandAction, Initialize, InitializeType, ModeName, SpecialKey,
};

const TID: &str = "tid_claude_repl_e2e";

fn extract_bg_id(out: &str) -> Option<String> {
    out.lines().find_map(|l| l.split_once("bg_command_id = ").map(|(_, id)| id.trim().to_string()))
}

fn cmd(action: BashCommandAction, wait: Option<f32>) -> BashCommand {
    BashCommand { action_json: action, wait_for_seconds: wait, thread_id: TID.to_string() }
}

async fn wait_turn(
    state: &Arc<Mutex<Option<BashState>>>,
    bg: &str,
    timeout: f32,
) -> Result<String> {
    handle_tool_call(
        state,
        cmd(
            BashCommandAction::WaitForTurn {
                wait_for_turn: true,
                bg_command_id: Some(bg.to_string()),
                recognizer: Some("claude".to_string()),
                quiet_ms: Some(800),
                timeout_seconds: Some(timeout),
                lines: None,
            },
            Some(timeout + 5.0),
        ),
    )
    .await
}

async fn press_enter(state: &Arc<Mutex<Option<BashState>>>, bg: &str) -> Result<()> {
    handle_tool_call(
        state,
        cmd(
            BashCommandAction::SendSpecials {
                send_specials: vec![SpecialKey::Enter],
                bg_command_id: Some(bg.to_string()),
                submit: false,
            },
            Some(2.0),
        ),
    )
    .await
    .map(|_| ())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the real claude CLI (auth + quota); manual validation only"]
async fn pilot_claude_repl_through_winx() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let state: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));
    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: temp_dir.path().to_string_lossy().to_string(),
        thread_id: TID.to_string(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };
    winx_code_agent::tools::initialize::handle_tool_call(&state, init).await?;

    // 1. Launch claude in the background (env scrubbed so a recursive launch
    //    doesn't inherit our own Claude Code session vars).
    let launch = cmd(
        BashCommandAction::Command {
            command: "env -u CLAUDECODE -u CLAUDE_CODE_ENTRYPOINT -u CLAUDE_CODE_SSE_PORT claude"
                .to_string(),
            is_background: true,
            allow_multi: false,
        },
        Some(3.0),
    );
    let bg = extract_bg_id(&handle_tool_call(&state, launch).await?)
        .expect("should get a bg_command_id for the claude shell");
    println!("[e2e] launched claude as bg_command_id={bg}");

    // 2. Wait for boot. On a fresh directory claude first asks to trust the
    //    folder — a real approval dialog the recognizer reports as
    //    `awaiting_approval`. Accept the highlighted default (1. Yes) with Enter.
    let boot = wait_turn(&state, &bg, 45.0).await?;
    println!("[e2e] boot turn:\n{boot}\n");
    if boot.contains("awaiting_approval") || boot.contains("trust this folder") {
        println!("[e2e] confirming trust-folder prompt");
        press_enter(&state, &bg).await?;
        let idle = wait_turn(&state, &bg, 30.0).await?;
        println!("[e2e] post-trust turn:\n{idle}\n");
    }

    // 3. Send a prompt and submit it.
    handle_tool_call(
        &state,
        cmd(
            BashCommandAction::SendText {
                send_text: "Responda com exatamente uma palavra, em maiúsculas: PONG".to_string(),
                bg_command_id: Some(bg.clone()),
                submit: true,
            },
            Some(2.0),
        ),
    )
    .await?;

    // 4. Wait for claude to finish answering.
    let answer_turn = wait_turn(&state, &bg, 90.0).await?;
    println!("[e2e] answer turn:\n{answer_turn}\n");

    // 5. Read the stable screen and confirm the answer is there.
    let screen = handle_tool_call(
        &state,
        cmd(
            BashCommandAction::Screen {
                screen: true,
                bg_command_id: Some(bg.clone()),
                lines: None,
                diff: false,
            },
            None,
        ),
    )
    .await?;
    println!("[e2e] final screen:\n{screen}\n");
    assert!(
        screen.contains("PONG") || answer_turn.contains("PONG"),
        "claude's answer (PONG) not found on the live screen"
    );

    // 6. Cleanup: interrupt twice to exit the TUI.
    for _ in 0..2 {
        let _ = handle_tool_call(
            &state,
            cmd(
                BashCommandAction::SendSpecials {
                    send_specials: vec![SpecialKey::CtrlC],
                    bg_command_id: Some(bg.clone()),
                    submit: false,
                },
                Some(1.0),
            ),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    Ok(())
}
