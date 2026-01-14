#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
use anyhow::{anyhow, Context as AnyhowContext, Result};
use glob;
use lazy_static::lazy_static;
use rand::{Rng, RngCore};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use crate::state::persistence::{
    delete_bash_state as delete_state_file, load_bash_state as load_state_file,
    save_bash_state as save_state_file, BashStateSnapshot,
};
use crate::state::pty::PtyShell;
use crate::state::terminal::{
    incremental_text, TerminalEmulator, TerminalOutputDiff, DEFAULT_MAX_SCREEN_LINES,
    MAX_OUTPUT_SIZE as TERMINAL_MAX_OUTPUT_SIZE,
};
use crate::types::{
    AllowedCommands, AllowedGlobs, BashCommandMode, BashMode, FileEditMode, Modes, WriteIfEmptyMode,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWhitelistData {
    pub file_hash: String,
    pub line_ranges_read: Vec<(usize, usize)>,
    pub total_lines: usize,
}

impl FileWhitelistData {
    pub fn new(
        file_hash: String,
        line_ranges_read: Vec<(usize, usize)>,
        total_lines: usize,
    ) -> Self {
        Self { file_hash, line_ranges_read, total_lines }
    }

    pub fn is_read_enough(&self) -> bool {
        self.get_percentage_read() >= 99.0
    }

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
        let mut unread = vec![];
        let mut start_range = None;
        for i in 1..=self.total_lines {
            if !lines_read.contains(&i) {
                if start_range.is_none() {
                    start_range = Some(i);
                }
            } else if let Some(start) = start_range {
                unread.push((start, i - 1));
                start_range = None;
            }
        }
        if let Some(start) = start_range {
            unread.push((start, self.total_lines));
        }
        unread
    }

    pub fn add_range(&mut self, start: usize, end: usize) {
        self.line_ranges_read.push((start, end));
    }

    pub fn get_read_error_message(&self, file_path: &Path) -> String {
        format!(
            "File {} needs more reading. Coverage: {:.1}%",
            file_path.display(),
            self.get_percentage_read()
        )
    }

    pub fn needs_more_reading(&self) -> bool {
        !self.is_read_enough()
    }
}

#[derive(Debug, Clone)]
pub struct TerminalState {
    pub last_command: String,
    pub last_pending_output: String,
    pub command_running: bool,
    pub terminal_emulator: Arc<Mutex<TerminalEmulator>>,
    pub diff_detector: Option<TerminalOutputDiff>,
    pub limit_buffer: bool,
    pub max_buffer_lines: usize,
}

impl Default for TerminalState {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalState {
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

    pub fn process_output(&mut self, output: &str) -> String {
        self.last_pending_output = output.to_string();
        if let Ok(mut emulator) = self.terminal_emulator.lock() {
            emulator.process(output);
            emulator.display().join("\n")
        } else {
            output.to_string()
        }
    }

    pub fn get_incremental_output(&mut self, output: &str) -> String {
        let result = incremental_text(output, &self.last_pending_output);
        self.last_pending_output = output.to_string();
        result
    }

    pub fn smart_truncate(&mut self, max_size: usize) {
        if let Ok(screen) = self.terminal_emulator.lock() {
            if let Ok(mut screen_guard) = screen.get_screen().lock() {
                screen_guard.smart_truncate(max_size);
            }
        }
    }
}

const WCGW_PROMPT_PATTERN: &str = r"◉ ([^\n]*)──➤";
const WCGW_PROMPT_COMMAND: &str = r#"printf '◉ "$(pwd)"──➤ '"#;
const BASH_PROMPT_STATEMENT: &str =
    r#"export GIT_PAGER=cat PAGER=cat PROMPT_COMMAND='printf \"◉ $(pwd)──➤ \"'"#;

lazy_static! {
    static ref PROMPT_REGEX: Regex = Regex::new(WCGW_PROMPT_PATTERN).expect("Invalid prompt regex");
}

fn contains_wcgw_prompt(text: &str) -> bool {
    PROMPT_REGEX.is_match(text)
}

const MAX_OUTPUT_SIZE: usize = 1_000_000;
const MAX_COMMAND_TIMEOUT: f32 = 60.0;
const DEFAULT_BUFFER_SIZE: usize = 8192;

#[derive(Debug, Clone, PartialEq)]
pub enum CommandState {
    Idle,
    Running { start_time: std::time::SystemTime, command: String },
}

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
    pub terminal_state: TerminalState,
    pub interactive_bash: Arc<Mutex<Option<InteractiveBash>>>,
    pub pty_shell: Arc<Mutex<Option<PtyShell>>>,
    pub initialized: bool,
}

#[derive(Debug)]
pub struct InteractiveBash {
    pub process: Child,
    pub last_command: String,
    pub last_output: String,
    pub output_buffer: String,
    pub command_state: CommandState,
    pub max_output_size: usize,
    pub output_truncated: bool,
    pub output_chunks: Vec<String>,
    initial_dir: PathBuf,
    restricted_mode: bool,
}

impl InteractiveBash {
    pub fn is_alive(&mut self) -> bool {
        matches!(self.process.try_wait(), Ok(None))
    }

