//! Integration tests for Winx MCP tools.
//!
//! These tests verify the tool handlers work correctly in realistic scenarios.

use std::sync::{Arc, Mutex};
use tempfile::TempDir;

use winx_code_agent::errors::Result;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::tools::WinxService;
use winx_code_agent::types::{
    Initialize, InitializeType, ModeName, ReadFiles,
};

// ==================== WinxService Tests ====================

#[test]
fn test_winx_service_creation() {
    let service = WinxService::new();
    assert!(!service.version().is_empty());
    assert!(service.uptime().as_secs() < 1);
}

#[test]
fn test_winx_service_default() {
    let service = WinxService::default();
    assert!(!service.version().is_empty());
}

// ==================== Initialize Tool Tests ====================

#[tokio::test]
async fn test_initialize_first_call_wcgw_mode() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));

    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: temp_dir.path().to_string_lossy().to_string(),
        thread_id: String::new(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };

    let response = winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    // Verify response contains expected content
    assert!(response.contains("Initialized"));
    assert!(response.contains("thread_id"));

    // Verify bash state was set
    let state = bash_state_arc.lock().expect("Lock failed");
    assert!(state.is_some());

    let bash_state = state.as_ref().expect("BashState should be Some");
    assert!(bash_state.initialized);
    assert!(!bash_state.current_thread_id.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_initialize_architect_mode() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));

    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Architect,
        any_workspace_path: temp_dir.path().to_string_lossy().to_string(),
        thread_id: String::new(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };

    let response = winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    assert!(response.contains("Initialized"));

    let state = bash_state_arc.lock().expect("Lock failed");
    let bash_state = state.as_ref().expect("BashState should be Some");
    assert!(bash_state.initialized);

    Ok(())
}

#[tokio::test]
async fn test_initialize_with_file_path() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "test content").expect("Failed to write file");

    let bash_state_arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));

    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: file_path.to_string_lossy().to_string(),
        thread_id: String::new(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };

    let response = winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    // When path is a file, should use parent directory
    assert!(response.contains("parent directory") || response.contains("Initialized"));

    Ok(())
}

#[tokio::test]
async fn test_initialize_mode_change() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));

    // First call to initialize
    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: temp_dir.path().to_string_lossy().to_string(),
        thread_id: String::new(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };
    winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    // Get the thread_id
    let thread_id = {
        let state = bash_state_arc.lock().expect("Lock failed");
        state.as_ref().expect("BashState").current_thread_id.clone()
    };

    // Mode change
    let mode_change = Initialize {
        init_type: InitializeType::UserAskedModeChange,
        mode_name: ModeName::Architect,
        any_workspace_path: temp_dir.path().to_string_lossy().to_string(),
        thread_id,
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };

    let response = winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, mode_change).await?;
    assert!(response.contains("Changed mode"));

    Ok(())
}

// ==================== ReadFiles Tool Tests ====================

