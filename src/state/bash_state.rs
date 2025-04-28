use anyhow::{anyhow, Context as AnyhowContext, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::state::terminal::TerminalEmulator;
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
    #[allow(dead_code)]
    pub last_command: String,
    /// Last pending output, used for incremental updates
    #[allow(dead_code)]
    pub last_pending_output: String,
    /// Flag indicating if a command is currently running
    #[allow(dead_code)]
    pub command_running: bool,
    /// Terminal emulator for processing output
    #[allow(dead_code)]
    pub terminal_emulator: Arc<Mutex<TerminalEmulator>>,
}

impl TerminalState {
    /// Creates a new terminal state
    pub fn new() -> Self {
        Self {
            last_command: String::new(),
            last_pending_output: String::new(),
            command_running: false,
            terminal_emulator: Arc::new(Mutex::new(TerminalEmulator::new(160))),
        }
    }

    /// Process new output with the terminal emulator
    #[allow(dead_code)]
    pub fn process_output(&mut self, output: &str) -> String {
        // Update the last pending output
        self.last_pending_output = output.to_string();

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
    #[allow(dead_code)]
    pub whitelist_for_overwrite: HashMap<String, FileWhitelistData>,
    /// Terminal state for tracking command output
    #[allow(dead_code)]
    pub terminal_state: TerminalState,
}

/// BashContext wraps a BashState and provides access to it
#[allow(dead_code)]
pub struct BashContext {
    pub bash_state: BashState,
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
        }
    }

    /// Updates the current working directory
    pub fn update_cwd(&mut self, path: &Path) -> Result<()> {
        if path.exists() && path.is_dir() {
            self.cwd = path.to_path_buf();
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

    /// Adds file paths with line ranges to the whitelist for overwrite
    #[allow(dead_code)]
    pub fn add_to_whitelist_for_overwrite(
        &mut self,
        file_paths_with_ranges: HashMap<String, Vec<(usize, usize)>>,
    ) -> Result<()> {
        for (file_path, ranges) in file_paths_with_ranges {
            let file_content = fs::read(&file_path).context("Failed to read file")?;
            let file_hash = sha256_hash(&file_content);
            let total_lines = file_content.iter().filter(|&&c| c == b'\n').count() + 1;

            if let Some(whitelist_data) = self.whitelist_for_overwrite.get_mut(&file_path) {
                whitelist_data.file_hash = file_hash;
                whitelist_data.total_lines = total_lines;
                for (range_start, range_end) in ranges {
                    whitelist_data.add_range(range_start, range_end);
                }
            } else {
                self.whitelist_for_overwrite.insert(
                    file_path,
                    FileWhitelistData::new(file_hash, ranges, total_lines),
                );
            }
        }
        Ok(())
    }

    /// Executes a command in the current working directory
    #[allow(dead_code)]
    pub fn execute_command(&self, command: &str) -> Result<String> {
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("Failed to execute command")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(anyhow!("Command failed: {}\nStderr: {}", command, stderr));
        }

        Ok(format!("{}{}", stdout, stderr))
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
