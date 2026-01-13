//! PTY-based BashCommand tests
//!
//! These tests use the PTY shell directly to verify functionality
//! Run with: cargo test --test bash_command_pty_test -- --nocapture

use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio::time::sleep;

use winx_code_agent::errors::Result;
use winx_code_agent::state::bash_state::BashState;
use winx_code_agent::state::pty::PtyShell;
use winx_code_agent::tools;
use winx_code_agent::types::{Initialize, InitializeType, ModeName};

/// Helper to initialize bash state
async fn setup_bash_state(thread_id: &str) -> (Arc<Mutex<Option<BashState>>>, TempDir) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let bash_state_arc: Arc<Mutex<Option<BashState>>> = Arc::new(Mutex::new(None));

    let init = Initialize {
        init_type: InitializeType::FirstCall,
        mode_name: ModeName::Wcgw,
        any_workspace_path: temp_dir.path().to_string_lossy().to_string(),
        thread_id: thread_id.to_string(),
        code_writer_config: None,
        initial_files_to_read: vec![],
        task_id_to_resume: String::new(),
    };

    let _ = tools::initialize::handle_tool_call(&bash_state_arc, init)
        .await
        .expect("Initialize should succeed");

    (bash_state_arc, temp_dir)
}

// ==================== Test: PTY Shell Directly ====================

