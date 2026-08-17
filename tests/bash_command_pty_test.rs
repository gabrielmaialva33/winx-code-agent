//! PTY-based `BashCommand` tests
//!
//! These tests use the PTY shell directly to verify functionality
//! Run with: cargo test --test `bash_command_pty_test` -- --nocapture

use std::time::Duration;
use tempfile::TempDir;
use tokio::time::sleep;

use winx_code_agent::errors::Result;
use winx_code_agent::state::pty::PtyShell;

// ==================== Test: PTY Shell Directly ====================

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_pty_shell_direct_echo() -> Result<()> {
    let temp_dir = TempDir::new()?;

    // Create PTY shell directly
    let mut shell = PtyShell::new(temp_dir.path(), false)?;

    shell.send_command("echo 'hello from pty'")?;

    // Read output
    let (output, complete) = shell.read_output(5.0)?;

    assert!(complete, "echo command should complete");
    assert!(output.contains("hello from pty"), "Should contain echo output");

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_pty_shell_pipe_command() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let mut shell = PtyShell::new(temp_dir.path(), false)?;

    shell.send_command("echo -e 'line1\nline2\nline3' | head -2")?;

    let (output, complete) = shell.read_output(5.0)?;

    assert!(complete, "pipe command should complete");
    assert!(output.contains("line1") || output.contains("line2"), "Should contain piped output");

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_pty_shell_interrupt() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let mut shell = PtyShell::new(temp_dir.path(), false)?;

    shell.send_command("sleep 30")?;

    // Wait a bit then interrupt
    sleep(Duration::from_millis(500)).await;

    shell.send_interrupt()?;

    // Read output after interrupt
    let (_output, _complete) = shell.read_output(3.0)?;

    // The output should contain ^C or show the prompt again
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_pty_shell_send_text() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let mut shell = PtyShell::new(temp_dir.path(), false)?;

    shell.send_command("cat")?;

    // Wait for cat to start
    sleep(Duration::from_millis(300)).await;

    shell.send_text("hello from send_text\n")?;

    // Wait for echo
    sleep(Duration::from_millis(200)).await;

    // Send Ctrl+D to end cat
    shell.send_eof()?;

    let (output, _complete) = shell.read_output(3.0)?;

    assert!(output.contains("hello") || output.contains("send_text"), "Should contain sent text");

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_pty_shell_multiple_commands() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let mut shell = PtyShell::new(temp_dir.path(), false)?;

    // Command 1
    shell.send_command("echo 'first'")?;
    let (output1, _) = shell.read_output(3.0)?;
    assert!(output1.contains("first"), "Should contain 'first'");

    // Command 2
    shell.send_command("echo 'second'")?;
    let (output2, _) = shell.read_output(3.0)?;
    assert!(output2.contains("second"), "Should contain 'second'");

    // Command 3
    shell.send_command("pwd")?;
    let (_output3, _) = shell.read_output(3.0)?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_pty_shell_file_operations() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let mut shell = PtyShell::new(temp_dir.path(), false)?;

    let file_path = temp_dir.path().join("test.txt");

    // Create file
    shell.send_command(&format!("echo 'file content' > {}", file_path.display()))?;
    let (_, _) = shell.read_output(3.0)?;

    // Read file
    shell.send_command(&format!("cat {}", file_path.display()))?;
    let (output, _) = shell.read_output(3.0)?;

    assert!(output.contains("file content"), "Should contain file content");

    // Delete file
    shell.send_command(&format!("rm {} && echo 'deleted'", file_path.display()))?;
    let (output2, _) = shell.read_output(3.0)?;

    assert!(output2.contains("deleted"), "Should confirm deletion");

    Ok(())
}

// ==================== Full Workflow Test ====================

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real PTY, fails in CI"]
async fn test_full_pty_workflow() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let mut shell = PtyShell::new(temp_dir.path(), false)?;

    // 1. Simple echo
    shell.send_command("echo \"test\"")?;
    let (r1, _) = shell.read_output(5.0)?;
    assert!(r1.contains("test"), "Echo test failed");

    // 2. Pipe command
    shell.send_command("ls -la | head -3")?;
    let (_r2, _) = shell.read_output(5.0)?;

    // 3. Long command + Ctrl-C
    shell.send_command("sleep 60")?;
    sleep(Duration::from_millis(500)).await;
    shell.send_interrupt()?;
    let (_r3, _) = shell.read_output(3.0)?;

    // 4. Cat + text input
    shell.send_command("cat")?;
    sleep(Duration::from_millis(300)).await;
    shell.send_text("hello\n")?;
    sleep(Duration::from_millis(200)).await;
    shell.send_eof()?;
    let (r4, _) = shell.read_output(3.0)?;
    assert!(r4.contains("hello"), "Cat echo failed");

    // 5. File operations
    let file_path = temp_dir.path().join("test.txt");
    shell.send_command(&format!(
        "echo 'content' > {} && cat {}",
        file_path.display(),
        file_path.display()
    ))?;
    let (r5, _) = shell.read_output(5.0)?;
    assert!(r5.contains("content"), "File ops failed");

    // 6. Background-like test (just start and check)
    shell.send_command("date")?;
    let (_r6, _) = shell.read_output(3.0)?;

    Ok(())
}
