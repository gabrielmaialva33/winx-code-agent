use crate::bash::{expand_user, generate_chat_id, Console, FileWhitelistData, SimpleConsole};
use crate::error::{WinxError, WinxResult};
use crate::types::Mode;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};

/// The current state of the bash process
pub enum BashStateStatus {
    /// The bash process is ready to accept commands
    Repl,

    /// The bash process is running a command
    Pending(DateTime<Utc>),
}

/// The mode of the bash process
pub enum BashMode {
    /// Normal mode with full access
    NormalMode,

    /// Restricted mode with limited access
    RestrictedMode,
}

/// Type of allowed commands
pub enum AllowedCommandsType {
    /// All commands are allowed
    All,

    /// No commands are allowed
    None,
}

/// Type of allowed globs
pub enum AllowedGlobsType {
    /// All globs are allowed
    All,

    /// Only specific globs are allowed
    Specific(Vec<String>),
}

/// Configuration for bash commands
pub struct BashCommandMode {
    /// The bash mode
    pub bash_mode: BashMode,

    /// The type of allowed commands
    pub allowed_commands: AllowedCommandsType,
}

/// Configuration for file editing
pub struct FileEditMode {
    /// The allowed globs for file editing
    pub allowed_globs: AllowedGlobsType,
}

/// Configuration for writing empty files
pub struct WriteIfEmptyMode {
    /// The allowed globs for writing empty files
    pub allowed_globs: AllowedGlobsType,
}

/// The state of the bash process
pub struct BashState {
    /// The console for output
    pub console: Box<dyn Console>,

    /// The current working directory
    pub cwd: String,

    /// The workspace root directory
    pub workspace_root: String,

    /// The bash command mode configuration
    pub bash_command_mode: BashCommandMode,

    /// The file edit mode configuration
    pub file_edit_mode: FileEditMode,

    /// The write if empty mode configuration
    pub write_if_empty_mode: WriteIfEmptyMode,

    /// The current mode
    pub mode: Mode,

    /// Files that are whitelisted for overwriting
    pub whitelist_for_overwrite: HashMap<String, FileWhitelistData>,

    /// The current chat ID
    pub current_chat_id: String,

    /// The bash process
    pub shell_process: Option<Child>,

    /// The current state of the bash process
    pub state: BashStateStatus,

    /// The last command executed
    pub last_command: String,

    /// The pending output from the last command
    pub pending_output: String,
}

// Constants
const PROMPT_CONST: &str = "wcgw ";
const PROMPT_STATEMENT: &str = "export GIT_PAGER=cat PAGER=cat PROMPT_COMMAND= PS1='wcgw'' '";

impl BashState {
    /// Create a new BashState
    pub fn new(
        console: Box<dyn Console>,
        working_dir: &str,
        bash_command_mode: Option<BashCommandMode>,
        file_edit_mode: Option<FileEditMode>,
        write_if_empty_mode: Option<WriteIfEmptyMode>,
        mode: Option<Mode>,
        chat_id: Option<String>,
    ) -> WinxResult<Self> {
        let cwd = if working_dir.is_empty() {
            std::env::current_dir()
                .map_err(|e| WinxError::Io(e))?
                .to_string_lossy()
                .to_string()
        } else {
            working_dir.to_string()
        };

        let bash_command_mode = bash_command_mode.unwrap_or(BashCommandMode {
            bash_mode: BashMode::NormalMode,
            allowed_commands: AllowedCommandsType::All,
        });

        let file_edit_mode = file_edit_mode.unwrap_or(FileEditMode {
            allowed_globs: AllowedGlobsType::All,
        });

        let write_if_empty_mode = write_if_empty_mode.unwrap_or(WriteIfEmptyMode {
            allowed_globs: AllowedGlobsType::All,
        });

        let mode = mode.unwrap_or(Mode::Wcgw);
        let current_chat_id = chat_id.unwrap_or_else(|| generate_chat_id());

        let mut state = BashState {
            console,
            cwd,
            workspace_root: working_dir.to_string(),
            bash_command_mode,
            file_edit_mode,
            write_if_empty_mode,
            mode,
            whitelist_for_overwrite: HashMap::new(),
            current_chat_id,
            shell_process: None,
            state: BashStateStatus::Repl,
            last_command: String::new(),
            pending_output: String::new(),
        };

        state.init_shell()?;

        Ok(state)
    }

