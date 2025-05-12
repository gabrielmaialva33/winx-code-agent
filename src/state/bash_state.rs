use anyhow::{anyhow, Context as AnyhowContext, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::state::terminal::MAX_OUTPUT_SIZE as TERMINAL_MAX_OUTPUT_SIZE;
use crate::state::terminal::{
    incremental_text, render_terminal_output, TerminalEmulator, TerminalOutputDiff,
    DEFAULT_MAX_SCREEN_LINES,
};
use crate::types::{
    AllowedCommands, AllowedGlobs, BashCommandMode, BashMode, FileEditMode, Modes, WriteIfEmptyMode,
};

/// FileWhitelistData tracks information about files that have been read
/// and can be edited or overwritten
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWhitelistData {
    pub file_hash: String,
    pub line_ranges_read: Vec<(usize, usize)>,
    pub total_lines: usize,
}

#[allow(dead_code)]
impl FileWhitelistData {
    #[allow(dead_code)]
    pub fn new(
        file_hash: String,
        line_ranges_read: Vec<(usize, usize)>,
        total_lines: usize,
    ) -> Self {
        Self {
            file_hash,
            line_ranges_read,
            total_lines,
        }
    }

    /// Checks if enough of the file has been read (at least 99%)
    #[allow(dead_code)]
    pub fn is_read_enough(&self) -> bool {
        self.get_percentage_read() >= 99.0
    }

    /// Calculates what percentage of the file has been read
    #[allow(dead_code)]
    pub fn get_percentage_read(&self) -> f64 {
        if self.total_lines == 0 {
            return 100.0;
        }

        let mut lines_read = std::collections::HashSet::new();
        for (start, end) in &self.line_ranges_read {
            for line in *start..=*end {
                lines_read.insert(line);
            }
        }

        (lines_read.len() as f64 / self.total_lines as f64) * 100.0
    }

    /// Returns the ranges of lines that have not been read yet
    #[allow(dead_code)]
    pub fn get_unread_ranges(&self) -> Vec<(usize, usize)> {
        if self.total_lines == 0 {
            return vec![];
        }

        let mut lines_read = std::collections::HashSet::new();
        for (start, end) in &self.line_ranges_read {
            for line in *start..=*end {
                lines_read.insert(line);
            }
        }

        let mut unread_ranges = vec![];
        let mut start_range = None;

        for i in 1..=self.total_lines {
            if !lines_read.contains(&i) {
                if start_range.is_none() {
                    start_range = Some(i);
                }
            } else if let Some(start) = start_range {
                unread_ranges.push((start, i - 1));
                start_range = None;
            }
        }

        if let Some(start) = start_range {
            unread_ranges.push((start, self.total_lines));
        }

        unread_ranges
    }

    /// Adds a range of lines to the list of lines that have been read
    #[allow(dead_code)]
    pub fn add_range(&mut self, start: usize, end: usize) {
        self.line_ranges_read.push((start, end));
    }
}

/// Terminal state for tracking command output between calls
#[derive(Debug, Clone)]
pub struct TerminalState {
    /// Last command that was executed
    pub last_command: String,
    /// Last pending output, used for incremental updates
    pub last_pending_output: String,
    /// Flag indicating if a command is currently running
    pub command_running: bool,
    /// Terminal emulator for processing output
    pub terminal_emulator: Arc<Mutex<TerminalEmulator>>,
    /// Output difference detector for efficient incremental updates
    pub diff_detector: Option<TerminalOutputDiff>,
    /// Indicates if buffer should be limited to handle large outputs
    pub limit_buffer: bool,
    /// Maximum buffer size for large outputs (in lines)
    pub max_buffer_lines: usize,
}

impl TerminalState {
    /// Creates a new terminal state
    pub fn new() -> Self {
        Self {
            last_command: String::new(),
            last_pending_output: String::new(),
            command_running: false,
            terminal_emulator: Arc::new(Mutex::new(TerminalEmulator::new(160))),
            diff_detector: Some(TerminalOutputDiff::new()),
            limit_buffer: false,
            max_buffer_lines: DEFAULT_MAX_SCREEN_LINES,
        }
    }

    /// Creates a new terminal state with custom settings
    pub fn new_with_settings(columns: usize, max_buffer_lines: usize, limit_buffer: bool) -> Self {
        Self {
            last_command: String::new(),
            last_pending_output: String::new(),
            command_running: false,
            terminal_emulator: Arc::new(Mutex::new(TerminalEmulator::new_with_max_lines(
                columns,
                max_buffer_lines,
            ))),
            diff_detector: Some(TerminalOutputDiff::new_with_max_lines(max_buffer_lines)),
            limit_buffer,
            max_buffer_lines,
        }
    }

    /// Process new output with the terminal emulator
    pub fn process_output(&mut self, output: &str) -> String {
        // Update the last pending output
        self.last_pending_output = output.to_string();

        // For large outputs, use limited buffer mode if configured
        if self.limit_buffer && output.len() > TERMINAL_MAX_OUTPUT_SIZE {
            if let Ok(mut emulator) = self.terminal_emulator.lock() {
                // Process with limited buffer
                emulator.process_with_limited_buffer(output, self.max_buffer_lines);
                let display = emulator.display();
                return display.join("\n");
            }
        }

        // Process the output with the terminal emulator
        if let Ok(mut emulator) = self.terminal_emulator.lock() {
            emulator.process(output);
            let display = emulator.display();
            display.join("\n")
        } else {
            // Fallback if we can't lock the emulator
            output.to_string()
        }
    }

    /// Get incremental output updates efficiently
    pub fn get_incremental_output(&mut self, output: &str) -> String {
        if output.is_empty() {
            return String::new();
        }

        // Use the optimized incremental_text function
        let result = incremental_text(output, &self.last_pending_output);

        // Update the last pending output
        self.last_pending_output = output.to_string();

        result
    }

    /// Reset the terminal state
    pub fn reset(&mut self) {
        self.last_command = String::new();
        self.last_pending_output = String::new();
        self.command_running = false;

        // Reset the diff detector
        if let Some(diff_detector) = &mut self.diff_detector {
            diff_detector.reset();
        }

        // Clear the terminal emulator
        if let Ok(mut emulator) = self.terminal_emulator.lock() {
            emulator.clear();
        }
    }

    /// Smart truncate the terminal output if it gets too large
    pub fn smart_truncate(&mut self, max_size: usize) {
        if let Ok(screen) = self.terminal_emulator.lock() {
            if let Ok(mut screen_guard) = screen.get_screen().lock() {
                screen_guard.smart_truncate(max_size);
            }
        }
    }
}

/// Default bash prompt to use
const DEFAULT_BASH_PROMPT: &str = "winx$ ";
/// Bash prompt statement to set up the environment
const BASH_PROMPT_STATEMENT: &str = "export GIT_PAGER=cat PAGER=cat PROMPT_COMMAND= PS1='winx$ '";
/// Maximum output size in bytes to prevent excessive memory usage
const MAX_OUTPUT_SIZE: usize = 1_000_000;
/// Maximum timeout for a command in seconds
const MAX_COMMAND_TIMEOUT: f32 = 60.0;
/// Default read interval in seconds
const DEFAULT_READ_INTERVAL: f32 = 0.1;
/// Default buffer size for output reading
const DEFAULT_BUFFER_SIZE: usize = 8192;
/// Waiting input message when a command is already running
const WAITING_INPUT_MESSAGE: &str =
    "A command is already running. You can't run multiple commands simultaneously. Options:
1. Use status_check to see current output
2. Use send_text or send_special keys to interact with the running program
3. Use Ctrl+C to interrupt the current program
4. If appropriate, consider using screen for background execution";

/// Enum for the state of a command
#[derive(Debug, Clone, PartialEq)]
pub enum CommandState {
    /// No command is running
    Idle,
    /// A command is running
    Running {
        start_time: std::time::SystemTime,
        command: String,
    },
}

/// The BashState struct holds the state of a bash session, including
/// the current working directory, workspace root, and various modes.
#[derive(Debug, Clone)]
pub struct BashState {
    pub cwd: PathBuf,
    pub workspace_root: PathBuf,
    pub current_chat_id: String,
    pub mode: Modes,
    pub bash_command_mode: BashCommandMode,
    pub file_edit_mode: FileEditMode,
    pub write_if_empty_mode: WriteIfEmptyMode,
    pub whitelist_for_overwrite: HashMap<String, FileWhitelistData>,
    /// Terminal state for tracking command output
    #[allow(dead_code)]
    pub terminal_state: TerminalState,
    /// Interactive bash process
    pub interactive_bash: Arc<Mutex<Option<InteractiveBash>>>,
}

/// BashContext wraps a BashState and provides access to it
#[allow(dead_code)]
pub struct BashContext {
    pub bash_state: BashState,
}

/// Interactive bash process that maintains a persistent bash shell
#[derive(Debug)]
pub struct InteractiveBash {
    /// The bash process
    pub process: Child,
    /// The last command that was executed
    pub last_command: String,
    /// The last output that was received
    pub last_output: String,
    /// Buffer for partial output
    pub output_buffer: String,
    /// Current command state
    pub command_state: CommandState,
    /// Maximum output size to prevent excessive memory usage
    pub max_output_size: usize,
    /// Flag indicating if output has been truncated
    pub output_truncated: bool,
    /// Output chunks for incremental updates
    pub output_chunks: Vec<String>,
}

impl InteractiveBash {
    /// Create a new interactive bash process
    pub fn new(initial_dir: &Path, restricted_mode: bool) -> Result<Self> {
        let mut cmd = Command::new("bash");
        if restricted_mode {
            cmd.arg("-r");
        }

        // Set up environment
        let cmd_env = cmd
            .env("PS1", DEFAULT_BASH_PROMPT)
            .env("PAGER", "cat")
            .env("GIT_PAGER", "cat")
            .env("PROMPT_COMMAND", "")
            .env("TERM", "xterm-256color")
            .current_dir(initial_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Spawn the process
        let mut process = cmd_env.spawn().context("Failed to spawn bash process")?;

        // Set up the prompt
        let mut stdin = process
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdin for bash process"))?;

        // Write the prompt statement to ensure consistent behavior
        writeln!(stdin, "{}", BASH_PROMPT_STATEMENT)
            .context("Failed to write prompt statement to bash process")?;

        // Return the stdin to the process
        process.stdin = Some(stdin);

        // Initialize with default values
        Ok(Self {
            process,
            last_command: String::new(),
            last_output: String::new(),
            output_buffer: String::new(),
            command_state: CommandState::Idle,
            max_output_size: MAX_OUTPUT_SIZE,
            output_truncated: false,
            output_chunks: Vec::new(),
        })
    }

    /// Send a command to the bash process
    pub fn send_command(&mut self, command: &str) -> Result<()> {
        debug!("Sending command to bash: {}", command);

        let mut stdin = self
            .process
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdin for bash process"))?;

        // Write the command and flush
        writeln!(stdin, "{}", command).context("Failed to write command to bash process")?;
        stdin.flush().context("Failed to flush bash stdin")?;

        // Return the stdin to the process
        self.process.stdin = Some(stdin);

        // Update state
        self.last_command = command.to_string();
        self.command_state = CommandState::Running {
            start_time: std::time::SystemTime::now(),
            command: command.to_string(),
        };

        Ok(())
    }

    /// Read output from the bash process with a timeout
    pub fn read_output(&mut self, timeout_secs: f32) -> Result<(String, bool)> {
        let effective_timeout = if timeout_secs <= 0.0 || timeout_secs > MAX_COMMAND_TIMEOUT {
            MAX_COMMAND_TIMEOUT
        } else {
            timeout_secs
        };

        let timeout = Duration::from_secs_f32(effective_timeout);
        let start = Instant::now();
        let mut new_output = String::new();
        let mut complete = false;

        // Combine the last output with the new output
        let mut full_output = self.last_output.clone();

        // Get references to stdout and stderr
        // Create separate scopes for getting stdout and stderr to avoid multiple mutable borrows
        let stdout_result = {
            self.process
                .stdout
                .as_mut()
                .ok_or_else(|| anyhow!("Failed to get stdout for bash process"))
        };
        let stdout = stdout_result?;

        let stderr_result = {
            self.process
                .stderr
                .as_mut()
                .ok_or_else(|| anyhow!("Failed to get stderr for bash process"))
        };
        let stderr = stderr_result?;

        // Create buffered readers with reasonable buffer sizes
        let mut stdout_reader = BufReader::with_capacity(DEFAULT_BUFFER_SIZE, stdout);
        let mut stderr_reader = BufReader::with_capacity(DEFAULT_BUFFER_SIZE, stderr);

        // Set non-blocking mode for both streams if possible
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            unsafe {
                let stdout_fd = stdout_reader.get_ref().as_raw_fd();
                let stderr_fd = stderr_reader.get_ref().as_raw_fd();

                // Get current flags
                let stdout_flags = libc::fcntl(stdout_fd, libc::F_GETFL, 0);
                let stderr_flags = libc::fcntl(stderr_fd, libc::F_GETFL, 0);

                // Set non-blocking flag
                libc::fcntl(stdout_fd, libc::F_SETFL, stdout_flags | libc::O_NONBLOCK);
                libc::fcntl(stderr_fd, libc::F_SETFL, stderr_flags | libc::O_NONBLOCK);
            }
        }

        // Patience counter for adaptive polling
        let mut patience = 3;

        // Try to read until we hit timeout or find the prompt
        while start.elapsed() < timeout && patience > 0 {
            let mut stdout_buf = vec![0; DEFAULT_BUFFER_SIZE];
            let mut stderr_buf = vec![0; DEFAULT_BUFFER_SIZE];
            let mut new_data = false;

            // Read from stdout (non-blocking)
            match stdout_reader.read(&mut stdout_buf) {
                Ok(0) => {
                    // No data or EOF
                }
                Ok(n) => {
                    // Successfully read data
                    let chunk = String::from_utf8_lossy(&stdout_buf[..n]).to_string();
                    if !chunk.is_empty() {
                        full_output.push_str(&chunk);
                        new_output.push_str(&chunk);
                        self.output_chunks.push(chunk.clone());
                        new_data = true;

                        // Check if we've received the prompt, indicating command completion
                        if chunk.ends_with(DEFAULT_BASH_PROMPT)
                            || full_output.ends_with(DEFAULT_BASH_PROMPT)
                        {
                            complete = true;
                            debug!("Command completed, prompt found in stdout");
                            break;
                        }

                        // Reset patience when we get new data
                        patience = 3;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No data available from stdout
                }
                Err(e) => {
                    warn!("Error reading from bash stdout: {}", e);
                }
            }

            // Read from stderr (non-blocking)
            match stderr_reader.read(&mut stderr_buf) {
                Ok(0) => {
                    // No data or EOF
                }
                Ok(n) => {
                    // Successfully read data
                    let chunk = String::from_utf8_lossy(&stderr_buf[..n]).to_string();
                    if !chunk.is_empty() {
                        if !full_output.contains("Stderr:") {
                            let stderr_marker = "\nStderr:\n";
                            full_output.push_str(stderr_marker);
                            new_output.push_str(stderr_marker);
                        }
                        full_output.push_str(&chunk);
                        new_output.push_str(&chunk);
                        new_data = true;

                        // Reset patience when we get new data
                        patience = 3;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No data available from stderr
                }
                Err(e) => {
                    warn!("Error reading from bash stderr: {}", e);
                }
            }

            // If output is too long, truncate to avoid memory issues
            if full_output.len() > self.max_output_size {
                self.output_truncated = true;
                let truncated_msg = format!(
                    "\n(...output truncated, showing last {} bytes...)\n",
                    self.max_output_size / 2
                );
                full_output = format!(
                    "{}{}",
                    truncated_msg,
                    &full_output[full_output.len() - self.max_output_size / 2..]
                );
            }

            // If no new data, decrease patience and sleep briefly
            if !new_data {
                patience -= 1;
                std::thread::sleep(Duration::from_millis(10));
            }

            // Check for prompt in entire output if we didn't find it in the last chunk
            if !complete && full_output.contains(DEFAULT_BASH_PROMPT) {
                complete = true;
                debug!("Command completed, prompt found in accumulated output");
                break;
            }

            // We need to drop stdout_reader and stderr_reader before checking process status
            drop(stdout_reader);
            drop(stderr_reader);

            // Now check if process has exited
            if let Ok(Some(status)) = self.process.try_wait() {
                debug!("Bash process exited with status: {:?}", status);
                let exit_message = format!("\nProcess exited with status: {:?}\n", status);
                if !full_output.contains("Process exited with status") {
                    full_output.push_str(&exit_message);
                    new_output.push_str(&exit_message);
                }
                complete = true;
                break;
            }

            // Re-create readers for next iteration
            let stdout = self
                .process
                .stdout
                .as_mut()
                .ok_or_else(|| anyhow!("Failed to get stdout for bash process"))?;
            let stderr = self
                .process
                .stderr
                .as_mut()
                .ok_or_else(|| anyhow!("Failed to get stderr for bash process"))?;

            stdout_reader = BufReader::with_capacity(DEFAULT_BUFFER_SIZE, stdout);
            stderr_reader = BufReader::with_capacity(DEFAULT_BUFFER_SIZE, stderr);
        }

        // Append timeout notice if we timed out
        if start.elapsed() >= timeout && !complete && patience <= 0 {
            debug!("Command read timed out after {:.2?}", timeout);
            let timeout_msg = format!(
                "\n(Command output reading timed out after {:.2?}, still running...)\n",
                timeout
            );
            full_output.push_str(&timeout_msg);
            new_output.push_str(&timeout_msg);
        }

        // Update state
        if complete {
            self.command_state = CommandState::Idle;
            // Record command completion time
            debug!("Command completed in {:.2?}", start.elapsed());
        }

        // Store the full output
        self.last_output = full_output.clone();
        self.output_buffer = new_output.clone();

        Ok((full_output, complete))
    }

    /// Send Ctrl+C to the process
    pub fn send_interrupt(&mut self) -> Result<()> {
        debug!("Sending interrupt (Ctrl+C) to bash process");

        if let Some(mut stdin) = self.process.stdin.take() {
            // Send Ctrl+C character (ASCII 3)
            stdin.write_all(&[3])?;
            stdin.flush()?;
            self.process.stdin = Some(stdin);

            // Wait briefly for interrupt to take effect
            std::thread::sleep(Duration::from_millis(100));

            // Check if the process is still alive
            if let Ok(Some(status)) = self.process.try_wait() {
                // Process exited due to the interrupt
                debug!("Process exited after interrupt with status: {:?}", status);
                self.command_state = CommandState::Idle;
            } else {
                // Process is still running, read any output
                match self.read_output(0.2) {
                    Ok((_output, complete)) => {
                        if complete {
                            debug!("Process completed after interrupt");
                            self.command_state = CommandState::Idle;
                        } else {
                            // Send a second Ctrl+C after a short delay for processes that need multiple signals
                            debug!(
                                "Process still running after first Ctrl+C, sending a second one"
                            );
                            if let Some(mut stdin) = self.process.stdin.take() {
                                let _ = stdin.write_all(&[3]);
                                let _ = stdin.flush();
                                self.process.stdin = Some(stdin);
                                std::thread::sleep(Duration::from_millis(100));
                            }

                            // Check again if the process has exited
                            if let Ok(Some(status)) = self.process.try_wait() {
                                debug!(
                                    "Process exited after second interrupt with status: {:?}",
                                    status
                                );
                                self.command_state = CommandState::Idle;
                            } else {
                                // If process is still not responding to Ctrl+C, try SIGTERM
                                #[cfg(unix)]
                                {
                                    // CommandExt not needed
                                    debug!("Process still running, attempting to terminate with SIGTERM");
                                    unsafe {
                                        // Get process ID
                                        let pid = self.process.id();
                                        if pid > 0 {
                                            // Send SIGTERM
                                            let _ = libc::kill(pid as i32, libc::SIGTERM);
                                            std::thread::sleep(Duration::from_millis(200));

                                            // Check if terminated
                                            if let Ok(Some(_)) = self.process.try_wait() {
                                                debug!("Process terminated with SIGTERM");
                                                self.command_state = CommandState::Idle;
                                            } else {
                                                // Process is still running, may be ignoring signals
                                                debug!("Process still running after SIGTERM");
                                                self.last_output.push_str(
                                                    "\n(Sent multiple interrupt signals, but process is still running. It may need to be killed manually)",
                                                );
                                            }
                                        }
                                    }
                                }

                                #[cfg(not(unix))]
                                {
                                    // Process might be waiting for more input or ignoring the interrupt
                                    debug!("Process still running after interrupt, may be ignoring Ctrl+C");
                                    self.last_output.push_str(
                                        "\n(Sent interrupt signals, but process is still running)",
                                    );
                                }

                                // Keep command state as running
                                if let CommandState::Running {
                                    start_time,
                                    command,
                                } = &self.command_state
                                {
                                    self.command_state = CommandState::Running {
                                        start_time: *start_time,
                                        command: command.clone(),
                                    };
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Error reading output after interrupt: {}", e);
                    }
                }
            }

            Ok(())
        } else {
            Err(anyhow!("Failed to get stdin for bash process"))
        }
    }

    /// Send special key to the process
    #[allow(dead_code)]
    pub fn send_special_key(&mut self, key: &str) -> Result<()> {
        debug!("Sending special key: {}", key);

        if let Some(mut stdin) = self.process.stdin.take() {
            let bytes = match key {
                "Enter" => b"\n".to_vec(),
                "KeyUp" => b"\x1b[A".to_vec(),
                "KeyDown" => b"\x1b[B".to_vec(),
                "KeyLeft" => b"\x1b[D".to_vec(),
                "KeyRight" => b"\x1b[C".to_vec(),
                "CtrlC" => b"\x03".to_vec(),
                "CtrlD" => b"\x04".to_vec(),
                _ => return Err(anyhow!("Unknown special key: {}", key)),
            };

            stdin.write_all(&bytes)?;
            stdin.flush()?;
            self.process.stdin = Some(stdin);

            // For Ctrl+C, use the dedicated interrupt method
            if key == "CtrlC" {
                return self.send_interrupt();
            }

            Ok(())
        } else {
            Err(anyhow!("Failed to get stdin for bash process"))
        }
    }

    /// Check if the process is still alive
    pub fn is_alive(&mut self) -> bool {
        match self.process.try_wait() {
            Ok(None) => true,     // Still running
            Ok(Some(_)) => false, // Exited
            Err(_) => false,      // Error checking status
        }
    }

    /// Get the current command state
    #[allow(dead_code)]
    pub fn command_state(&self) -> &CommandState {
        &self.command_state
    }

    /// Restart the bash process if it died
    pub fn ensure_alive(&mut self, initial_dir: &Path, restricted_mode: bool) -> Result<()> {
        if !self.is_alive() {
            debug!("Bash process is dead, restarting");

            // Create a new process
            let new_bash = Self::new(initial_dir, restricted_mode)?;

            // Replace the current process
            *self = new_bash;

            debug!("Bash process restarted successfully");
        }

        Ok(())
    }
}

impl BashState {
    /// Creates a new BashState with default settings
    pub fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
        let bash_command_mode = BashCommandMode {
            bash_mode: BashMode::NormalMode,
            allowed_commands: AllowedCommands::All("all".to_string()),
        };
        let file_edit_mode = FileEditMode {
            allowed_globs: AllowedGlobs::All("all".to_string()),
        };
        let write_if_empty_mode = WriteIfEmptyMode {
            allowed_globs: AllowedGlobs::All("all".to_string()),
        };

        Self {
            cwd: cwd.clone(),
            workspace_root: cwd,
            current_chat_id: generate_chat_id(),
            mode: Modes::Wcgw,
            bash_command_mode,
            file_edit_mode,
            write_if_empty_mode,
            whitelist_for_overwrite: HashMap::new(),
            terminal_state: TerminalState::new(),
            interactive_bash: Arc::new(Mutex::new(None)),
        }
    }

    /// Initialize the interactive bash process
    pub fn init_interactive_bash(&mut self) -> Result<()> {
        let restricted_mode = self.bash_command_mode.bash_mode == BashMode::RestrictedMode;

        debug!(
            "Initializing interactive bash (restricted: {})",
            restricted_mode
        );

        // Create a new interactive bash process
        let bash = InteractiveBash::new(&self.cwd, restricted_mode)?;

        // Update the state
        let mut guard = self
            .interactive_bash
            .lock()
            .map_err(|e| anyhow!("Failed to lock interactive bash mutex: {}", e))?;

        *guard = Some(bash);

        debug!("Interactive bash initialized successfully");

        Ok(())
    }

    /// Updates the current working directory
    pub fn update_cwd(&mut self, path: &Path) -> Result<()> {
        if path.exists() && path.is_dir() {
            self.cwd = path.to_path_buf();

            // Update cwd in interactive bash if it exists
            if let Ok(mut bash_guard) = self.interactive_bash.lock() {
                if let Some(bash) = bash_guard.as_mut() {
                    // Send cd command to bash
                    bash.send_command(&format!("cd \"{}\"", path.display()))?;
                    // Wait briefly and read output to process the cd command
                    let _ = bash.read_output(0.5)?;
                }
            }

            Ok(())
        } else {
            Err(anyhow!(
                "Path does not exist or is not a directory: {:?}",
                path
            ))
        }
    }

    /// Updates the workspace root directory
    pub fn update_workspace_root(&mut self, path: &Path) -> Result<()> {
        if path.exists() && path.is_dir() {
            self.workspace_root = path.to_path_buf();
            Ok(())
        } else {
            Err(anyhow!(
                "Path does not exist or is not a directory: {:?}",
                path
            ))
        }
    }

    /// Update the current working directory from bash
    fn update_cwd_from_bash(&self) -> Result<String> {
        let mut bash_guard = self
            .interactive_bash
            .lock()
            .map_err(|e| anyhow!("Failed to lock interactive bash mutex: {}", e))?;

        if let Some(bash) = bash_guard.as_mut() {
            // Send pwd command and read result
            bash.send_command("pwd")?;
            let (output, _) = bash.read_output(0.5)?;

            // Extract pwd result from output
            let lines: Vec<&str> = output.lines().collect();
            for line in lines {
                let trimmed = line.trim();
                if !trimmed.is_empty()
                    && !trimmed.starts_with("pwd")
                    && !trimmed.contains(DEFAULT_BASH_PROMPT)
                {
                    return Ok(trimmed.to_string());
                }
            }
        }

        // Fallback to current value
        Ok(self.cwd.display().to_string())
    }

    /// Check for background jobs
    pub fn check_background_jobs(&self) -> Result<usize> {
        let mut bash_guard = self
            .interactive_bash
            .lock()
            .map_err(|e| anyhow!("Failed to lock interactive bash mutex: {}", e))?;

        if let Some(bash) = bash_guard.as_mut() {
            // Use 'jobs -l' and manually count - avoids creating a pipeline which can leave jobs running
            bash.send_command("jobs -l")?;
            let (output, _) = bash.read_output(0.5)?;

            // Parse output to count jobs manually
            let lines = render_terminal_output(&output);
            let mut job_count = 0;

            for line in lines {
                let trimmed = line.trim();
                // Count lines that start with job numbers (e.g. "[1]", "[2]+", etc.)
                if !trimmed.is_empty() && trimmed.starts_with('[') && trimmed.contains(']') {
                    job_count += 1;
                }
            }

            // Send an empty command to ensure cleanup
            bash.send_command("")?;
            let _ = bash.read_output(0.1)?;

            return Ok(job_count);
        }

        Ok(0)
    }

    /// Execute a command in the interactive bash
    pub async fn execute_interactive(
        &mut self,
        command: &str,
        timeout_secs: f32,
    ) -> Result<String> {
        // Get effective timeout - use default if none specified
        let effective_timeout = if timeout_secs <= 0.0 {
            DEFAULT_READ_INTERVAL * 100.0 // 10 seconds default
        } else {
            timeout_secs
        };

        debug!(
            "Executing interactive command with timeout {:.2?}s: {}",
            effective_timeout, command
        );

        // We need to check initialization and command running status
        // without holding a lock across await points
        let need_init;
        let command_running_info: Option<(String, Duration)>;

        // First, lock to check state
        {
            let bash_guard = self
                .interactive_bash
                .lock()
                .map_err(|e| anyhow!("Failed to lock bash state: {}", e))?;

            need_init = bash_guard.is_none();
            command_running_info = match bash_guard.as_ref() {
                Some(bash) => match &bash.command_state {
                    CommandState::Running {
                        start_time,
                        command: ref running_command,
                    } => {
                        let elapsed = start_time
                            .elapsed()
                            .unwrap_or_else(|_| Duration::from_secs(0));
                        Some((running_command.clone(), elapsed))
                    }
                    CommandState::Idle => None,
                },
                None => None,
            };
        }

        // Handle command already running case
        if let Some((running_command, elapsed)) = command_running_info {
            // Check if this is a status check request
            if command.trim().is_empty() || command == "status_check" {
                debug!(
                    "Status check requested for running command: {}",
                    running_command
                );

                // Get current output (needs lock, but doesn't await)
                let (output, complete) = {
                    let mut bash_guard = self.interactive_bash.lock().map_err(|e| {
                        anyhow!("Failed to lock bash state for status check: {}", e)
                    })?;

                    if let Some(bash) = bash_guard.as_mut() {
                        bash.read_output(0.2)?
                    } else {
                        return Err(anyhow!(
                            "Interactive bash is None when trying to check status"
                        ));
                    }
                };

                // Process the output through terminal emulation
                let rendered_output = crate::state::terminal::incremental_text(&output, "");

                // Add status information
                let status = if complete {
                    "process exited".to_string()
                } else {
                    format!("still running (for {:.2?})", elapsed)
                };

                // Assemble final result with formatted output
                let final_result = format!(
                    "{}\n\n---\n\nstatus = {}\ncwd = {}\n",
                    rendered_output,
                    status,
                    self.cwd.display()
                );

                return Ok(final_result);
            } else {
                // A command is already running and user wants to run another
                return Err(anyhow!(
                    "{}\n\nA command is already running: '{}' (for {:.2?}).\nUse status_check to see current output, or send_text/send_specials to interact with it.",
                    WAITING_INPUT_MESSAGE,
                    running_command,
                    elapsed
                ));
            }
        }

        // Initialize bash if needed
        if need_init {
            info!("Interactive bash not initialized, initializing now");
            let mut self_mut = self.clone();

            // No await here, so no lock held across await points
            if let Err(e) = self_mut.init_interactive_bash() {
                return Err(anyhow!("Failed to initialize interactive bash: {}", e));
            }

            debug!("Successfully initialized interactive bash");
        }

        // Execute the command - split into phases to avoid holding locks across await points

        // Phase 1: Send command and get initial output (no await)
        let (initial_output, mut complete) = {
            let mut bash_guard = self
                .interactive_bash
                .lock()
                .map_err(|e| anyhow!("Failed to lock bash state for command execution: {}", e))?;

            let bash = match bash_guard.as_mut() {
                Some(b) => b,
                None => return Err(anyhow!("Interactive bash is None after initialization")),
            };

            // Ensure bash process is alive
            let restricted_mode = self.bash_command_mode.bash_mode == BashMode::RestrictedMode;
            if let Err(e) = bash.ensure_alive(&self.cwd, restricted_mode) {
                return Err(anyhow!("Failed to ensure bash process is alive: {}", e));
            }

            // Clear output buffers
            bash.output_chunks.clear();
            bash.output_truncated = false;

            // Send the command
            if let Err(e) = bash.send_command(command) {
                return Err(anyhow!("Failed to send command to bash: {}", e));
            }

            // Read initial output
            match bash.read_output(0.5) {
                Ok(result) => result,
                Err(e) => return Err(anyhow!("Failed to read initial output: {}", e)),
            }
        };

        // Store initial output
        let mut result = initial_output;

        // Phase 2: If not complete, start polling with sleeps (has await points)
        if !complete {
            debug!("Command not complete after initial read, starting polling");

            // Use adaptive polling
            let read_intervals = [0.1, 0.2, 0.5, 1.0, 2.0];
            let mut elapsed = 0.0;
            let mut patience = 3;
            let mut last_output_len = result.len();

            // Poll until complete, timeout, or patience exhausted
            while elapsed < effective_timeout && patience > 0 {
                // Calculate appropriate interval
                let interval_idx =
                    (elapsed / effective_timeout * (read_intervals.len() as f32)) as usize;
                let read_interval = read_intervals[interval_idx.min(read_intervals.len() - 1)];

                // Sleep (await point, no locks held)
                sleep(Duration::from_secs_f32(read_interval)).await;
                elapsed += read_interval;

                // Check output (lock, but no await)
                let (new_output, cmd_complete) = {
                    let mut bash_guard = match self.interactive_bash.lock() {
                        Ok(guard) => guard,
                        Err(e) => {
                            warn!("Failed to lock bash mutex during polling: {}", e);
                            continue;
                        }
                    };

                    match bash_guard.as_mut() {
                        Some(bash) => match bash.read_output(0.1) {
                            Ok(res) => res,
                            Err(e) => {
                                warn!("Error reading output during polling: {}", e);
                                patience -= 1;
                                continue;
                            }
                        },
                        None => {
                            warn!("Bash disappeared during polling");
                            break;
                        }
                    }
                };

                // Check for new content
                if new_output.len() > last_output_len {
                    patience = 3;
                    last_output_len = new_output.len();
                } else {
                    patience -= 1;
                }

                result = new_output;
                complete = cmd_complete;

                if complete {
                    debug!("Command completed during polling after {:.2?}s", elapsed);
                    break;
                }

                // Log progress
                if elapsed > 5.0 && (elapsed as usize) % 5 == 0 {
                    debug!(
                        "Still waiting for command completion - elapsed: {:.2?}s",
                        elapsed
                    );
                }
            }
        }

        // Process output through optimized terminal emulation
        let rendered_output =
            if self.terminal_state.limit_buffer && result.len() > TERMINAL_MAX_OUTPUT_SIZE {
                // For very large outputs, use the limited buffer processing
                self.terminal_state.process_output(&result)
            } else {
                // For normal outputs, use the optimized incremental text function
                self.terminal_state.get_incremental_output(&result)
            };

        // Add status information
        let status = if complete {
            "process exited"
        } else {
            "still running"
        };

        // Get current working directory (it might have changed if cd was used)
        let current_cwd = self
            .update_cwd_from_bash()
            .unwrap_or_else(|_| self.cwd.display().to_string());

        // Check for background jobs
        let bg_jobs = self.check_background_jobs().unwrap_or_default();

        // Include background job info in status if available
        let status_line = if bg_jobs > 0 {
            format!(
                "status = {}; {} background job{} running",
                status,
                bg_jobs,
                if bg_jobs == 1 { "" } else { "s" }
            )
        } else {
            format!("status = {}", status)
        };

        // Check if output is very large and might need truncation
        if result.len() > TERMINAL_MAX_OUTPUT_SIZE {
            // Apply smart truncation to avoid excessive memory usage
            self.terminal_state
                .smart_truncate(self.terminal_state.max_buffer_lines);
            debug!(
                "Applied smart truncation for large output ({} bytes)",
                result.len()
            );
        }

        // Assemble final result
        let final_result = format!(
            "{}\n\n---\n\n{}\ncwd = {}\n",
            rendered_output, status_line, current_cwd
        );

        Ok(final_result)
    }
    pub async fn check_command_status(&mut self, timeout_secs: f32) -> Result<String> {
        let mut bash_guard = self
            .interactive_bash
            .lock()
            .map_err(|e| anyhow!("Failed to lock interactive bash mutex: {}", e))?;

        if let Some(bash) = bash_guard.as_mut() {
            let (output, complete) = bash.read_output(timeout_secs)?;

            // Process output through optimized terminal emulation
            let rendered_output =
                if self.terminal_state.limit_buffer && output.len() > TERMINAL_MAX_OUTPUT_SIZE {
                    // For very large outputs, use the limited buffer processing
                    self.terminal_state.process_output(&output)
                } else {
                    // For normal outputs, use the optimized incremental text function
                    self.terminal_state.get_incremental_output(&output)
                };

            // Add status information
            let status = if complete {
                "process exited"
            } else {
                "still running"
            };

            // Check if output is very large and might need truncation
            if output.len() > TERMINAL_MAX_OUTPUT_SIZE {
                // Apply smart truncation to avoid excessive memory usage
                self.terminal_state
                    .smart_truncate(self.terminal_state.max_buffer_lines);
                debug!(
                    "Applied smart truncation for large output ({} bytes)",
                    output.len()
                );
            }

            // Assemble final result
            let final_result = format!(
                "{}\n\n---\n\nstatus = {}\ncwd = {}\n",
                rendered_output,
                status,
                self.cwd.display()
            );

            Ok(final_result)
        } else {
            Ok(format!(
                "No command running\n\n---\n\nstatus = process exited\ncwd = {}\n",
                self.cwd.display()
            ))
        }
    }
}

/// Generates a SHA256 hash of the provided data
#[allow(dead_code)]
fn sha256_hash(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Generates a random 4-digit chat ID with 'i' prefix
pub fn generate_chat_id() -> String {
    let mut rng = rand::rng();
    format!("i{}", rng.random_range(1000..10000))
}