    pub fn reinit(&mut self) -> Result<()> {
        let mut cmd = Command::new("bash");
        if self.restricted_mode {
            cmd.arg("-r");
        }
        let mut process = cmd
            .env("PAGER", "cat")
            .env("GIT_PAGER", "cat")
            .env("PROMPT_COMMAND", WCGW_PROMPT_COMMAND)
            .env("TERM", "xterm-256color")
            .current_dir(&self.initial_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = process.stdin.take().ok_or_else(|| anyhow!("No stdin"))?;
        writeln!(stdin, "{BASH_PROMPT_STATEMENT}")?;
        stdin.flush()?;
        process.stdin = Some(stdin);
        self.process = process;
        self.command_state = CommandState::Idle;
        Ok(())
    }

    pub fn ensure_alive(&mut self) -> Result<()> {
        if !self.is_alive() {
            self.reinit()?;
        }
        Ok(())
    }

    pub fn new(initial_dir: &Path, restricted_mode: bool) -> Result<Self> {
        let mut cmd = Command::new("bash");
        if restricted_mode {
            cmd.arg("-r");
        }
        let mut process = cmd
            .env("PAGER", "cat")
            .env("GIT_PAGER", "cat")
            .env("PROMPT_COMMAND", WCGW_PROMPT_COMMAND)
            .env("TERM", "xterm-256color")
            .current_dir(initial_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = process.stdin.take().ok_or_else(|| anyhow!("No stdin"))?;
        writeln!(stdin, "{BASH_PROMPT_STATEMENT}")?;
        stdin.flush()?;
        process.stdin = Some(stdin);
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

    pub fn send_command(&mut self, command: &str) -> Result<()> {
        self.ensure_alive()?;
        let mut stdin = self.process.stdin.take().ok_or_else(|| anyhow!("No stdin"))?;
        writeln!(stdin, "{command}")?;
        stdin.flush()?;
        self.process.stdin = Some(stdin);
        self.last_command = command.to_string();
        self.command_state = CommandState::Running {
            start_time: std::time::SystemTime::now(),
            command: command.to_string(),
        };
        Ok(())
    }

    pub fn read_output(&mut self, timeout_secs: f32) -> Result<(String, bool)> {
        let timeout = Duration::from_secs_f32(timeout_secs.clamp(0.1, MAX_COMMAND_TIMEOUT));
        let start = Instant::now();
        let mut new_output = String::new();
        let mut complete = false;
        let mut full_output = self.last_output.clone();

        while start.elapsed() < timeout {
            let mut buf = vec![0; DEFAULT_BUFFER_SIZE];
            if let Some(stdout) = self.process.stdout.as_mut() {
                if let Ok(n) = stdout.read(&mut buf) {
                    if n > 0 {
                        let chunk = String::from_utf8_lossy(&buf[..n]);
                        full_output.push_str(&chunk);
                        new_output.push_str(&chunk);
                        if contains_wcgw_prompt(&full_output) {
                            complete = true;
                            break;
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        if complete {
            self.command_state = CommandState::Idle;
        }
        self.last_output = full_output.clone();
        Ok((full_output, complete))
    }

    pub fn send_interrupt(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            let pid = self.process.id() as i32;
            unsafe {
                libc::kill(pid, libc::SIGINT);
            }
        }
        Ok(())
    }
}

impl Default for BashState {
    fn default() -> Self {
        Self::new()
    }
}

impl BashState {
    pub fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
        Self {
            cwd: cwd.clone(),
            workspace_root: cwd,
            current_thread_id: generate_thread_id(),
            mode: Modes::Wcgw,
            bash_command_mode: BashCommandMode {
                bash_mode: BashMode::NormalMode,
                allowed_commands: AllowedCommands::All("all".to_string()),
            },
            file_edit_mode: FileEditMode { allowed_globs: AllowedGlobs::All("all".to_string()) },
            write_if_empty_mode: WriteIfEmptyMode {
                allowed_globs: AllowedGlobs::All("all".to_string()),
            },
            whitelist_for_overwrite: HashMap::new(),
            terminal_state: TerminalState::new(),
            interactive_bash: Arc::new(Mutex::new(None)),
            pty_shell: Arc::new(Mutex::new(None)),
            initialized: false,
        }
    }

    pub fn init_interactive_bash(&mut self) -> Result<()> {
        let bash = InteractiveBash::new(
            &self.cwd,
            self.bash_command_mode.bash_mode == BashMode::RestrictedMode,
        )?;
        *self.interactive_bash.lock().unwrap() = Some(bash);
        Ok(())
    }

    pub fn update_cwd(&mut self, path: &Path) -> Result<()> {
        self.cwd = path.to_path_buf();
        Ok(())
    }

    pub fn update_workspace_root(&mut self, path: &Path) -> Result<()> {
        self.workspace_root = path.to_path_buf();
        Ok(())
    }

    pub fn is_command_allowed(&self, _command: &str) -> bool {
        true
    }
    pub fn is_file_edit_allowed(&self, _path: &str) -> bool {
        true
    }
    pub fn is_file_write_allowed(&self, _path: &str) -> bool {
        true
    }
    pub fn get_mode_violation_message(&self, op: &str, _target: &str) -> String {
        format!("Operation {op} not allowed")
    }

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
        Ok(())
    }

    pub fn load_state_from_disk(&mut self, thread_id: &str) -> Result<bool> {
        if let Some(snapshot) = load_state_file(thread_id)? {
            let (cwd, root, mode, bmode, emode, wmode, whitelist, tid) =
                snapshot.to_state_components();
            self.cwd = PathBuf::from(cwd);
            self.workspace_root = PathBuf::from(root);
            self.mode = mode;
            self.bash_command_mode = bmode;
            self.file_edit_mode = emode;
            self.write_if_empty_mode = wmode;
            self.whitelist_for_overwrite = whitelist;
            self.current_thread_id = tid;
            self.initialized = true;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

pub fn generate_thread_id() -> String {
    let mut rng = rand::rng();
    format!("tid_{:x}", rng.next_u64())
}