    /// Initialize the bash shell
    pub fn init_shell(&mut self) -> WinxResult<()> {
        self.state = BashStateStatus::Repl;
        self.last_command = String::new();

        // Create the working directory if it doesn't exist
        fs::create_dir_all(&self.cwd).map_err(|e| WinxError::Io(e))?;

        // Start a new bash process
        let restricted_flag = match self.bash_command_mode.bash_mode {
            BashMode::RestrictedMode => "-r",
            BashMode::NormalMode => "",
        };

        let mut cmd = Command::new("bash");
        if !restricted_flag.is_empty() {
            cmd.arg(restricted_flag);
        }

        let mut shell_process = cmd
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PS1", PROMPT_CONST)
            .env("PROMPT_COMMAND", "")
            .env("GIT_PAGER", "cat")
            .env("PAGER", "cat")
            .spawn()
            .map_err(|e| WinxError::Io(e))?;

        // Set PS1 to a constant value for easier parsing
        if let Some(ref mut stdin) = shell_process.stdin {
            writeln!(stdin, "{}", PROMPT_STATEMENT).map_err(|e| WinxError::Io(e))?;
        }

        self.shell_process = Some(shell_process);

        Ok(())
    }

    /// Execute a command in the bash shell
    pub fn execute_command(&mut self, cmd: &str) -> WinxResult<String> {
        if let Some(ref mut process) = self.shell_process {
            if let Some(ref mut stdin) = process.stdin {
                writeln!(stdin, "{}", cmd).map_err(|e| WinxError::Io(e))?;
                self.last_command = cmd.to_string();
            }

            // Wait for the output
            let mut output = String::new();
            if let Some(ref mut stdout) = process.stdout {
                // Read with timeout (simplified for this implementation)
                // In a real implementation, we would need a more sophisticated approach
                // to read with timeout and handle ANSI escape sequences
                match stdout.read_to_string(&mut output) {
                    Ok(_) => (),
                    Err(e) => {
                        return Err(WinxError::Io(e));
                    }
                }
            }

            // Update the state
            self.state = BashStateStatus::Repl;

            return Ok(output);
        }

        Err(WinxError::ShellNotInitialized)
    }

    /// Add files to the whitelist for overwriting
    pub fn add_to_whitelist_for_overwrite(
        &mut self,
        file_paths_with_ranges: HashMap<String, Vec<(usize, usize)>>,
    ) -> WinxResult<()> {
        for (file_path, ranges) in file_paths_with_ranges {
            let file_content = fs::read(&file_path).map_err(|e| WinxError::Io(e))?;
            let file_hash = format!("{:x}", Sha256::digest(&file_content));
            let total_lines = file_content.iter().filter(|&&b| b == b'\n').count() + 1;

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

    /// Get the status of the bash shell
    pub fn get_status(&self) -> String {
        let mut status = "\n\n---\n\n".to_string();

        match self.state {
            BashStateStatus::Pending(timestamp) => {
                let now = Utc::now();
                let duration = now.signed_duration_since(timestamp);
                status.push_str(&format!("status = still running\n"));
                status.push_str(&format!(
                    "running for = {} seconds\n",
                    duration.num_seconds()
                ));
                status.push_str(&format!("cwd = {}\n", self.cwd));
            }
            BashStateStatus::Repl => {
                // In a real implementation, we would check for background jobs here
                status.push_str("status = process exited\n");
                status.push_str(&format!("cwd = {}\n", self.cwd));
            }
        }

        status.trim_end().to_string()
    }

    /// Update the current working directory
    pub fn update_cwd(&mut self) -> WinxResult<String> {
        let output = self.execute_command("pwd")?;
        let current_dir = output.trim().to_string();
        self.cwd = current_dir.clone();
        Ok(current_dir)
    }
}
