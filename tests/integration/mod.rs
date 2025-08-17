// Integration tests for Winx Code Agent
// These tests verify the complete functionality of the system

pub mod mcp_protocol_tests;
pub mod file_operations_tests;
pub mod bash_execution_tests;

// Re-export common test utilities
pub use tempfile::TempDir;
pub use std::sync::Arc;
pub use tokio::sync::Mutex;
pub use winx_code_agent::tools::WinxService;
pub use winx_code_agent::state::bash_state::BashState;
pub use winx_code_agent::types::*;

/// Helper function to create a temporary directory with test files
pub fn create_test_workspace() -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    
    // Create some test files
    std::fs::write(
        temp_dir.path().join("test.txt"),
        "This is a test file\nwith multiple lines\nfor testing purposes"
    ).unwrap();
    
    std::fs::write(
        temp_dir.path().join("script.sh"),
        "#!/bin/bash\necho 'Hello from script'"
    ).unwrap();
    
    // Create a subdirectory with files
    let subdir = temp_dir.path().join("subdir");
    std::fs::create_dir(&subdir).unwrap();
    std::fs::write(
        subdir.join("nested.txt"),
        "This is a nested file"
    ).unwrap();
    
    temp_dir
}

/// Helper function to initialize a service with default settings
pub async fn initialize_test_service(temp_dir: &TempDir) -> WinxService {
    let bash_state = Arc::new(Mutex::new(BashState::new(temp_dir.path().to_path_buf())));
    let service = WinxService::new(bash_state);

    let init_params = InitializeParams {
        folder_to_start: temp_dir.path().to_string_lossy().to_string(),
        mode: Some(Modes::Wcgw),
        over_screen: Some(false),
    };
    
    service.initialize(init_params).await.unwrap();
    service
}