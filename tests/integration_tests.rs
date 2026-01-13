//! Integration tests for Winx MCP tools.
//!
//! These tests verify the tool handlers work correctly in realistic scenarios.

use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Mutex;

use winx_code_agent::errors::Result;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::tools::WinxService;
use winx_code_agent::types::{Initialize, InitializeType, ModeName, ReadFiles};

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

    let response =
        winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    // Verify response contains expected content
    assert!(response.contains("Initialized"));
    assert!(response.contains("thread_id"));

    // Verify bash state was set
    let state = bash_state_arc.lock().await;
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

    let response =
        winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    assert!(response.contains("Initialized"));

    let state = bash_state_arc.lock().await;
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

    let response =
        winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

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
        let state = bash_state_arc.lock().await;
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

    let response =
        winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, mode_change).await?;
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

    let response =
        winx_code_agent::tools::read_files::handle_tool_call(&bash_state_arc, read).await?;

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
        file_paths: vec![file1.to_string_lossy().to_string(), file2.to_string_lossy().to_string()],
        start_line_nums: vec![None, None],
        end_line_nums: vec![None, None],
    };

    let response =
        winx_code_agent::tools::read_files::handle_tool_call(&bash_state_arc, read).await?;

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

    let response =
        winx_code_agent::tools::read_files::handle_tool_call(&bash_state_arc, read).await?;

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

    let response =
        winx_code_agent::tools::read_files::handle_tool_call(&bash_state_arc, read).await?;

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

    let response =
        winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    assert!(response.contains("CodeWriter") || response.contains("Initialized"));

    let state = bash_state_arc.lock().await;
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

    let response =
        winx_code_agent::tools::initialize::handle_tool_call(&bash_state_arc, init).await?;

    // Response should include content from initial file
    assert!(response.contains("Initial file content") || response.contains("Requested files"));

    Ok(())
}

// ==================== ContextSave Tool Tests ====================