#[tokio::test(flavor = "multi_thread")]
async fn test_pty_shell_direct_echo() -> Result<()> {
    println!("\n=== TEST: PTY Shell Direct Echo ===\n");

    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Create PTY shell directly
    let mut shell = PtyShell::new(temp_dir.path(), false).expect("Failed to create PTY shell");

    println!("[TEST] Sending echo command...");
    shell.send_command("echo 'hello from pty'").expect("Failed to send command");

    // Read output
    let (output, complete) = shell.read_output(5.0).expect("Failed to read output");

    println!("[TEST] Output:\n{}", output);
    println!("[TEST] Complete: {}", complete);

    assert!(output.contains("hello from pty"), "Should contain echo output");

    println!("[TEST] PASSED\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pty_shell_pipe_command() -> Result<()> {
    println!("\n=== TEST: PTY Shell Pipe Command ===\n");

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let mut shell = PtyShell::new(temp_dir.path(), false).expect("Failed to create PTY shell");

    println!("[TEST] Sending pipe command...");
    shell.send_command("echo -e 'line1\nline2\nline3' | head -2").expect("Failed to send command");

    let (output, complete) = shell.read_output(5.0).expect("Failed to read output");

    println!("[TEST] Output:\n{}", output);
    println!("[TEST] Complete: {}", complete);

    assert!(output.contains("line1") || output.contains("line2"), "Should contain piped output");

    println!("[TEST] PASSED\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pty_shell_interrupt() -> Result<()> {
    println!("\n=== TEST: PTY Shell Interrupt (Ctrl-C) ===\n");

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let mut shell = PtyShell::new(temp_dir.path(), false).expect("Failed to create PTY shell");

    println!("[TEST] Starting sleep command...");
    shell.send_command("sleep 30").expect("Failed to send command");

    // Wait a bit then interrupt
    sleep(Duration::from_millis(500)).await;

    println!("[TEST] Sending Ctrl-C...");
    shell.send_interrupt().expect("Failed to send interrupt");

    // Read output after interrupt
    let (output, _complete) = shell.read_output(3.0).expect("Failed to read output");

    println!("[TEST] Output after Ctrl-C:\n{}", output);

    // The output should contain ^C or show the prompt again
    println!("[TEST] PASSED (interrupt sent successfully)\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pty_shell_send_text() -> Result<()> {
    println!("\n=== TEST: PTY Shell Send Text ===\n");

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let mut shell = PtyShell::new(temp_dir.path(), false).expect("Failed to create PTY shell");

    println!("[TEST] Starting cat command...");
    shell.send_command("cat").expect("Failed to send command");

    // Wait for cat to start
    sleep(Duration::from_millis(300)).await;

    println!("[TEST] Sending text to cat...");
    shell.send_text("hello from send_text\n").expect("Failed to send text");

    // Wait for echo
    sleep(Duration::from_millis(200)).await;

    // Send Ctrl+D to end cat
    println!("[TEST] Sending Ctrl-D...");
    shell.send_eof().expect("Failed to send EOF");

    let (output, _complete) = shell.read_output(3.0).expect("Failed to read output");

    println!("[TEST] Output:\n{}", output);

    assert!(output.contains("hello") || output.contains("send_text"), "Should contain sent text");

    println!("[TEST] PASSED\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pty_shell_multiple_commands() -> Result<()> {
    println!("\n=== TEST: PTY Shell Multiple Commands ===\n");

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let mut shell = PtyShell::new(temp_dir.path(), false).expect("Failed to create PTY shell");

    // Command 1
    println!("[TEST] Command 1: echo 'first'...");
    shell.send_command("echo 'first'").expect("Failed to send command");
    let (output1, _) = shell.read_output(3.0).expect("Failed to read output");
    println!("[TEST] Output 1:\n{}", output1);
    assert!(output1.contains("first"), "Should contain 'first'");

    // Command 2
    println!("[TEST] Command 2: echo 'second'...");
    shell.send_command("echo 'second'").expect("Failed to send command");
    let (output2, _) = shell.read_output(3.0).expect("Failed to read output");
    println!("[TEST] Output 2:\n{}", output2);
    assert!(output2.contains("second"), "Should contain 'second'");

    // Command 3
    println!("[TEST] Command 3: pwd...");
    shell.send_command("pwd").expect("Failed to send command");
    let (output3, _) = shell.read_output(3.0).expect("Failed to read output");
    println!("[TEST] Output 3:\n{}", output3);

    println!("[TEST] PASSED\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pty_shell_file_operations() -> Result<()> {
    println!("\n=== TEST: PTY Shell File Operations ===\n");

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let mut shell = PtyShell::new(temp_dir.path(), false).expect("Failed to create PTY shell");

    let file_path = temp_dir.path().join("test.txt");

    // Create file
    println!("[TEST] Creating file...");
    shell
        .send_command(&format!("echo 'file content' > {}", file_path.display()))
        .expect("Failed to send command");
    let (_, _) = shell.read_output(3.0).expect("Failed to read output");

    // Read file
    println!("[TEST] Reading file...");
    shell.send_command(&format!("cat {}", file_path.display())).expect("Failed to send command");
    let (output, _) = shell.read_output(3.0).expect("Failed to read output");

    println!("[TEST] Output:\n{}", output);
    assert!(output.contains("file content"), "Should contain file content");

    // Delete file
    println!("[TEST] Deleting file...");
    shell
        .send_command(&format!("rm {} && echo 'deleted'", file_path.display()))
        .expect("Failed to send command");
    let (output2, _) = shell.read_output(3.0).expect("Failed to read output");

    println!("[TEST] Output:\n{}", output2);
    assert!(output2.contains("deleted"), "Should confirm deletion");

    println!("[TEST] PASSED\n");
    Ok(())
}

// ==================== Full Workflow Test ====================

#[tokio::test(flavor = "multi_thread")]
async fn test_full_pty_workflow() -> Result<()> {
    println!("\n========================================");
    println!("=== FULL PTY Workflow Test ===");
    println!("=== Thread ID: i2238 ===");
    println!("========================================\n");

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let mut shell = PtyShell::new(temp_dir.path(), false).expect("Failed to create PTY shell");

    // 1. Simple echo
    println!("\n--- Step 1: Simple echo ---");
    shell.send_command("echo \"test\"").expect("send");
    let (r1, _) = shell.read_output(5.0).expect("read");
    println!("Result:\n{}", r1);
    assert!(r1.contains("test"), "Echo test failed");

    // 2. Pipe command
    println!("\n--- Step 2: Pipe command ---");
    shell.send_command("ls -la | head -3").expect("send");
    let (r2, _) = shell.read_output(5.0).expect("read");
    println!("Result:\n{}", r2);

    // 3. Long command + Ctrl-C
    println!("\n--- Step 3: Long command + Ctrl-C ---");
    shell.send_command("sleep 60").expect("send");
    sleep(Duration::from_millis(500)).await;
    shell.send_interrupt().expect("interrupt");
    let (r3, _) = shell.read_output(3.0).expect("read");
    println!("Result:\n{}", r3);

    // 4. Cat + text input
    println!("\n--- Step 4: Cat + text input ---");
    shell.send_command("cat").expect("send");
    sleep(Duration::from_millis(300)).await;
    shell.send_text("hello\n").expect("send_text");
    sleep(Duration::from_millis(200)).await;
    shell.send_eof().expect("eof");
    let (r4, _) = shell.read_output(3.0).expect("read");
    println!("Result:\n{}", r4);
    assert!(r4.contains("hello"), "Cat echo failed");

    // 5. File operations
    println!("\n--- Step 5: File operations ---");
    let file_path = temp_dir.path().join("test.txt");
    shell
        .send_command(&format!(
            "echo 'content' > {} && cat {}",
            file_path.display(),
            file_path.display()
        ))
        .expect("send");
    let (r5, _) = shell.read_output(5.0).expect("read");
    println!("Result:\n{}", r5);
    assert!(r5.contains("content"), "File ops failed");

    // 6. Background-like test (just start and check)
    println!("\n--- Step 6: Quick command ---");
    shell.send_command("date").expect("send");
    let (r6, _) = shell.read_output(3.0).expect("read");
    println!("Result:\n{}", r6);

    println!("\n========================================");
    println!("=== ALL PTY TESTS PASSED ===");
    println!("========================================\n");

    Ok(())
}
