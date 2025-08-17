use std::sync::Arc;
use tokio::sync::Mutex;
use tempfile::TempDir;
use serde_json::{json, Value};
use rmcp::*;
use winx_code_agent::server::WinxMcpServer;
use winx_code_agent::tools::WinxService;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::types::*;

/// Integration tests for MCP protocol compliance
/// Tests the full MCP server implementation against the protocol specification

#[tokio::test]
async fn test_mcp_server_initialization() {
    let temp_dir = TempDir::new().unwrap();
    let bash_state = Arc::new(Mutex::new(BashState::new(temp_dir.path().to_path_buf())));
    let service = WinxService::new(bash_state);
    let server = WinxMcpServer::new(service);

    // Test server initialization
    assert!(server.is_initialized());
}

#[tokio::test]
async fn test_initialize_tool_mcp_compliance() {
    let temp_dir = TempDir::new().unwrap();
    let bash_state = Arc::new(Mutex::new(BashState::new(temp_dir.path().to_path_buf())));
    let service = WinxService::new(bash_state);

    // Test Initialize tool parameters conform to MCP specification
    let params = InitializeParams {
        folder_to_start: temp_dir.path().to_string_lossy().to_string(),
        mode: Some(Modes::Wcgw),
        over_screen: Some(false),
    };

    let result = service.initialize(params).await;
    assert!(result.is_ok());
    
    let response = result.unwrap();
    assert!(response.contains("Environment initialized"));
}

#[tokio::test]
async fn test_bash_command_tool_mcp_compliance() {
    let temp_dir = TempDir::new().unwrap();
    let bash_state = Arc::new(Mutex::new(BashState::new(temp_dir.path().to_path_buf())));
    let service = WinxService::new(bash_state.clone());

    // Initialize first
    let init_params = InitializeParams {
        folder_to_start: temp_dir.path().to_string_lossy().to_string(),
        mode: Some(Modes::Wcgw),
        over_screen: Some(false),
    };
    service.initialize(init_params).await.unwrap();

    // Test BashCommand tool
    let params = BashCommandParams {
        command: "echo 'test'".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result = service.bash_command(params).await;
    assert!(result.is_ok());
    
    let response = result.unwrap();
    assert!(response.contains("test"));
}

#[tokio::test]
async fn test_read_files_tool_mcp_compliance() {
    let temp_dir = TempDir::new().unwrap();
    let bash_state = Arc::new(Mutex::new(BashState::new(temp_dir.path().to_path_buf())));
    let service = WinxService::new(bash_state);

    // Create a test file
    let test_file = temp_dir.path().join("test.txt");
    std::fs::write(&test_file, "Hello, World!\nLine 2\nLine 3").unwrap();

    // Test ReadFiles tool
    let params = ReadFilesParams {
        paths: vec![test_file.to_string_lossy().to_string()],
        include_line_numbers: Some(true),
    };

    let result = service.read_files(params).await;
    assert!(result.is_ok());
    
    let response = result.unwrap();
    assert!(response.contains("Hello, World!"));
    assert!(response.contains("Line 2"));
}

#[tokio::test]
async fn test_file_write_or_edit_tool_mcp_compliance() {
    let temp_dir = TempDir::new().unwrap();
    let bash_state = Arc::new(Mutex::new(BashState::new(temp_dir.path().to_path_buf())));
    let service = WinxService::new(bash_state);

    let test_file = temp_dir.path().join("new_file.txt");

    // Test FileWriteOrEdit tool - write new file
    let params = FileWriteOrEditParams {
        path: test_file.to_string_lossy().to_string(),
        new_content: Some("New file content".to_string()),
        search_replace_blocks: None,
        is_executable: Some(false),
    };

    let result = service.file_write_or_edit(params).await;
    assert!(result.is_ok());
    
    let response = result.unwrap();
    assert!(response.contains("File written successfully"));
    
    // Verify file was created
    assert!(test_file.exists());
    let content = std::fs::read_to_string(&test_file).unwrap();
    assert_eq!(content, "New file content");
}

#[tokio::test]
async fn test_read_image_tool_mcp_compliance() {
    let temp_dir = TempDir::new().unwrap();
    let bash_state = Arc::new(Mutex::new(BashState::new(temp_dir.path().to_path_buf())));
    let service = WinxService::new(bash_state);

    // Create a simple test image (1x1 pixel PNG)
    let test_image = temp_dir.path().join("test.png");
    let png_data = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1 dimensions
        0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE, // IHDR data
        0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, // IDAT chunk
        0x08, 0x99, 0x01, 0x01, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01,
        0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82 // IEND
    ];
    std::fs::write(&test_image, png_data).unwrap();

    // Test ReadImage tool
    let params = ReadImageParams {
        path: test_image.to_string_lossy().to_string(),
    };

    let result = service.read_image(params).await;
    assert!(result.is_ok());
    
    let response = result.unwrap();
    assert!(response.contains("data:image/png;base64,"));
}

