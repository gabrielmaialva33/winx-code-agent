#![allow(clippy::unwrap_used)]
use anyhow::Result;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;
use tracing::info;

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
    pub terminal_emulator: Arc<StdMutex<TerminalEmulator>>,
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
            terminal_emulator: Arc::new(StdMutex::new(TerminalEmulator::new(160))),
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
    pub pty_shell: Arc<Mutex<Option<PtyShell>>>,
    pub initialized: bool,
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
            pty_shell: Arc::new(Mutex::new(None)),
            initialized: false,
        }
    }

    pub async fn init_pty_shell(&mut self) -> Result<()> {
        let shell =
            PtyShell::new(&self.cwd, self.bash_command_mode.bash_mode == BashMode::RestrictedMode)?;
        *self.pty_shell.lock().await = Some(shell);
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

    pub fn is_command_allowed(&self, command: &str) -> bool {
        self.bash_command_mode.allowed_commands.is_allowed(command)
    }

    pub fn is_file_edit_allowed(&self, path: &str) -> bool {
        self.file_edit_mode.allowed_globs.is_allowed(path)
    }

    pub fn is_file_write_allowed(&self, path: &str) -> bool {
        self.write_if_empty_mode.allowed_globs.is_allowed(path)
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

    pub fn new_with_thread_id(thread_id: Option<&str>) -> Self {
        let mut state = Self::new();

        if let Some(tid) = thread_id {
            if !tid.is_empty() {
                if let Ok(true) = state.load_state_from_disk(tid) {
                    info!("Loaded state for thread_id '{}'", tid);
                } else {
                    state.current_thread_id = tid.to_string();
                }
            }
        }

        state
    }
}

pub fn generate_thread_id() -> String {
    let mut rng = rand::rng();
    format!("tid_{:x}", rng.random::<u64>())
}
