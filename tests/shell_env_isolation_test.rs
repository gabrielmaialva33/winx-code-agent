// Agent shells must not inherit the server's own configuration: on 2026-10-27
// a `cargo test` run from a Winx shell inherited WINX_USAGE_LOG and appended
// ~900 synthetic tool calls to the production usage log.
//
// The test sets server variables for the whole process before any runtime
// thread starts, so it must stay the only test in this binary.
#![cfg(unix)]

use std::sync::Arc;

use tempfile::TempDir;
use tokio::sync::Mutex;

use winx_code_agent::config::SERVER_ONLY_ENV;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::tools;
use winx_code_agent::types::{
    BashCommand, BashCommandAction, Initialize, InitializeType, ModeName,
};

const THREAD_ID: &str = "shell-env-isolation";
const CANARY: &str = "WINX_SHELL_ENV_ISOLATION_CANARY";

#[test]
fn agent_shell_does_not_inherit_server_only_environment() -> anyhow::Result<()> {
    let production_log = TempDir::new()?;
    let production_path = production_log.path().join("usage.jsonl");
    std::env::set_var("WINX_USAGE_LOG", &production_path);
    std::env::set_var("WINX_USAGE_LOG_ROTATION", "never");
    std::env::set_var("WINX_USAGE_LOG_KEEP_DAYS", "0");
    std::env::set_var("WINX_HTTP_TOKEN", "server-token-must-not-reach-agent-shells");
    // Ordinary variables keep flowing so the user's toolchain environment works.
    std::env::set_var(CANARY, "inherited");

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let response = runtime.block_on(run_in_agent_shell(&env_probe_command()))?;

    for key in SERVER_ONLY_ENV {
        assert!(
            response.lines().any(|line| line.trim() == format!("{key}=[unset]")),
            "{key} leaked into the agent shell: {response}"
        );
    }
    assert!(
        response.lines().any(|line| line.trim() == format!("{CANARY}=[inherited]")),
        "ordinary environment must still be inherited: {response}"
    );
    assert!(!response.contains("server-token-must-not-reach-agent-shells"), "{response}");
    assert!(!production_path.exists(), "nothing may open the production usage log");
    Ok(())
}

/// One `sh -c` child so the probe reads the exported environment a nested
/// process (such as `cargo test`) would receive, not just shell variables.
fn env_probe_command() -> String {
    let probes = SERVER_ONLY_ENV
        .iter()
        .chain(std::iter::once(&CANARY))
        .map(|key| format!(r#""{key}=[${{{key}-unset}}]""#))
        .collect::<Vec<_>>()
        .join(" ");
    format!("sh -c 'printf \"%s\\n\" {probes}'")
}

async fn run_in_agent_shell(command: &str) -> anyhow::Result<String> {
    let workspace = TempDir::new()?;
    let bash_state: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));
    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: workspace.path().to_string_lossy().to_string(),
        thread_id: THREAD_ID.to_string(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };
    tools::initialize::handle_tool_call(&bash_state, init).await?;

    let command = BashCommand {
        action_json: BashCommandAction::Command {
            command: command.to_string(),
            is_background: false,
            allow_multi: false,
        },
        // Generous cap for cold runners; the call returns once the probe exits.
        wait_for_seconds: Some(10.0),
        thread_id: THREAD_ID.to_string(),
    };
    Ok(tools::bash_command::handle_tool_call(&bash_state, command).await?)
}
