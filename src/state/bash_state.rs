use anyhow::{anyhow, Context as AnyhowContext, Result};
use glob;
use lazy_static::lazy_static;
use rand::Rng;
use regex::Regex;
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

use crate::state::persistence::{
    delete_bash_state as delete_state_file, load_bash_state as load_state_file,
    save_bash_state as save_state_file, BashStateSnapshot,
};
use crate::state::pty::PtyShell;
use crate::state::terminal::MAX_OUTPUT_SIZE as TERMINAL_MAX_OUTPUT_SIZE;
use crate::state::terminal::{
    incremental_text, render_terminal_output, TerminalEmulator, TerminalOutputDiff,
    DEFAULT_MAX_SCREEN_LINES,
};
use crate::types::{
    AllowedCommands, AllowedGlobs, BashCommandMode, BashMode, FileEditMode, Modes, WriteIfEmptyMode,
};
use crate::utils::error_predictor::SharedErrorPredictor;
use crate::utils::pattern_analyzer::SharedPatternAnalyzer;

/// `FileWhitelistData` tracks information about files that have been read
/// and can be edited or overwritten
/// Enhanced with WCGW-style comprehensive tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWhitelistData {
    pub file_hash: String,
    pub line_ranges_read: Vec<(usize, usize)>,
    pub total_lines: usize,
    /// Hash of the file content when it was last read
    pub content_hash: Option<String>,
    /// Timestamp when the file was last read
    pub last_read_time: Option<std::time::SystemTime>,
    /// Whether this file has been modified since last read
    pub modified_since_read: bool,
    /// Minimum percentage of file that must be read before editing
    pub min_read_percentage: f64,
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
            content_hash: None,
            last_read_time: Some(std::time::SystemTime::now()),
            modified_since_read: false,
            min_read_percentage: 99.0, // WCGW default
        }
    }

    /// Create new `FileWhitelistData` with enhanced tracking
    pub fn new_enhanced(
        file_hash: String,
        content_hash: String,
        line_ranges_read: Vec<(usize, usize)>,
        total_lines: usize,
        min_read_percentage: f64,
    ) -> Self {
        Self {
            file_hash,
            line_ranges_read,
            total_lines,
            content_hash: Some(content_hash),
            last_read_time: Some(std::time::SystemTime::now()),
            modified_since_read: false,
            min_read_percentage,
        }
    }

    /// Checks if enough of the file has been read using configurable threshold
    #[allow(dead_code)]
    pub fn is_read_enough(&self) -> bool {
        self.get_percentage_read() >= self.min_read_percentage
    }

    /// Checks if file needs to be read more before editing (WCGW-style protection)
    pub fn needs_more_reading(&self) -> bool {
        !self.is_read_enough()
    }

    /// Check if file has been modified since last read using hash comparison
    pub fn check_file_changed(&mut self, current_content_hash: &str) -> bool {
        if let Some(ref stored_hash) = self.content_hash {
            if stored_hash == current_content_hash {
                false
            } else {
                self.modified_since_read = true;
                true
            }
        } else {
            // No stored hash, assume changed
            self.modified_since_read = true;
            true
        }
    }

    /// Update file hash and reset modification flag
    pub fn update_content_hash(&mut self, new_hash: String) {
        self.content_hash = Some(new_hash);
        self.modified_since_read = false;
        self.last_read_time = Some(std::time::SystemTime::now());
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

    /// Merge overlapping ranges to optimize tracking
    pub fn optimize_ranges(&mut self) {
        if self.line_ranges_read.is_empty() {
            return;
        }

        // Sort ranges by start position
        self.line_ranges_read.sort_by(|a, b| a.0.cmp(&b.0));

        let mut merged = Vec::new();
        let mut current = self.line_ranges_read[0];

        for &(start, end) in &self.line_ranges_read[1..] {
            if start <= current.1 + 1 {
                // Overlapping or adjacent ranges, merge them
                current.1 = current.1.max(end);
            } else {
                // Non-overlapping range, add current to merged and start new
                merged.push(current);
                current = (start, end);
            }
        }
        merged.push(current);

        self.line_ranges_read = merged;
    }

    /// Adds a range of lines to the list of lines that have been read
    #[allow(dead_code)]
    pub fn add_range(&mut self, start: usize, end: usize) {
        self.line_ranges_read.push((start, end));
        self.last_read_time = Some(std::time::SystemTime::now());
    }

    /// Get a human-readable error message for insufficient reading
    pub fn get_read_error_message(&self, file_path: &Path) -> String {
        let unread_ranges = self.get_unread_ranges();
        let percentage_read = self.get_percentage_read();

        if unread_ranges.is_empty() {
            format!(
                "File {} has been read ({:.1}% coverage), but minimum required is {:.1}%",
                file_path.display(),
                percentage_read,
                self.min_read_percentage
            )
        } else {
            let range_descriptions: Vec<String> = unread_ranges
                .iter()
                .take(3) // Show max 3 ranges
                .map(|(start, end)| {
                    if start == end {
                        format!("line {start}")
                    } else {
                        format!("lines {start}-{end}")
                    }
                })
                .collect();

            let ranges_str = if unread_ranges.len() > 3 {
                format!(
                    "{} and {} more ranges",
                    range_descriptions.join(", "),
                    unread_ranges.len() - 3
                )
            } else {
                range_descriptions.join(", ")
            };

            format!(
                "File {} needs more reading. Only {:.1}% read (need {:.1}%). Unread: {}",
                file_path.display(),
                percentage_read,
                self.min_read_percentage,
                ranges_str
            )
        }
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

impl Default for TerminalState {
    fn default() -> Self {
        Self::new()
    }
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

/// Dynamic bash prompt regex pattern - matches WCGW Python `PROMPT_CONST` exactly
/// Format: ◉ /path/to/dir──➤
const WCGW_PROMPT_PATTERN: &str = r"◉ ([^\n]*)──➤";

/// `PROMPT_COMMAND` that displays dynamic prompt with cwd - matches WCGW Python exactly
/// Uses printf with special formatting to show current directory
/// Note: removed \r\e[2K which was erasing the prompt before it could be detected
const WCGW_PROMPT_COMMAND: &str = r#"printf '◉ '"$(pwd)"'──➤ '"#;

/// Bash prompt statement to set up the dynamic prompt - matches WCGW Python `PROMPT_STATEMENT` setup
/// Note: removed \r\e[2K which was erasing the prompt before it could be detected
const BASH_PROMPT_STATEMENT: &str =
    r#"export GIT_PAGER=cat PAGER=cat PROMPT_COMMAND='printf "◉ $(pwd)──➤ '"'"#;

/// Fallback static prompt for detection (used when dynamic prompt fails)
const FALLBACK_PROMPT: &str = "winx$ ";

lazy_static! {
    /// Compiled regex for WCGW-style dynamic prompt detection
    /// Matches: ◉ /path/to/dir──➤
    static ref PROMPT_REGEX: Regex = Regex::new(WCGW_PROMPT_PATTERN).expect("Invalid prompt regex");
}

/// Check if text contains the WCGW-style prompt - matches WCGW Python prompt detection
fn contains_wcgw_prompt(text: &str) -> bool {
    PROMPT_REGEX.is_match(text)
}

/// Extract the current working directory from the prompt - matches WCGW Python prompt parsing
fn extract_cwd_from_prompt(text: &str) -> Option<String> {
    PROMPT_REGEX.captures(text).map(|caps| caps[1].to_string())
}

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
    Running { start_time: std::time::SystemTime, command: String },
}

/// The `BashState` struct holds the state of a bash session, including
/// the current working directory, workspace root, and various modes.
#[derive(Debug, Clone)]
pub struct BashState {
    pub cwd: PathBuf,
    pub workspace_root: PathBuf,
    pub current_thread_id: String,
    pub mode: Modes,
    pub bash_command_mode: BashCommandMode,
    pub file_edit_mode: FileEditMode,
    pub write_if_empty_mode: WriteIfEmptyMode,
    pub whitelist_for_overwrite: HashMap<String, FileWhitelistData>,
    /// Terminal state for tracking command output
    #[allow(dead_code)]
    pub terminal_state: TerminalState,
    /// Interactive bash process (legacy - being replaced by PTY)
    pub interactive_bash: Arc<Mutex<Option<InteractiveBash>>>,
    /// Real PTY shell (preferred over `interactive_bash`)
    pub pty_shell: Arc<Mutex<Option<PtyShell>>>,
    /// Pattern analyzer for intelligent command suggestions
    pub pattern_analyzer: SharedPatternAnalyzer,
    /// Error predictor for error prevention
    pub error_predictor: SharedErrorPredictor,
    /// Flag indicating if this state has been initialized
    pub initialized: bool,
}

/// `BashContext` wraps a `BashState` and provides access to it
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
    /// Initial directory for the shell (needed for reinit)
    initial_dir: PathBuf,
    /// Whether restricted mode is enabled (needed for reinit)
    restricted_mode: bool,
}

impl InteractiveBash {
    /// Check if the bash process is still alive
    pub fn is_alive(&mut self) -> bool {
        // try_wait returns Ok(None) if process is still running
        // Returns Ok(Some(status)) if process has exited
        // Returns Err if checking status failed
        matches!(self.process.try_wait(), Ok(None))
    }

    /// Reinitialize the bash process after it has died
    /// This is called automatically when the process is detected as dead
    /// Uses the stored `initial_dir` and `restricted_mode` from when the shell was created
    pub fn reinit(&mut self) -> Result<()> {
        info!("Reinitializing bash process after death in {}", self.initial_dir.display());

        let mut cmd = Command::new("bash");
        if self.restricted_mode {
            cmd.arg("-r");
        }

        let cmd_env = cmd
            .env("PAGER", "cat")
            .env("GIT_PAGER", "cat")
            .env("PROMPT_COMMAND", WCGW_PROMPT_COMMAND)
            .env("TERM", "xterm-256color")
            .env("COLUMNS", "200")
            .env("ROWS", "50")
            .current_dir(&self.initial_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut process = cmd_env.spawn().context("Failed to respawn bash process")?;

        let mut stdin = process
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdin for respawned bash process"))?;

        writeln!(stdin, "{BASH_PROMPT_STATEMENT}")
            .context("Failed to write prompt statement to respawned bash")?;
        stdin.flush().context("Failed to flush prompt statement")?;

        process.stdin = Some(stdin);

        // Replace the old process
        self.process = process;
        self.command_state = CommandState::Idle;
        self.output_truncated = false;
        self.output_chunks.clear();

        info!("Bash process reinitialized successfully");
        Ok(())
    }

    /// Ensure the bash process is alive, reinitializing if necessary
    /// Returns Ok(()) if process is alive or was successfully reinitialized
    pub fn ensure_alive(&mut self) -> Result<()> {
        if !self.is_alive() {
            warn!("Bash process died, reinitializing...");
            self.reinit()?;
        }
        Ok(())
    }

    /// Create a new interactive bash process - matches WCGW Python pexpect.spawn behavior
    pub fn new(initial_dir: &Path, restricted_mode: bool) -> Result<Self> {
        let mut cmd = Command::new("bash");
        if restricted_mode {
            cmd.arg("-r");
        }

        // Set up environment - matches WCGW Python spawn_bash env setup
        let cmd_env = cmd
            .env("PAGER", "cat")
            .env("GIT_PAGER", "cat")
            // WCGW-style dynamic PROMPT_COMMAND that shows ◉ /path──➤
            .env("PROMPT_COMMAND", WCGW_PROMPT_COMMAND)
            .env("TERM", "xterm-256color")
            // Set COLUMNS/ROWS for proper terminal emulation - matches WCGW Python
            .env("COLUMNS", "200")
            .env("ROWS", "50")
            .current_dir(initial_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Spawn the process
        let mut process = cmd_env.spawn().context("Failed to spawn bash process")?;

        // Set up the dynamic prompt - matches WCGW Python PROMPT_STATEMENT
        let mut stdin =
            process.stdin.take().ok_or_else(|| anyhow!("Failed to get stdin for bash process"))?;

        // Write the prompt statement to ensure consistent behavior - matches WCGW Python
        writeln!(stdin, "{BASH_PROMPT_STATEMENT}")
            .context("Failed to write prompt statement to bash process")?;
        stdin.flush().context("Failed to flush prompt statement")?;

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
            initial_dir: initial_dir.to_path_buf(),
            restricted_mode,
        })
    }

    /// Send a command to the bash process
    pub fn send_command(&mut self, command: &str) -> Result<()> {
        debug!("Sending command to bash: {}", command);

        // Ensure bash is alive, reinitialize if it died (e.g., after Ctrl-c)
        self.ensure_alive()?;

        let mut stdin = self
            .process
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdin for bash process"))?;

        // Write the command and flush
        writeln!(stdin, "{command}").context("Failed to write command to bash process")?;
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
            // SAFETY: fcntl is used to set non-blocking mode on valid file descriptors.
            // The file descriptors are obtained from BufReader wrapping stdout/stderr of a
            // spawned Child process, guaranteeing they are valid open file descriptors.
            // fcntl(F_GETFL/F_SETFL) is a safe operation that cannot cause UB with valid fds.
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
        // Increased from 3 to 10 to give more time for PROMPT_COMMAND to execute
        // Each patience tick is ~10ms, so 10 = ~100ms wait after last output
        let mut patience = 10;

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

                        // Check if we've received the WCGW-style prompt, indicating command completion
                        // Uses regex to match ◉ /path──➤ format - matches WCGW Python PROMPT_CONST
                        if contains_wcgw_prompt(&chunk)
                            || contains_wcgw_prompt(&full_output)
                            || chunk.ends_with(FALLBACK_PROMPT)
                            || full_output.ends_with(FALLBACK_PROMPT)
                        {
                            complete = true;
                            debug!("Command completed, WCGW prompt found in stdout");
                            break;
                        }

                        // Reset patience when we get new data
                        patience = 10;
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
                        patience = 10;
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

            // Check for WCGW prompt in entire output if we didn't find it in the last chunk
            // Uses regex for dynamic ◉ /path──➤ format - matches WCGW Python
            if !complete
                && (contains_wcgw_prompt(&full_output) || full_output.contains(FALLBACK_PROMPT))
            {
                complete = true;
                debug!("Command completed, WCGW prompt found in accumulated output");
                break;
            }

            // We need to drop stdout_reader and stderr_reader before checking process status
            drop(stdout_reader);
            drop(stderr_reader);

            // Now check if process has exited
            if let Ok(Some(status)) = self.process.try_wait() {
                debug!("Bash process exited with status: {:?}", status);
                let exit_message = format!("\nProcess exited with status: {status:?}\n");
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
                "\n(Command output reading timed out after {timeout:.2?}, still running...)\n"
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
                                    debug!("Process still running, attempting to terminate with SIGTERM");
                                    // SAFETY: libc::kill sends a signal to a process.
                                    // - The pid is obtained from self.process.id() which returns the OS pid
                                    // - We verify pid > 0 and fits in i32 before sending to avoid invalid process IDs
                                    // - SIGTERM is a safe signal that requests graceful termination
                                    // - The return value is ignored as we check process status after
                                    unsafe {
                                        let pid = self.process.id();
                                        // SECURITY: Use safe conversion to prevent wrap on systems with large PIDs
                                        if let Ok(pid_i32) = i32::try_from(pid) {
                                            if pid_i32 > 0 {
                                                let _ = libc::kill(pid_i32, libc::SIGTERM);
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
                                if let CommandState::Running { start_time, command } =
                                    &self.command_state
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
                _ => return Err(anyhow!("Unknown special key: {key}")),
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

    /// Get the current command state
    #[allow(dead_code)]
    pub fn command_state(&self) -> &CommandState {
        &self.command_state
    }

    // NOTE: is_alive and ensure_alive are now defined at line ~496/~551
    // and use stored initial_dir/restricted_mode for reinit
}

impl Default for BashState {
    fn default() -> Self {
        Self::new()
    }
}

impl BashState {
    /// Creates a new `BashState` with default settings
    pub fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
        let bash_command_mode = BashCommandMode {
            bash_mode: BashMode::NormalMode,
            allowed_commands: AllowedCommands::All("all".to_string()),
        };
        let file_edit_mode = FileEditMode { allowed_globs: AllowedGlobs::All("all".to_string()) };
        let write_if_empty_mode =
            WriteIfEmptyMode { allowed_globs: AllowedGlobs::All("all".to_string()) };

        Self {
            cwd: cwd.clone(),
            workspace_root: cwd,
            current_thread_id: generate_thread_id(),
            mode: Modes::Wcgw,
            bash_command_mode,
            file_edit_mode,
            write_if_empty_mode,
            whitelist_for_overwrite: HashMap::new(),
            terminal_state: TerminalState::new(),
            interactive_bash: Arc::new(Mutex::new(None)),
            pty_shell: Arc::new(Mutex::new(None)),
            pattern_analyzer: SharedPatternAnalyzer::new(),
            error_predictor: SharedErrorPredictor::new(),
            initialized: false,
        }
    }

    /// Initialize the interactive bash process
    pub fn init_interactive_bash(&mut self) -> Result<()> {
        let restricted_mode = self.bash_command_mode.bash_mode == BashMode::RestrictedMode;

        debug!("Initializing interactive bash (restricted: {})", restricted_mode);

        // Create a new interactive bash process
        let bash = InteractiveBash::new(&self.cwd, restricted_mode)?;

        // Update the state
        let mut guard = self
            .interactive_bash
            .lock()
            .map_err(|e| anyhow!("Failed to lock interactive bash mutex: {e}"))?;

        *guard = Some(bash);

        debug!("Interactive bash initialized successfully");

        Ok(())
    }

    /// Initialize the PTY shell (preferred over `interactive_bash`)
    ///
    /// This uses a real pseudo-terminal for better compatibility with
    /// interactive programs like sudo, vim, less, etc.
    pub fn init_pty_shell(&mut self) -> Result<()> {
        let restricted_mode = self.bash_command_mode.bash_mode == BashMode::RestrictedMode;

        info!("Initializing PTY shell (restricted: {}) in {}", restricted_mode, self.cwd.display());

        // Create a new PTY shell
        let shell = PtyShell::new(&self.cwd, restricted_mode)?;

        // Update the state
        let mut guard =
            self.pty_shell.lock().map_err(|e| anyhow!("Failed to lock PTY shell mutex: {e}"))?;

        *guard = Some(shell);

        info!("PTY shell initialized successfully");

        Ok(())
    }

    /// Check if PTY shell is available
    pub fn has_pty_shell(&self) -> bool {
        self.pty_shell.lock().map(|guard| guard.is_some()).unwrap_or(false)
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
            Err(anyhow!("Path does not exist or is not a directory: {path:?}"))
        }
    }

    /// Updates the workspace root directory
    pub fn update_workspace_root(&mut self, path: &Path) -> Result<()> {
        if path.exists() && path.is_dir() {
            self.workspace_root = path.to_path_buf();
            Ok(())
        } else {
            Err(anyhow!("Path does not exist or is not a directory: {path:?}"))
        }
    }

    /// Update the current working directory from bash
    fn update_cwd_from_bash(&self) -> Result<String> {
        let mut bash_guard = self
            .interactive_bash
            .lock()
            .map_err(|e| anyhow!("Failed to lock interactive bash mutex: {e}"))?;

        if let Some(bash) = bash_guard.as_mut() {
            // Send pwd command and read result
            bash.send_command("pwd")?;
            let (output, _) = bash.read_output(0.5)?;

            // Extract pwd result from output - filter out prompt lines
            // First, try to extract cwd from WCGW-style prompt if present
            if let Some(cwd_from_prompt) = extract_cwd_from_prompt(&output) {
                return Ok(cwd_from_prompt);
            }

            // Fall back to parsing output lines
            let lines: Vec<&str> = output.lines().collect();
            for line in lines {
                let trimmed = line.trim();
                if !trimmed.is_empty()
                    && !trimmed.starts_with("pwd")
                    && !contains_wcgw_prompt(trimmed)
                    && !trimmed.contains(FALLBACK_PROMPT)
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
            .map_err(|e| anyhow!("Failed to lock interactive bash mutex: {e}"))?;

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
                .map_err(|e| anyhow!("Failed to lock bash state: {e}"))?;

            need_init = bash_guard.is_none();
            command_running_info = match bash_guard.as_ref() {
                Some(bash) => match &bash.command_state {
                    CommandState::Running { start_time, command: ref running_command } => {
                        let elapsed =
                            start_time.elapsed().unwrap_or_else(|_| Duration::from_secs(0));
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
                debug!("Status check requested for running command: {}", running_command);

                // Get current output (needs lock, but doesn't await)
                let (output, complete) = {
                    let mut bash_guard = self
                        .interactive_bash
                        .lock()
                        .map_err(|e| anyhow!("Failed to lock bash state for status check: {e}"))?;

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
                    format!("still running (for {elapsed:.2?})")
                };

                // Assemble final result with formatted output
                let final_result = format!(
                    "{}\n\n---\n\nstatus = {}\ncwd = {}\n",
                    rendered_output,
                    status,
                    self.cwd.display()
                );

                return Ok(final_result);
            }
            // A command is already running and user wants to run another
            return Err(anyhow!(
                "{WAITING_INPUT_MESSAGE}\n\nA command is already running: '{running_command}' (for {elapsed:.2?}).\nUse status_check to see current output, or send_text/send_specials to interact with it."
            ));
        }

        // Initialize bash if needed
        if need_init {
            info!("Interactive bash not initialized, initializing now");
            let mut self_mut = self.clone();

            // No await here, so no lock held across await points
            if let Err(e) = self_mut.init_interactive_bash() {
                return Err(anyhow!("Failed to initialize interactive bash: {e}"));
            }

            debug!("Successfully initialized interactive bash");
        }

        // Execute the command - split into phases to avoid holding locks across await points

        // Phase 1: Send command and get initial output (no await)
        let (initial_output, mut complete) = {
            let mut bash_guard = self
                .interactive_bash
                .lock()
                .map_err(|e| anyhow!("Failed to lock bash state for command execution: {e}"))?;

            let bash = match bash_guard.as_mut() {
                Some(b) => b,
                None => return Err(anyhow!("Interactive bash is None after initialization")),
            };

            // Ensure bash process is alive (reinit if needed)
            if let Err(e) = bash.ensure_alive() {
                return Err(anyhow!("Failed to ensure bash process is alive: {e}"));
            }

            // Clear output buffers
            bash.output_chunks.clear();
            bash.output_truncated = false;

            // Send the command
            if let Err(e) = bash.send_command(command) {
                return Err(anyhow!("Failed to send command to bash: {e}"));
            }

            // Read initial output
            match bash.read_output(0.5) {
                Ok(result) => result,
                Err(e) => return Err(anyhow!("Failed to read initial output: {e}")),
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
            let mut patience = 10;
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

                    if let Some(bash) = bash_guard.as_mut() {
                        match bash.read_output(0.1) {
                            Ok(res) => res,
                            Err(e) => {
                                warn!("Error reading output during polling: {}", e);
                                patience -= 1;
                                continue;
                            }
                        }
                    } else {
                        warn!("Bash disappeared during polling");
                        break;
                    }
                };

                // Check for new content
                if new_output.len() > last_output_len {
                    patience = 10;
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
                if elapsed > 5.0 && (elapsed as usize).is_multiple_of(5) {
                    debug!("Still waiting for command completion - elapsed: {:.2?}s", elapsed);
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
        let status = if complete { "process exited" } else { "still running" };

        // Get current working directory (it might have changed if cd was used)
        let current_cwd =
            self.update_cwd_from_bash().unwrap_or_else(|_| self.cwd.display().to_string());

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
            format!("status = {status}")
        };

        // Check if output is very large and might need truncation
        if result.len() > TERMINAL_MAX_OUTPUT_SIZE {
            // Apply smart truncation to avoid excessive memory usage
            self.terminal_state.smart_truncate(self.terminal_state.max_buffer_lines);
            debug!("Applied smart truncation for large output ({} bytes)", result.len());
        }

        // Assemble final result
        let final_result =
            format!("{rendered_output}\n\n---\n\n{status_line}\ncwd = {current_cwd}\n");

        // Record command in pattern analyzer for future suggestions
        if !command.trim().is_empty() && command != "status_check" {
            // Don't record status checks or empty commands
            if let Err(e) = self.pattern_analyzer.record_command(command, &current_cwd).await {
                warn!("Failed to record command in pattern analyzer: {}", e);
            }
        }

        Ok(final_result)
    }

    /// Execute a command using the real PTY shell
    ///
    /// This is the preferred method for command execution as it uses a real
    /// pseudo-terminal, enabling proper handling of interactive programs.
    pub fn execute_pty(&mut self, command: &str, timeout_secs: f32) -> Result<String> {
        let effective_timeout = if timeout_secs <= 0.0 { 10.0 } else { timeout_secs };

        debug!("Executing PTY command with timeout {:.2?}s: {}", effective_timeout, command);

        // Initialize PTY if needed
        let need_init = {
            let guard =
                self.pty_shell.lock().map_err(|e| anyhow!("Failed to lock PTY mutex: {e}"))?;
            guard.is_none()
        };

        if need_init {
            info!("PTY shell not initialized, initializing now");
            self.init_pty_shell()?;
        }

        // Execute the command
        let mut guard =
            self.pty_shell.lock().map_err(|e| anyhow!("Failed to lock PTY mutex: {e}"))?;

        let shell =
            guard.as_mut().ok_or_else(|| anyhow!("PTY shell is None after initialization"))?;

        // Send the command
        shell.send_command(command)?;

        // Read output with timeout
        let (output, complete) = shell.read_output(effective_timeout)?;

        // Process output through terminal emulation
        let rendered_output = if output.len() > TERMINAL_MAX_OUTPUT_SIZE {
            self.terminal_state.process_output(&output)
        } else {
            incremental_text(&output, "")
        };

        // Get status
        let status = if complete { "process exited" } else { "still running" };

        // Format result
        let final_result = format!(
            "{}\n\n---\n\nstatus = {}\ncwd = {}\n",
            rendered_output,
            status,
            self.cwd.display()
        );

        Ok(final_result)
    }

    /// Send interrupt (Ctrl+C) to the PTY shell
    pub fn send_pty_interrupt(&mut self) -> Result<()> {
        let mut guard =
            self.pty_shell.lock().map_err(|e| anyhow!("Failed to lock PTY mutex: {e}"))?;

        if let Some(shell) = guard.as_mut() {
            shell.send_interrupt()
        } else {
            Err(anyhow!("PTY shell not initialized"))
        }
    }

    /// Send text directly to the PTY shell (for interactive input)
    pub fn send_pty_text(&mut self, text: &str) -> Result<()> {
        let mut guard =
            self.pty_shell.lock().map_err(|e| anyhow!("Failed to lock PTY mutex: {e}"))?;

        if let Some(shell) = guard.as_mut() {
            shell.send_text(text)
        } else {
            Err(anyhow!("PTY shell not initialized"))
        }
    }

    /// Send a special key to the PTY shell
    pub fn send_pty_special_key(&mut self, key: &str) -> Result<()> {
        let mut guard =
            self.pty_shell.lock().map_err(|e| anyhow!("Failed to lock PTY mutex: {e}"))?;

        if let Some(shell) = guard.as_mut() {
            shell.send_special_key(key)
        } else {
            Err(anyhow!("PTY shell not initialized"))
        }
    }

    /// Resize the PTY terminal
    pub fn resize_pty(&mut self, cols: u16, rows: u16) -> Result<()> {
        let mut guard =
            self.pty_shell.lock().map_err(|e| anyhow!("Failed to lock PTY mutex: {e}"))?;

        if let Some(shell) = guard.as_mut() {
            shell.resize(cols, rows)
        } else {
            Err(anyhow!("PTY shell not initialized"))
        }
    }

    pub async fn check_command_status(&mut self, timeout_secs: f32) -> Result<String> {
        let mut bash_guard = self
            .interactive_bash
            .lock()
            .map_err(|e| anyhow!("Failed to lock interactive bash mutex: {e}"))?;

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
            let status = if complete { "process exited" } else { "still running" };

            // Check if output is very large and might need truncation
            if output.len() > TERMINAL_MAX_OUTPUT_SIZE {
                // Apply smart truncation to avoid excessive memory usage
                self.terminal_state.smart_truncate(self.terminal_state.max_buffer_lines);
                debug!("Applied smart truncation for large output ({} bytes)", output.len());
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

    // Enhanced mode validation methods (inspired by WCGW)

    /// Check if a command is allowed in the current mode
    pub fn is_command_allowed(&self, command: &str) -> bool {
        match self.mode {
            Modes::Wcgw => true, // Full permissions
            Modes::Architect => {
                // Architect mode: only read-only commands allowed
                self.is_readonly_command(command)
            }
            Modes::CodeWriter => {
                // Code writer mode: check against allowed commands
                match &self.bash_command_mode.allowed_commands {
                    AllowedCommands::All(_) => true,
                    AllowedCommands::List(commands) => {
                        commands.iter().any(|allowed| self.command_matches(command, allowed))
                    }
                }
            }
        }
    }

    /// Check if a file path is allowed for editing in the current mode
    pub fn is_file_edit_allowed(&self, file_path: &str) -> bool {
        match self.mode {
            Modes::Wcgw => true,       // Full permissions
            Modes::Architect => false, // No file editing in architect mode
            Modes::CodeWriter => {
                // Code writer mode: check against allowed globs
                match &self.file_edit_mode.allowed_globs {
                    AllowedGlobs::All(_) => true,
                    AllowedGlobs::List(globs) => {
                        globs.iter().any(|glob| self.path_matches_glob(file_path, glob))
                    }
                }
            }
        }
    }

    /// Check if a file path is allowed for writing (new files) in the current mode  
    pub fn is_file_write_allowed(&self, file_path: &str) -> bool {
        match self.mode {
            Modes::Wcgw => true,       // Full permissions
            Modes::Architect => false, // No file writing in architect mode
            Modes::CodeWriter => {
                // Code writer mode: check against allowed globs
                match &self.write_if_empty_mode.allowed_globs {
                    AllowedGlobs::All(_) => true,
                    AllowedGlobs::List(globs) => {
                        globs.iter().any(|glob| self.path_matches_glob(file_path, glob))
                    }
                }
            }
        }
    }

    /// Check if a command is read-only (safe for architect mode)
    fn is_readonly_command(&self, command: &str) -> bool {
        let cmd = command.trim().to_lowercase();

        // List of read-only commands allowed in architect mode
        let readonly_commands = [
            "ls",
            "cat",
            "head",
            "tail",
            "less",
            "more",
            "find",
            "grep",
            "wc",
            "file",
            "pwd",
            "which",
            "whereis",
            "type",
            "ps",
            "top",
            "df",
            "du",
            "free",
            "uname",
            "whoami",
            "id",
            "date",
            "history",
            "echo",
            "printf",
            "tree",
            "stat",
            "readlink",
            "dirname",
            "basename",
            // Git read-only commands
            "git status",
            "git log",
            "git show",
            "git diff",
            "git branch",
            "git remote",
            // Package manager queries
            "pip list",
            "npm list",
            "cargo tree",
            "gem list",
            // Language-specific inspections
            "python --version",
            "node --version",
            "cargo --version",
            "rustc --version",
        ];

        // Check for exact matches first
        if readonly_commands.iter().any(|&readonly_cmd| cmd.starts_with(readonly_cmd)) {
            return true;
        }

        // Check for dangerous patterns that should be blocked
        let dangerous_patterns = [
            "rm",
            "mv",
            "cp",
            "chmod",
            "chown",
            "sudo",
            "su",
            "kill",
            "killall",
            "mkdir",
            "rmdir",
            "touch",
            "dd",
            "mount",
            "umount",
            "git add",
            "git commit",
            "git push",
            "git pull",
            "git merge",
            "git rebase",
            "npm install",
            "pip install",
            "cargo install",
            "gem install",
            "make",
            "cmake",
            "gcc",
            "g++",
            "clang",
            "rustc",
        ];

        !dangerous_patterns.iter().any(|&dangerous| cmd.contains(dangerous))
    }

    /// Check if a command matches an allowed command pattern
    fn command_matches(&self, command: &str, pattern: &str) -> bool {
        if pattern == "all" || pattern == "*" {
            return true;
        }

        // Simple glob-style matching
        if pattern.contains('*') {
            // SECURITY: Escape regex special characters before converting glob to regex
            // This prevents regex injection attacks via malformed glob patterns
            let escaped_pattern = regex::escape(pattern).replace(r"\*", ".*");
            if let Ok(regex) = regex::Regex::new(&escaped_pattern) {
                return regex.is_match(command);
            }
        }

        // Exact match or prefix match
        command == pattern || command.starts_with(&format!("{pattern} "))
    }

    /// Check if a file path matches a glob pattern
    fn path_matches_glob(&self, file_path: &str, glob_pattern: &str) -> bool {
        if glob_pattern == "all" || glob_pattern == "*" {
            return true;
        }

        // Use glob crate for proper glob matching
        if let Ok(pattern) = glob::Pattern::new(glob_pattern) {
            pattern.matches(file_path)
        } else {
            // Fallback to simple prefix/suffix matching
            if glob_pattern.starts_with('*') && glob_pattern.ends_with('*') {
                let middle = &glob_pattern[1..glob_pattern.len() - 1];
                file_path.contains(middle)
            } else if let Some(suffix) = glob_pattern.strip_prefix('*') {
                file_path.ends_with(suffix)
            } else if let Some(prefix) = glob_pattern.strip_suffix('*') {
                file_path.starts_with(prefix)
            } else {
                file_path == glob_pattern
            }
        }
    }

    /// Enhanced file safety check combining WCGW patterns
    pub fn validate_file_access(&mut self, file_path: &Path) -> Result<()> {
        let file_path_str = file_path.to_string_lossy();

        // Check mode permissions first
        if !self.is_file_edit_allowed(&file_path_str) {
            return Err(anyhow!(
                "File editing not allowed in {} mode for path: {}. Check your mode configuration.",
                match self.mode {
                    Modes::Wcgw => "wcgw",
                    Modes::Architect => "architect",
                    Modes::CodeWriter => "code_writer",
                },
                file_path.display()
            ));
        }

        // Check if file is whitelisted and has been read sufficiently
        if let Some(whitelist_data) = self.whitelist_for_overwrite.get(file_path_str.as_ref()) {
            if whitelist_data.needs_more_reading() {
                return Err(anyhow!(
                    "{}. Use ReadFiles tool to read more of the file first.",
                    whitelist_data.get_read_error_message(file_path)
                ));
            }

            // Check if file has been modified since last read by comparing file_hash
            // (content_hash is optional and may not be set, but file_hash is always set)
            if let Ok(current_content) = std::fs::read(file_path) {
                let mut hasher = Sha256::new();
                hasher.update(&current_content);
                let current_hash = format!("{:x}", hasher.finalize());
                if whitelist_data.file_hash != current_hash {
                    return Err(anyhow!(
                        "File {} has changed since last read. Please read the file again with ReadFiles before modifying.",
                        file_path.display()
                    ));
                }
            }
        } else {
            return Err(anyhow!(
                "File {} has not been read yet. You must read the file at least once using ReadFiles before editing it.",
                file_path.display()
            ));
        }

        Ok(())
    }

    /// Get mode-specific error message for unauthorized operations
    pub fn get_mode_violation_message(&self, operation: &str, target: &str) -> String {
        match self.mode {
            Modes::Wcgw => format!(
                "Unexpected error: {operation} should be allowed in wcgw mode"
            ),
            Modes::Architect => format!(
                "Operation '{operation}' not allowed in architect mode. Architect mode is read-only. \
                Use Initialize with mode_name=\"wcgw\" or \"code_writer\" to enable modifications."
            ),
            Modes::CodeWriter => format!(
                "Operation '{operation}' on '{target}' not allowed in code_writer mode. \
                Check your allowed_globs and allowed_commands configuration, or use Initialize \
                with mode_name=\"wcgw\" for full permissions."
            ),
        }
    }

    // ==================== State Persistence Methods ====================

    /// Save the current bash state to disk
    ///
    /// State is saved to `~/.local/share/wcgw/bash_state/{thread_id}_bash_state.json`
    /// Compatible with WCGW Python implementation.
    pub fn save_state_to_disk(&self) -> Result<()> {
        let snapshot = BashStateSnapshot::from_state(
            &self.cwd.to_string_lossy(),
            &self.workspace_root.to_string_lossy(),
            &self.mode,
            &self.bash_command_mode,
            &self.file_edit_mode,
            &self.write_if_empty_mode,
            &self.whitelist_for_overwrite,
            &self.current_thread_id,
        );

        save_state_file(&self.current_thread_id, &snapshot)?;
        debug!("Saved bash state to disk for thread_id '{}'", self.current_thread_id);
        Ok(())
    }

    /// Load bash state from disk for the given `thread_id`
    ///
    /// If state exists, it will be loaded into this `BashState` instance.
    /// Returns true if state was successfully loaded, false if no state exists.
    pub fn load_state_from_disk(&mut self, thread_id: &str) -> Result<bool> {
        if let Some(snapshot) = load_state_file(thread_id)? {
            let (
                cwd,
                workspace_root,
                mode,
                bash_command_mode,
                file_edit_mode,
                write_if_empty_mode,
                whitelist,
                loaded_thread_id,
            ) = snapshot.to_state_components();

            // Update state fields
            self.cwd = PathBuf::from(&cwd);
            self.workspace_root = PathBuf::from(&workspace_root);
            self.mode = mode;
            self.bash_command_mode = bash_command_mode;
            self.file_edit_mode = file_edit_mode;
            self.write_if_empty_mode = write_if_empty_mode;
            self.whitelist_for_overwrite = whitelist;
            self.current_thread_id = loaded_thread_id;
            self.initialized = true;

            // Re-initialize bash with new settings if needed
            if let Ok(mut bash_guard) = self.interactive_bash.lock() {
                if let Some(bash) = bash_guard.as_mut() {
                    // Ensure the bash process is in the right directory
                    if let Err(e) = bash.ensure_alive() {
                        warn!("Failed to ensure bash alive after loading state: {}", e);
                    }
                    // Change to loaded cwd
                    if let Err(e) = bash.send_command(&format!("cd \"{cwd}\"")) {
                        warn!("Failed to change directory after loading state: {}", e);
                    }
                    let _ = bash.read_output(0.5);
                }
            }

            info!("Loaded bash state from disk for thread_id '{}' (cwd: {})", thread_id, cwd);
            Ok(true)
        } else {
            debug!("No saved state found for thread_id '{}'", thread_id);
            Ok(false)
        }
    }

    /// Delete the saved state for this `thread_id` from disk
    pub fn delete_state_from_disk(&self) -> Result<()> {
        delete_state_file(&self.current_thread_id)?;
        info!("Deleted bash state from disk for thread_id '{}'", self.current_thread_id);
        Ok(())
    }

    /// Check if a saved state exists for the given `thread_id`
    pub fn has_saved_state(thread_id: &str) -> Result<bool> {
        Ok(load_state_file(thread_id)?.is_some())
    }

    /// Create a new `BashState` and load state from disk if available
    ///
    /// If `thread_id` is provided and state exists on disk, it will be loaded.
    /// Otherwise, a new state will be created.
    pub fn new_with_thread_id(thread_id: Option<&str>) -> Self {
        let mut state = Self::new();

        if let Some(tid) = thread_id {
            if !tid.is_empty() {
                match state.load_state_from_disk(tid) {
                    Ok(true) => {
                        info!("Created BashState with loaded state for thread_id '{}'", tid);
                    }
                    Ok(false) => {
                        // No saved state, just use the provided thread_id
                        state.current_thread_id = tid.to_string();
                        debug!("Created new BashState with thread_id '{}' (no saved state)", tid);
                    }
                    Err(e) => {
                        warn!("Failed to load state for thread_id '{}': {}", tid, e);
                        state.current_thread_id = tid.to_string();
                    }
                }
            }
        }

        state
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
pub fn generate_thread_id() -> String {
    let mut rng = rand::rng();
    format!("i{}", rng.random_range(1000..10000))
}
