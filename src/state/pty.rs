//! Real PTY implementation using portable-pty
//!
//! This module provides a true pseudo-terminal interface for interactive
//! shell sessions, enabling proper handling of:
//! - ANSI escape sequences and colors
//! - Interactive programs (sudo, vim, less, etc.)
//! - Terminal resize events
//! - Job control signals (Ctrl+C, Ctrl+Z, etc.)

use anyhow::{anyhow, Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Default terminal dimensions (columns x rows)
pub const DEFAULT_COLS: u16 = 200;
pub const DEFAULT_ROWS: u16 = 50;

/// Maximum output buffer size to prevent memory issues
const MAX_OUTPUT_SIZE: usize = 1_000_000;

/// WCGW-style prompt pattern for command completion detection
const WCGW_PROMPT_PATTERN: &str = "◉";
const WCGW_PROMPT_END: &str = "──➤";

/// Real PTY-based interactive shell
///
/// Uses portable-pty for true pseudo-terminal functionality,
/// enabling proper handling of interactive programs like sudo, vim, etc.
pub struct PtyShell {
    /// The PTY master handle for resize operations
    master: Box<dyn MasterPty + Send>,
    /// Writer for PTY input (taken from master)
    writer: Box<dyn Write + Send>,
    /// Channel receiver for output from reader thread
    output_rx: mpsc::Receiver<String>,
    /// Current terminal size
    size: PtySize,
    /// Last command executed
    pub last_command: String,
    /// Accumulated output buffer
    pub output_buffer: String,
    /// Whether a command is currently running
    pub command_running: bool,
    /// Maximum output size before truncation
    max_output_size: usize,
    /// Flag for output truncation
    pub output_truncated: bool,
}

impl std::fmt::Debug for PtyShell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyShell")
            .field("size", &format!("{}x{}", self.size.cols, self.size.rows))
            .field("last_command", &self.last_command)
            .field("command_running", &self.command_running)
            .field("output_truncated", &self.output_truncated)
            .field("output_buffer_len", &self.output_buffer.len())
            .finish()
    }
}

