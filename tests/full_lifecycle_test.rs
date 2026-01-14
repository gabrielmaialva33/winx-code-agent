use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio::time::timeout;

use winx_code_agent::errors::Result;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::types::{
    BashCommand, BashCommandAction, ContextSave, FileWriteOrEdit, Initialize, InitializeType,
    ModeName, ReadFiles,
};

const TEST_THREAD_ID: &str = "lifecycle-thread";
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

async fn create_initialized_state(
    temp_dir: &TempDir,
) -> Result<Arc<Mutex<Option<BashState>>>> {
    let bash_state_arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));
    let workspace_path = std::fs::canonicalize(temp_dir.path())
        .expect("Failed to canonicalize temp dir")
        .to_string_lossy()
        .to_string();

    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: workspace_path,
        thread_id: TEST_THREAD_ID.to_string(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };

    winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;
    Ok(bash_state_arc)
}

#[tokio::test(flavor = "multi_thread")]
async fn test_full_lifecycle_workflow() -> Result<()> {
    // Wrap entire test in timeout to prevent hangs
    timeout(TEST_TIMEOUT, async {
        // 1. Setup
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let state = create_initialized_state(&temp_dir).await?;
        let workspace = temp_dir.path().to_path_buf();

        // 2. Bash: Create directory structure
        println!("DEBUG: Running mkdir");
        let mkdir_cmd = BashCommand {
            action_json: BashCommandAction::Command {
                command: "mkdir -p src/data".to_string(),
                is_background: false,
            },
            wait_for_seconds: None,
            thread_id: TEST_THREAD_ID.to_string(),
        };
        let output = winx_code_agent::tools::bash_command::handle_tool_call(&state, mkdir_cmd).await?;
        println!("DEBUG: Mkdir done: {}", output);
        assert!(output.contains("") || output.contains("exit code: 0"), "Mkdir failed");

        // 3. Bash: Verify structure (replaced ListDir)
        println!("DEBUG: Running ls");
        let ls_cmd = BashCommand {
            action_json: BashCommandAction::Command {
                command: "ls -R src".to_string(),
                is_background: false,
            },
            wait_for_seconds: None, // Wait until done
            thread_id: TEST_THREAD_ID.to_string(),
        };
        let ls_output = winx_code_agent::tools::bash_command::handle_tool_call(&state, ls_cmd).await?;
        println!("DEBUG: ls done: {}", ls_output);
        assert!(ls_output.contains("data"), "ls missed 'data' directory");

        // 4. FileWriteOrEdit: Create a data file (JSON)
        println!("DEBUG: Running FileWriteOrEdit (create)");
        let data_file = workspace.join("src/data/config.json");
        let write_cmd = FileWriteOrEdit {
            file_path: data_file.to_string_lossy().to_string(),
            percentage_to_change: 100,
            text_or_search_replace_blocks: r#"{"version": 1, "mode": "test"}"#.to_string(),
            thread_id: TEST_THREAD_ID.to_string(),
        };
        winx_code_agent::tools::file_write_or_edit::handle_tool_call(&state, write_cmd).await?;
        println!("DEBUG: Create done");

        // 5. ReadFiles: Verify content
        println!("DEBUG: Running ReadFiles");
        let read_cmd = ReadFiles {
            file_paths: vec![data_file.to_string_lossy().to_string()],
            start_line_nums: vec![],
            end_line_nums: vec![],
        };
        let read_output = winx_code_agent::tools::read_files::handle_tool_call(&state, read_cmd.clone()).await?;
        println!("DEBUG: ReadFiles done");
        assert!(read_output.contains("\"version\": 1"), "ReadFiles content mismatch");

        // 6. FileWriteOrEdit: Edit the file (SEARCH/REPLACE)
        println!("DEBUG: Running FileWriteOrEdit (edit)");
        let edit_cmd = FileWriteOrEdit {
            file_path: data_file.to_string_lossy().to_string(),
            percentage_to_change: 40,
            text_or_search_replace_blocks: r#"<<<<<<< SEARCH
"mode": "test"
=======
"mode": "production"
>>>>>>> REPLACE"#.to_string(),
            thread_id: TEST_THREAD_ID.to_string(),
        };
        winx_code_agent::tools::file_write_or_edit::handle_tool_call(&state, edit_cmd).await?;
        println!("DEBUG: Edit done");

        let _ = winx_code_agent::tools::read_files::handle_tool_call(&state, read_cmd.clone()).await?;

        // 7. Bash: Execute a command that verifies the file content
        println!("DEBUG: Running grep");
        let grep_cmd = BashCommand {
            action_json: BashCommandAction::Command {
                command: format!("grep 'production' {}", data_file.to_string_lossy()),
                is_background: false,
            },
            wait_for_seconds: None,
            thread_id: TEST_THREAD_ID.to_string(),
        };
        let grep_output = winx_code_agent::tools::bash_command::handle_tool_call(&state, grep_cmd).await?;
        println!("DEBUG: Grep done: {}", grep_output);
        assert!(grep_output.contains("production"), "Grep failed to find edited content");

        // 8. ContextSave: Save state
        println!("DEBUG: Running ContextSave");
        let save_cmd = ContextSave {
            id: "test-save".to_string(),
            project_root_path: workspace.to_string_lossy().to_string(),
            description: "Lifecycle test".to_string(),
            relevant_file_globs: vec!["**/*.json".to_string()],
        };
        let save_output = winx_code_agent::tools::context_save::handle_tool_call(&state, save_cmd).await?;
        println!("DEBUG: ContextSave done");
        assert!(save_output.contains("Context saved"), "Context check failed");

        Ok(())
    }).await.expect("Test timed out")
}
