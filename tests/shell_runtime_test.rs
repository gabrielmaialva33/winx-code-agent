use std::sync::Arc;

use tempfile::TempDir;
use tokio::sync::Mutex;

use winx_code_agent::errors::Result;
use winx_code_agent::runtime::EmbeddedShellRuntime;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::tools;
use winx_code_agent::types::{
    BashCommand, BashCommandAction, Initialize, InitializeType, ModeName,
};

#[tokio::test(flavor = "multi_thread")]
async fn embedded_runtime_preserves_bash_command_contract() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let state = Arc::new(Mutex::new(None::<BashState>));
    tools::initialize::handle_tool_call(
        &state,
        Initialize {
            init_type: InitializeType::FirstCall,
            mode_name: ModeName::Wcgw,
            any_workspace_path: temp_dir.path().to_string_lossy().into_owned(),
            thread_id: "embedded-runtime-contract".to_string(),
            code_writer_config: None,
            initial_files_to_read: vec![],
            task_id_to_resume: String::new(),
        },
    )
    .await?;

    let output = tools::bash_command::handle_tool_call_with_runtime(
        &EmbeddedShellRuntime,
        &state,
        BashCommand {
            action_json: BashCommandAction::Command {
                command: "printf 'runtime-contract\\n'".to_string(),
                is_background: false,
                allow_multi: false,
            },
            wait_for_seconds: Some(1.0),
            thread_id: "embedded-runtime-contract".to_string(),
        },
    )
    .await?;

    assert!(output.contains("runtime-contract"));
    assert!(output.contains("status = process exited"));
    Ok(())
}
