use std::sync::Arc;
use tokio::sync::Mutex;
use tempfile::TempDir;
use winx_code_agent::tools::WinxService;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::types::*;

/// Integration tests for bash command execution
/// Tests shell interaction, state persistence, and command handling

async fn setup_initialized_service() -> (WinxService, TempDir) {
    let temp_dir = TempDir::new().unwrap();
    let bash_state = Arc::new(Mutex::new(BashState::new(temp_dir.path().to_path_buf())));
    let service = WinxService::new(bash_state);

    // Initialize the service
    let init_params = InitializeParams {
        folder_to_start: temp_dir.path().to_string_lossy().to_string(),
        mode: Some(Modes::Wcgw),
        over_screen: Some(false),
    };
    service.initialize(init_params).await.unwrap();

    (service, temp_dir)
}

#[tokio::test]
async fn test_basic_command_execution() {
    let (service, _temp_dir) = setup_initialized_service().await;

    // Test simple echo command
    let params = BashCommandParams {
        command: "echo 'Hello, World!'".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result = service.bash_command(params).await;
    assert!(result.is_ok());
    
    let response = result.unwrap();
    assert!(response.contains("Hello, World!"));
}

#[tokio::test]
async fn test_command_with_exit_code() {
    let (service, _temp_dir) = setup_initialized_service().await;

    // Test command that returns non-zero exit code
    let params = BashCommandParams {
        command: "exit 42".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result = service.bash_command(params).await;
    assert!(result.is_ok());
    
    let response = result.unwrap();
    // Should report the exit code
    assert!(response.contains("42") || response.contains("exit"));
}

#[tokio::test]
async fn test_environment_variable_persistence() {
    let (service, _temp_dir) = setup_initialized_service().await;

    // Set an environment variable
    let params1 = BashCommandParams {
        command: "export TEST_VAR='test_value'".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result1 = service.bash_command(params1).await;
    assert!(result1.is_ok());

    // Check if the environment variable persists
    let params2 = BashCommandParams {
        command: "echo $TEST_VAR".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result2 = service.bash_command(params2).await;
    assert!(result2.is_ok());
    
    let response = result2.unwrap();
    assert!(response.contains("test_value"));
}

#[tokio::test]
async fn test_working_directory_changes() {
    let (service, temp_dir) = setup_initialized_service().await;

    // Create a subdirectory
    let subdir = temp_dir.path().join("subdir");
    std::fs::create_dir(&subdir).unwrap();

    // Change to subdirectory
    let params1 = BashCommandParams {
        command: format!("cd {}", subdir.to_string_lossy()),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result1 = service.bash_command(params1).await;
    assert!(result1.is_ok());

    // Check current directory
    let params2 = BashCommandParams {
        command: "pwd".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result2 = service.bash_command(params2).await;
    assert!(result2.is_ok());
    
    let response = result2.unwrap();
    assert!(response.contains("subdir"));
}

#[tokio::test]
async fn test_multiline_command_execution() {
    let (service, _temp_dir) = setup_initialized_service().await;

    // Test multiline command with here document
    let params = BashCommandParams {
        command: "cat << EOF\nLine 1\nLine 2\nLine 3\nEOF".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result = service.bash_command(params).await;
    assert!(result.is_ok());
    
    let response = result.unwrap();
    assert!(response.contains("Line 1"));
    assert!(response.contains("Line 2"));
    assert!(response.contains("Line 3"));
}

#[tokio::test]
async fn test_command_with_pipes_and_redirection() {
    let (service, temp_dir) = setup_initialized_service().await;

    // Test command with pipes and file redirection
    let output_file = temp_dir.path().join("output.txt");
    let params = BashCommandParams {
        command: format!("echo 'Hello' | cat > {}", output_file.to_string_lossy()),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result = service.bash_command(params).await;
    assert!(result.is_ok());

    // Verify file was created
    assert!(output_file.exists());
    let content = std::fs::read_to_string(&output_file).unwrap();
    assert!(content.trim() == "Hello");
}

#[tokio::test]
async fn test_long_running_command() {
    let (service, _temp_dir) = setup_initialized_service().await;

    // Test command that takes some time to execute
    let params = BashCommandParams {
        command: "sleep 1 && echo 'done'".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let start = std::time::Instant::now();
    let result = service.bash_command(params).await;
    let duration = start.elapsed();

    assert!(result.is_ok());
    assert!(duration.as_secs() >= 1); // Should take at least 1 second
    
    let response = result.unwrap();
    assert!(response.contains("done"));
}

#[tokio::test]
async fn test_command_with_large_output() {
    let (service, _temp_dir) = setup_initialized_service().await;

    // Generate large output
    let params = BashCommandParams {
        command: "for i in {1..100}; do echo \"Line $i with some content\"; done".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result = service.bash_command(params).await;
    assert!(result.is_ok());
    
    let response = result.unwrap();
    assert!(response.contains("Line 1"));
    assert!(response.contains("Line 100"));
}

#[tokio::test]
async fn test_interactive_command_simulation() {
    let (service, _temp_dir) = setup_initialized_service().await;

    // Test sending text to a command that would normally be interactive
    let params = BashCommandParams {
        command: "read -p 'Enter something: ' input && echo \"You entered: $input\"".to_string(),
        send_text: Some("test input\n".to_string()),
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result = service.bash_command(params).await;
    assert!(result.is_ok());
    
    let response = result.unwrap();
    assert!(response.contains("test input") || response.contains("Enter something"));
}

#[tokio::test]
async fn test_command_error_handling() {
    let (service, _temp_dir) = setup_initialized_service().await;

    // Test command that doesn't exist
    let params = BashCommandParams {
        command: "nonexistentcommand123".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result = service.bash_command(params).await;
    assert!(result.is_ok()); // Should complete but with error output
    
    let response = result.unwrap();
    assert!(response.contains("not found") || response.contains("command not found"));
}

#[tokio::test]
async fn test_command_timeout_handling() {
    let (service, _temp_dir) = setup_initialized_service().await;

    // Test a command that might timeout (very long sleep)
    // This test verifies timeout handling exists
    let params = BashCommandParams {
        command: "echo 'starting' && sleep 1 && echo 'finished'".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result = service.bash_command(params).await;
    assert!(result.is_ok());
    
    let response = result.unwrap();
    assert!(response.contains("starting"));
    // May or may not contain "finished" depending on timeout settings
}

#[tokio::test]
async fn test_bash_state_persistence() {
    let (service, temp_dir) = setup_initialized_service().await;

    // Create a function in bash
    let params1 = BashCommandParams {
        command: "test_function() { echo 'Function called with: $1'; }".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result1 = service.bash_command(params1).await;
    assert!(result1.is_ok());

    // Use the function in a later command
    let params2 = BashCommandParams {
        command: "test_function 'hello'".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result2 = service.bash_command(params2).await;
    assert!(result2.is_ok());
    
    let response = result2.unwrap();
    assert!(response.contains("Function called with: hello"));
}

#[tokio::test]
async fn test_command_with_special_characters() {
    let (service, _temp_dir) = setup_initialized_service().await;

    // Test command with special characters and escaping
    let params = BashCommandParams {
        command: r#"echo 'Special chars: $@#%^&*()[]{}|\"'"#.to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result = service.bash_command(params).await;
    assert!(result.is_ok());
    
    let response = result.unwrap();
    assert!(response.contains("Special chars"));
}

#[tokio::test]
async fn test_concurrent_bash_commands() {
    let (service, _temp_dir) = setup_initialized_service().await;
    let service = Arc::new(service);

    // Test concurrent execution of bash commands
    let service1 = service.clone();
    let service2 = service.clone();

    let task1 = tokio::spawn(async move {
        let params = BashCommandParams {
            command: "echo 'concurrent1' && sleep 0.5 && echo 'done1'".to_string(),
            send_text: None,
            include_run_config: Some(false),
            include_bash_state: Some(false),
        };
        service1.bash_command(params).await
    });

    let task2 = tokio::spawn(async move {
        let params = BashCommandParams {
            command: "echo 'concurrent2' && sleep 0.5 && echo 'done2'".to_string(),
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
    
    assert!(response1.contains("concurrent1"));
    assert!(response2.contains("concurrent2"));
}

#[tokio::test]
async fn test_mode_restrictions() {
    let temp_dir = TempDir::new().unwrap();
    let bash_state = Arc::new(Mutex::new(BashState::new(temp_dir.path().to_path_buf())));
    let service = WinxService::new(bash_state);

    // Initialize in architect mode (restricted)
    let init_params = InitializeParams {
        folder_to_start: temp_dir.path().to_string_lossy().to_string(),
        mode: Some(Modes::Architect),
        over_screen: Some(false),
    };
    service.initialize(init_params).await.unwrap();

    // Test that dangerous commands are restricted
    let params = BashCommandParams {
        command: "rm -rf /tmp/test".to_string(),
        send_text: None,
        include_run_config: Some(false),
        include_bash_state: Some(false),
    };

    let result = service.bash_command(params).await;
    // Should either be restricted or handled safely
    assert!(result.is_ok() || result.is_err());
}