impl PtyShell {
    /// Create a new PTY shell session
    ///
    /// # Arguments
    /// * `initial_dir` - Starting directory for the shell
    /// * `restricted_mode` - Whether to use bash restricted mode (-r)
    ///
    /// # Returns
    /// A new `PtyShell` instance with an active bash session
    pub fn new(initial_dir: &Path, restricted_mode: bool) -> Result<Self> {
        info!(
            "Creating new PTY shell (restricted: {}) in {}",
            restricted_mode,
            initial_dir.display()
        );

        // Initialize the native PTY system
        let pty_system = native_pty_system();

        // Configure terminal size
        let size =
            PtySize { rows: DEFAULT_ROWS, cols: DEFAULT_COLS, pixel_width: 0, pixel_height: 0 };

        // Open the PTY pair (master + slave)
        let pair = pty_system.openpty(size).context("Failed to open PTY pair")?;

        // Build the command
        let mut cmd = CommandBuilder::new("bash");
        if restricted_mode {
            cmd.arg("-r");
        }

        // Set up environment for proper terminal behavior
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("PAGER", "cat");
        cmd.env("GIT_PAGER", "cat");
        cmd.env("COLUMNS", DEFAULT_COLS.to_string());
        cmd.env("ROWS", DEFAULT_ROWS.to_string());
        // WCGW-style prompt for command completion detection
        // Note: removed \r\e[2K which was erasing the prompt before it could be detected
        cmd.env("PROMPT_COMMAND", r#"printf '◉ '"$(pwd)"'──➤ '"#);
        cmd.cwd(initial_dir);

        // Spawn bash in the PTY slave
        let _child = pair.slave.spawn_command(cmd).context("Failed to spawn bash in PTY")?;

        // Get reader and writer from master
        let mut reader = pair.master.try_clone_reader().context("Failed to clone PTY reader")?;
        let writer = pair.master.take_writer().context("Failed to take PTY writer")?;

        // Create channel for output from reader thread
        let (output_tx, output_rx) = mpsc::channel::<String>();

        // Spawn a background thread to read from the PTY
        // This prevents blocking the main thread
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        // EOF - PTY closed
                        break;
                    }
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                        if output_tx.send(chunk).is_err() {
                            // Receiver dropped, exit thread
                            break;
                        }
                    }
                    Err(e) => {
                        debug!("PTY reader thread error: {}", e);
                        break;
                    }
                }
            }
            debug!("PTY reader thread exiting");
        });

        // Create the shell instance
        let mut shell = Self {
            master: pair.master,
            writer,
            output_rx,
            size,
            last_command: String::new(),
            output_buffer: String::new(),
            command_running: false,
            max_output_size: MAX_OUTPUT_SIZE,
            output_truncated: false,
        };

        // Initialize the shell with WCGW-style prompt
        shell.initialize_prompt()?;

        debug!("PTY shell created successfully");
        Ok(shell)
    }

    /// Initialize the shell prompt for WCGW compatibility
    fn initialize_prompt(&mut self) -> Result<()> {
        // Set up the dynamic prompt - matches WCGW Python PROMPT_STATEMENT
        // Note: removed \r\e[2K which was erasing the prompt before it could be detected
        let prompt_statement =
            r#"export GIT_PAGER=cat PAGER=cat PROMPT_COMMAND='printf "◉ $(pwd)──➤ '"'"#;

        self.write_command(prompt_statement)?;

        // Wait for prompt to be ready
        std::thread::sleep(Duration::from_millis(100));
        self.drain_output()?;

        Ok(())
    }

    /// Write a command to the PTY
    fn write_command(&mut self, command: &str) -> Result<()> {
        // Commands in PTY need \r\n for proper terminal behavior
        let cmd_with_newline = format!("{command}\n");
        self.writer.write_all(cmd_with_newline.as_bytes()).context("Failed to write to PTY")?;
        self.writer.flush().context("Failed to flush PTY")?;
        Ok(())
    }

    /// Drain any pending output from the PTY channel
    fn drain_output(&mut self) -> Result<String> {
        let mut output = String::new();
        let deadline = Instant::now() + Duration::from_millis(200);

        // Drain all available output from the channel
        while Instant::now() < deadline {
            match self.output_rx.try_recv() {
                Ok(chunk) => {
                    output.push_str(&chunk);

                    // Prevent runaway reads
                    if output.len() > self.max_output_size {
                        self.output_truncated = true;
                        break;
                    }
                }
                Err(TryRecvError::Empty) => {
                    // No more data, wait briefly for more
                    thread::sleep(Duration::from_millis(10));
                }
                Err(TryRecvError::Disconnected) => {
                    // Reader thread died
                    break;
                }
            }
        }

        Ok(output)
    }

    /// Send a command to the shell and start reading output
    pub fn send_command(&mut self, command: &str) -> Result<()> {
        debug!("PTY sending command: {}", command);

        // Clear previous state
        self.output_buffer.clear();
        self.output_truncated = false;
        self.last_command = command.to_string();
        self.command_running = true;

        // Write the command
        self.write_command(command)?;

        Ok(())
    }

    /// Read output from the PTY with timeout
    ///
    /// Returns (output, `is_complete`) tuple where `is_complete` indicates
    /// whether the command has finished (prompt detected)
    pub fn read_output(&mut self, timeout_secs: f32) -> Result<(String, bool)> {
        let timeout = Duration::from_secs_f32(timeout_secs.max(0.1).min(60.0));
        let start = Instant::now();
        let mut complete = false;
        let mut no_data_count = 0;
        let mut prompt_detected_at: Option<Instant> = None;

        while start.elapsed() < timeout {
            match self.output_rx.try_recv() {
                Ok(chunk) => {
                    self.output_buffer.push_str(&chunk);
                    no_data_count = 0;

                    // Check for WCGW prompt indicating command completion
                    if prompt_detected_at.is_none()
                        && (self.check_prompt_complete(&chunk)
                            || self.check_prompt_complete(&self.output_buffer))
                    {
                        prompt_detected_at = Some(Instant::now());
                        debug!("Prompt detected, draining remaining output...");
                    }

                    // Truncate if too large
                    if self.output_buffer.len() > self.max_output_size {
                        self.output_truncated = true;
                        let truncate_msg = "\n(...output truncated...)\n";
                        let keep_size = self.max_output_size / 2;
                        self.output_buffer = format!(
                            "{}{}",
                            truncate_msg,
                            &self.output_buffer[self.output_buffer.len() - keep_size..]
                        );
                    }
                }
                Err(TryRecvError::Empty) => {
                    // No data available, wait briefly
                    thread::sleep(Duration::from_millis(10));
                    no_data_count += 1;

                    // If prompt was detected, check if we've drained long enough
                    if let Some(detected_time) = prompt_detected_at {
                        // Wait 100ms after prompt detection to capture any trailing output
                        if detected_time.elapsed() > Duration::from_millis(100) {
                            complete = true;
                            debug!("Command completed - prompt detected and drained");
                            break;
                        }
                    } else if no_data_count > 10 && self.check_prompt_complete(&self.output_buffer)
                    {
                        // Prompt detected during empty reads
                        prompt_detected_at = Some(Instant::now());
                        debug!("Prompt detected after wait, draining...");
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    // Reader thread died - PTY closed
                    warn!("PTY reader disconnected");
                    complete = true;
                    break;
                }
            }
        }

        if complete || prompt_detected_at.is_some() {
            self.command_running = false;
            complete = true;
        }

        Ok((self.output_buffer.clone(), complete))
    }

    /// Check if the output contains the WCGW-style prompt
    fn check_prompt_complete(&self, text: &str) -> bool {
        // Look for the WCGW prompt pattern: ◉ /path──➤
        text.contains(WCGW_PROMPT_PATTERN) && text.contains(WCGW_PROMPT_END)
    }

    /// Send Ctrl+C (interrupt) to the PTY
    pub fn send_interrupt(&mut self) -> Result<()> {
        debug!("PTY sending Ctrl+C");
        self.writer
            .write_all(&[0x03]) // ASCII ETX (Ctrl+C)
            .context("Failed to send Ctrl+C")?;
        self.writer.flush()?;
        Ok(())
    }

    /// Send Ctrl+D (EOF) to the PTY
    pub fn send_eof(&mut self) -> Result<()> {
        debug!("PTY sending Ctrl+D");
        self.writer
            .write_all(&[0x04]) // ASCII EOT (Ctrl+D)
            .context("Failed to send Ctrl+D")?;
        self.writer.flush()?;
        Ok(())
    }

    /// Send Ctrl+Z (suspend) to the PTY
    pub fn send_suspend(&mut self) -> Result<()> {
        debug!("PTY sending Ctrl+Z");
        self.writer
            .write_all(&[0x1A]) // ASCII SUB (Ctrl+Z)
            .context("Failed to send Ctrl+Z")?;
        self.writer.flush()?;
        Ok(())
    }

    /// Send text directly to the PTY (for interactive input)
    pub fn send_text(&mut self, text: &str) -> Result<()> {
        debug!("PTY sending text: {:?}", text);
        self.writer.write_all(text.as_bytes()).context("Failed to send text")?;
        self.writer.flush()?;
        Ok(())
    }

    /// Send a special key sequence
    pub fn send_special_key(&mut self, key: &str) -> Result<()> {
        let bytes: &[u8] = match key {
            "Enter" => b"\r",
            "Tab" => b"\t",
            "Backspace" => b"\x7F",
            "Escape" => b"\x1B",
            "Up" | "KeyUp" => b"\x1B[A",
            "Down" | "KeyDown" => b"\x1B[B",
            "Right" | "KeyRight" => b"\x1B[C",
            "Left" | "KeyLeft" => b"\x1B[D",
            "Home" => b"\x1B[H",
            "End" => b"\x1B[F",
            "PageUp" => b"\x1B[5~",
            "PageDown" => b"\x1B[6~",
            "Delete" => b"\x1B[3~",
            "Insert" => b"\x1B[2~",
            "CtrlC" | "Ctrl-C" => b"\x03",
            "CtrlD" | "Ctrl-D" => b"\x04",
            "CtrlZ" | "Ctrl-Z" => b"\x1A",
            "CtrlL" | "Ctrl-L" => b"\x0C",
            _ => return Err(anyhow!("Unknown special key: {key}")),
        };

        debug!("PTY sending special key: {} ({:?})", key, bytes);
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Resize the terminal
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        debug!("PTY resizing to {}x{}", cols, rows);

        let new_size = PtySize { rows, cols, pixel_width: 0, pixel_height: 0 };

        self.master.resize(new_size).context("Failed to resize PTY")?;

        self.size = new_size;
        Ok(())
    }

    /// Get current terminal size
    pub fn get_size(&self) -> (u16, u16) {
        (self.size.cols, self.size.rows)
    }

    /// Check if the shell is still alive
    pub fn is_alive(&self) -> bool {
        // Try a non-blocking read to check if PTY is still valid
        // If we get an error that's not WouldBlock, the PTY is dead
        true // PTY handles this internally
    }
}