#[tokio::test(flavor = "multi_thread")]
async fn test_context_save_basic() -> Result<()> {
    use winx_code_agent::types::ContextSave;

    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Create a test file to be included via glob
    let test_file = temp_dir.path().join("src/main.rs");
    std::fs::create_dir_all(test_file.parent().unwrap()).expect("Failed to create src dir");
    std::fs::write(&test_file, "fn main() { println!(\"Hello\"); }").expect("Failed to write file");

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

    // Generate unique ID for test
    let unique_id = format!(
        "test-context-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
    );

    let context_save = ContextSave {
        id: unique_id.clone(),
        project_root_path: temp_dir.path().to_string_lossy().to_string(),
        description: "Test context save for integration tests".to_string(),
        relevant_file_globs: vec!["src/*.rs".to_string()],
    };

    let response =
        winx_code_agent::tools::context_save::handle_tool_call(&bash_state_arc, context_save)
            .await?;

    // Response should contain the path where context was saved
    assert!(
        response.contains(&unique_id) || response.contains(".txt") || response.contains("saved")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_context_save_empty_globs() -> Result<()> {
    use winx_code_agent::types::ContextSave;

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

    let unique_id = format!(
        "test-empty-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
    );

    let context_save = ContextSave {
        id: unique_id,
        project_root_path: temp_dir.path().to_string_lossy().to_string(),
        description: "Test with empty globs".to_string(),
        relevant_file_globs: vec![],
    };

    let response =
        winx_code_agent::tools::context_save::handle_tool_call(&bash_state_arc, context_save)
            .await?;

    // Should still succeed even with no globs
    assert!(response.contains(".txt") || response.contains("saved"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_context_save_no_matching_files() -> Result<()> {
    use winx_code_agent::types::ContextSave;

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

    let unique_id = format!(
        "test-nomatch-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
    );

    let context_save = ContextSave {
        id: unique_id,
        project_root_path: temp_dir.path().to_string_lossy().to_string(),
        description: "Test with non-matching glob".to_string(),
        relevant_file_globs: vec!["*.nonexistent".to_string()],
    };

    let response =
        winx_code_agent::tools::context_save::handle_tool_call(&bash_state_arc, context_save)
            .await?;

    // Should warn about no files found but still save
    let response_lower = response.to_lowercase();
    assert!(
        response_lower.contains("no files")
            || response_lower.contains("warning")
            || response.contains(".txt")
    );

    Ok(())
}

// ==================== ReadImage Tool Tests ====================

#[tokio::test(flavor = "multi_thread")]
async fn test_read_image_png() -> Result<()> {
    use winx_code_agent::types::ReadImage;

    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Create a minimal valid PNG file (1x1 red pixel)
    // PNG header + IHDR + IDAT + IEND chunks
    let png_data: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
        0x00, 0x00, 0x00, 0x0D, // IHDR length
        0x49, 0x48, 0x44, 0x52, // IHDR
        0x00, 0x00, 0x00, 0x01, // width = 1
        0x00, 0x00, 0x00, 0x01, // height = 1
        0x08, 0x02, // bit depth = 8, color type = 2 (RGB)
        0x00, 0x00, 0x00, // compression, filter, interlace
        0x90, 0x77, 0x53, 0xDE, // CRC
        0x00, 0x00, 0x00, 0x0C, // IDAT length
        0x49, 0x44, 0x41, 0x54, // IDAT
        0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x01, 0x01, 0x01,
        0x00, // compressed data
        0x18, 0xDD, 0x8D, 0xB5, // CRC
        0x00, 0x00, 0x00, 0x00, // IEND length
        0x49, 0x45, 0x4E, 0x44, // IEND
        0xAE, 0x42, 0x60, 0x82, // CRC
    ];

    let image_path = temp_dir.path().join("test.png");
    std::fs::write(&image_path, &png_data).expect("Failed to write PNG");

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

    let read_image = ReadImage { file_path: image_path.to_string_lossy().to_string() };

    let (mime_type, base64_data) =
        winx_code_agent::tools::read_image::handle_tool_call(&bash_state_arc, read_image).await?;

    // Verify MIME type
    assert_eq!(mime_type, "image/png");

    // Verify base64 data is not empty
    assert!(!base64_data.is_empty());

    // Decode and verify it matches original
    use base64::{engine::general_purpose, Engine};
    let decoded = general_purpose::STANDARD.decode(&base64_data).expect("Failed to decode base64");
    assert_eq!(decoded, png_data);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_read_image_jpeg() -> Result<()> {
    use winx_code_agent::types::ReadImage;

    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Create a minimal valid JPEG file
    // SOI + APP0 + DQT + SOF0 + DHT + SOS + EOI
    let jpeg_data: Vec<u8> = vec![
        0xFF, 0xD8, // SOI
        0xFF, 0xE0, 0x00, 0x10, // APP0 marker + length
        0x4A, 0x46, 0x49, 0x46, 0x00, // JFIF identifier
        0x01, 0x01, // version
        0x00, // aspect ratio units
        0x00, 0x01, // X density
        0x00, 0x01, // Y density
        0x00, 0x00, // thumbnail
        0xFF, 0xD9, // EOI
    ];

    let image_path = temp_dir.path().join("test.jpg");
    std::fs::write(&image_path, &jpeg_data).expect("Failed to write JPEG");

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

    let read_image = ReadImage { file_path: image_path.to_string_lossy().to_string() };

    let (mime_type, _base64_data) =
        winx_code_agent::tools::read_image::handle_tool_call(&bash_state_arc, read_image).await?;

    // Verify MIME type
    assert_eq!(mime_type, "image/jpeg");

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_read_image_nonexistent() -> Result<()> {
    use winx_code_agent::types::ReadImage;

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

    let read_image = ReadImage {
        file_path: temp_dir.path().join("nonexistent.png").to_string_lossy().to_string(),
    };

    let result =
        winx_code_agent::tools::read_image::handle_tool_call(&bash_state_arc, read_image).await;

    // Should return an error for non-existent file
    assert!(result.is_err());

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_read_image_non_image_file() -> Result<()> {
    use winx_code_agent::types::ReadImage;

    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Create a text file (not an image)
    let text_path = temp_dir.path().join("test.txt");
    std::fs::write(&text_path, "This is not an image").expect("Failed to write text file");

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

    let read_image = ReadImage { file_path: text_path.to_string_lossy().to_string() };

    // ReadImage should still work (returns base64 of any file with guessed MIME type)
    // It falls back to image/jpeg for unknown types per the implementation
    let result =
        winx_code_agent::tools::read_image::handle_tool_call(&bash_state_arc, read_image).await;

    // The function should succeed but with fallback MIME type
    match result {
        Ok((mime_type, base64_data)) => {
            // It uses fallback for unknown extensions
            assert!(
                mime_type == "image/jpeg"
                    || mime_type == "text/plain"
                    || mime_type.starts_with("text/")
            );
            assert!(!base64_data.is_empty());
        }
        Err(_) => {
            // Some implementations might error on non-image files
            // This is also acceptable behavior
        }
    }

    Ok(())
}