#[tokio::test(flavor = "multi_thread")]
async fn test_read_files_single_file() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let file_path = temp_dir.path().join("test.rs");
    std::fs::write(&file_path, "fn main() {\n    println!(\"Hello\");\n}\n")
        .expect("Failed to write file");

    // Initialize bash state first
    let bash_state_arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));
    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: temp_dir.path().to_string_lossy().to_string(),
        thread_id: String::new(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };
    winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    let read = ReadFiles {
        file_paths: vec![file_path.to_string_lossy().to_string()],
        start_line_nums: vec![None],
        end_line_nums: vec![None],
    };

    let response = winx_code_agent::tools::read_files::handle_tool_call(&bash_state_arc, read).await?;

    assert!(response.contains("fn main()"));
    assert!(response.contains("println!"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_read_files_multiple_files() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    let file1 = temp_dir.path().join("file1.txt");
    let file2 = temp_dir.path().join("file2.txt");
    std::fs::write(&file1, "Content of file 1").expect("Failed to write file1");
    std::fs::write(&file2, "Content of file 2").expect("Failed to write file2");

    // Initialize bash state
    let bash_state_arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));
    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: temp_dir.path().to_string_lossy().to_string(),
        thread_id: String::new(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };
    winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    let read = ReadFiles {
        file_paths: vec![
            file1.to_string_lossy().to_string(),
            file2.to_string_lossy().to_string(),
        ],
        start_line_nums: vec![None, None],
        end_line_nums: vec![None, None],
    };

    let response = winx_code_agent::tools::read_files::handle_tool_call(&bash_state_arc, read).await?;

    assert!(response.contains("Content of file 1"));
    assert!(response.contains("Content of file 2"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_read_files_with_line_range() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let file_path = temp_dir.path().join("lines.txt");
    std::fs::write(&file_path, "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\n")
        .expect("Failed to write file");

    // Initialize bash state
    let bash_state_arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));
    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: temp_dir.path().to_string_lossy().to_string(),
        thread_id: String::new(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };
    winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    // Test with explicit line range
    let read = ReadFiles {
        file_paths: vec![file_path.to_string_lossy().to_string()],
        start_line_nums: vec![Some(2)],
        end_line_nums: vec![Some(4)],
    };

    let response = winx_code_agent::tools::read_files::handle_tool_call(&bash_state_arc, read).await?;

    assert!(response.contains("Line 2"));
    assert!(response.contains("Line 3"));
    assert!(response.contains("Line 4"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_read_files_nonexistent() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Initialize bash state
    let bash_state_arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));
    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: temp_dir.path().to_string_lossy().to_string(),
        thread_id: String::new(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };
    winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    let read = ReadFiles {
        file_paths: vec![temp_dir.path().join("nonexistent.txt").to_string_lossy().to_string()],
        start_line_nums: vec![None],
        end_line_nums: vec![None],
    };

    let response = winx_code_agent::tools::read_files::handle_tool_call(&bash_state_arc, read).await?;

    // Should contain some indication of failure - could be error message or empty result
    // Different error formats: "Error", "not found", "No such file", "does not exist", "failed"
    let has_error_indication = response.to_lowercase().contains("error")
        || response.to_lowercase().contains("not found")
        || response.to_lowercase().contains("no such file")
        || response.to_lowercase().contains("does not exist")
        || response.to_lowercase().contains("failed")
        || response.is_empty();

    assert!(has_error_indication, "Expected error indication in response: {}", response);

    Ok(())
}

// ==================== BashState Tests ====================

#[test]
fn test_bash_state_creation() {
    let state = BashState::new();
    assert!(!state.initialized);
    // Note: thread_id may be auto-generated, so we just check it exists
    let _ = &state.current_thread_id;
}

#[test]
fn test_bash_state_with_thread_id() {
    let state = BashState::new_with_thread_id(Some("test-thread-123"));
    // Thread ID is set, but may or may not be initialized depending on disk state
    // Just verify it doesn't panic
    let _ = state.current_thread_id;
}

#[test]
fn test_generate_thread_id() {
    let id1 = winx_code_agent::state::bash_state::generate_thread_id();
    let id2 = winx_code_agent::state::bash_state::generate_thread_id();

    // IDs should be unique
    assert_ne!(id1, id2);

    // IDs should have reasonable format (hex string)
    assert!(!id1.is_empty());
    assert!(!id2.is_empty());
}

// ==================== Mode Configuration Tests ====================

#[tokio::test]
async fn test_code_writer_mode() -> Result<()> {
    use winx_code_agent::types::{AllowedCommands, AllowedGlobs, CodeWriterConfig};

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));

    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::CodeWriter,
        any_workspace_path: temp_dir.path().to_string_lossy().to_string(),
        thread_id: String::new(),
        code_writer_config: Some(CodeWriterConfig {
            allowed_globs: AllowedGlobs::List(vec!["*.rs".to_string(), "*.toml".to_string()]),
            allowed_commands: AllowedCommands::List(vec!["cargo".to_string(), "rustc".to_string()]),
        }),
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };

    let response = winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    assert!(response.contains("CodeWriter") || response.contains("Initialized"));

    let state = bash_state_arc.lock().expect("Lock failed");
    let bash_state = state.as_ref().expect("BashState should be Some");
    assert!(bash_state.initialized);

    Ok(())
}

// ==================== Initial Files Tests ====================

#[tokio::test]
async fn test_initialize_with_initial_files() -> Result<()> {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let file_path = temp_dir.path().join("initial.rs");
    std::fs::write(&file_path, "// Initial file content\nfn init() {}\n")
        .expect("Failed to write file");

    let bash_state_arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));

    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: temp_dir.path().to_string_lossy().to_string(),
        thread_id: String::new(),
        code_writer_config: None,
        initial_files_to_read: vec![file_path.to_string_lossy().to_string()],
        task_id_to_resume: String::new(),
    };

    let response = winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    // Response should include content from initial file
    assert!(response.contains("Initial file content") || response.contains("Requested files"));

    Ok(())
}