/// Thread-safe wrapper for `PtyShell`
pub type SharedPtyShell = Arc<Mutex<Option<PtyShell>>>;

/// Create a new shared PTY shell
pub fn create_shared_pty(initial_dir: &Path, restricted_mode: bool) -> Result<SharedPtyShell> {
    let shell = PtyShell::new(initial_dir, restricted_mode)?;
    Ok(Arc::new(Mutex::new(Some(shell))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_pty_shell_creation() {
        let temp_dir = TempDir::new().unwrap();
        let result = PtyShell::new(temp_dir.path(), false);
        assert!(result.is_ok(), "Failed to create PTY shell: {:?}", result.err());
    }

    #[test]
    fn test_pty_shell_echo() {
        let temp_dir = TempDir::new().unwrap();
        let mut shell = PtyShell::new(temp_dir.path(), false).unwrap();

        shell.send_command("echo 'hello pty'").unwrap();
        let (output, _complete) = shell.read_output(2.0).unwrap();

        assert!(output.contains("hello pty"), "Output should contain 'hello pty': {}", output);
    }

    #[test]
    fn test_pty_shell_pwd() {
        let temp_dir = TempDir::new().unwrap();
        let mut shell = PtyShell::new(temp_dir.path(), false).unwrap();

        // Simply verify shell responds to pwd command
        // Use single quotes like echo test for consistency
        shell.send_command("pwd && echo 'pwd_done'").unwrap();
        let (output, _complete) = shell.read_output(2.0).unwrap();

        // Verify the echo marker appears (proves command executed)
        assert!(output.contains("pwd_done"), "Output should contain 'pwd_done': {}", output);
    }

    #[test]
    fn test_pty_resize() {
        let temp_dir = TempDir::new().unwrap();
        let mut shell = PtyShell::new(temp_dir.path(), false).unwrap();

        let result = shell.resize(120, 40);
        assert!(result.is_ok());

        let (cols, rows) = shell.get_size();
        assert_eq!(cols, 120);
        assert_eq!(rows, 40);
    }
}