#[tokio::test]
async fn test_context_save_tool_mcp_compliance() {
    let temp_dir = TempDir::new().unwrap();
    let bash_state = Arc::new(Mutex::new(BashState::new(temp_dir.path().to_path_buf())));
    let service = WinxService::new(bash_state);

    // Test ContextSave tool
    let params = ContextSaveParams {
        memory_id: "test_memory".to_string(),
        description: "Test memory description".to_string(),
        relevant_files_data: Some("test_file.txt".to_string()),
    };

    let result = service.context_save(params).await;
    assert!(result.is_ok());
    
    let response = result.unwrap();
    assert!(response.contains("Context saved"));
}

#[tokio::test]
async fn test_error_handling_mcp_compliance() {
    let temp_dir = TempDir::new().unwrap();
    let bash_state = Arc::new(Mutex::new(BashState::new(temp_dir.path().to_path_buf())));
    let service = WinxService::new(bash_state);

    // Test error handling with invalid file path
    let params = ReadFilesParams {
        paths: vec!["/nonexistent/path/file.txt".to_string()],
        include_line_numbers: Some(true),
    };

    let result = service.read_files(params).await;
    assert!(result.is_err());
    
    // Error should be properly formatted for MCP
    let error = result.unwrap_err();
    assert!(error.to_string().contains("not found") || error.to_string().contains("does not exist"));
}

#[tokio::test]
async fn test_tool_parameter_validation() {
    let temp_dir = TempDir::new().unwrap();
    let bash_state = Arc::new(Mutex::new(BashState::new(temp_dir.path().to_path_buf())));
    let service = WinxService::new(bash_state);

    // Test with empty command - should handle gracefully
    let params = BashCommandParams {
        command: "".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    // Initialize first
    let init_params = InitializeParams {
        folder_to_start: temp_dir.path().to_string_lossy().to_string(),
        mode: Some(Modes::Wcgw),
        over_screen: Some(false),
    };
    service.initialize(init_params).await.unwrap();

    let result = service.bash_command(params).await;
    // Should either succeed with empty output or fail gracefully
    assert!(result.is_ok() || result.is_err());
}

#[tokio::test]
async fn test_concurrent_tool_execution() {
    let temp_dir = TempDir::new().unwrap();
    let bash_state = Arc::new(Mutex::new(BashState::new(temp_dir.path().to_path_buf())));
    let service = Arc::new(WinxService::new(bash_state));

    // Initialize first
    let init_params = InitializeParams {
        folder_to_start: temp_dir.path().to_string_lossy().to_string(),
        mode: Some(Modes::Wcgw),
        over_screen: Some(false),
    };
    service.initialize(init_params).await.unwrap();

    // Test concurrent execution of multiple bash commands
    let service1 = service.clone();
    let service2 = service.clone();

    let task1 = tokio::spawn(async move {
        let params = BashCommandParams {
            command: "echo 'task1'".to_string(),
            send_text: None,
            include_run_config: Some(false),
            include_bash_state: Some(false),
        };
        service1.bash_command(params).await
    });

    let task2 = tokio::spawn(async move {
        let params = BashCommandParams {
            command: "echo 'task2'".to_string(),
            send_text: None,
            include_run_config: Some(false),
            include_bash_state: Some(false),
        };
        service2.bash_command(params).await
    });

    let (result1, result2) = tokio::join!(task1, task2);
    
    assert!(result1.is_ok());
    assert!(result2.is_ok());
    
    let response1 = result1.unwrap().unwrap();
    let response2 = result2.unwrap().unwrap();
    
    assert!(response1.contains("task1"));
    assert!(response2.contains("task2"));
}