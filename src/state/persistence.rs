//! Bash state persistence module
//!
//! Provides disk persistence for `BashState`, compatible with WCGW Python implementation.
//! State is stored in `~/.local/share/wcgw/bash_state/` as JSON files.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::{debug, info, warn};

use crate::types::{
    AllowedCommands, AllowedGlobs, BashCommandMode, BashMode, FileEditMode, Modes, WriteIfEmptyMode,
};

use super::bash_state::FileWhitelistData;

/// Snapshot of `BashState` that can be serialized to disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashStateSnapshot {
    pub bash_command_mode: BashCommandModeSnapshot,
    pub file_edit_mode: FileEditModeSnapshot,
    pub write_if_empty_mode: WriteIfEmptyModeSnapshot,
    pub whitelist_for_overwrite: HashMap<String, FileWhitelistDataSnapshot>,
    pub mode: String,
    pub workspace_root: String,
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashCommandModeSnapshot {
    pub bash_mode: String,
    pub allowed_commands: AllowedCommandsSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AllowedCommandsSnapshot {
    All(String),
    List(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEditModeSnapshot {
    pub allowed_globs: AllowedGlobsSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteIfEmptyModeSnapshot {
    pub allowed_globs: AllowedGlobsSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AllowedGlobsSnapshot {
    All(String),
    List(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWhitelistDataSnapshot {
    pub file_hash: String,
    pub line_ranges_read: Vec<(usize, usize)>,
    pub total_lines: usize,
}

pub fn get_state_dir() -> Result<PathBuf> {
    let home = home::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    let bash_state_dir = home.join(".local/share/wcgw/bash_state");
    if !bash_state_dir.exists() {
        fs::create_dir_all(&bash_state_dir)?;
    }
    Ok(bash_state_dir)
}

fn get_state_file_path(thread_id: &str) -> Result<PathBuf> {
    Ok(get_state_dir()?.join(format!("{thread_id}_bash_state.json")))
}

pub fn save_bash_state(thread_id: &str, state: &BashStateSnapshot) -> Result<()> {
    if thread_id.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_string_pretty(state)?;
    fs::write(get_state_file_path(thread_id)?, json)?;
    Ok(())
}

pub fn load_bash_state(thread_id: &str) -> Result<Option<BashStateSnapshot>> {
    if thread_id.is_empty() {
        return Ok(None);
    }
    let path = get_state_file_path(thread_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let json = fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&json)?))
}

pub fn delete_bash_state(thread_id: &str) -> Result<()> {
    if thread_id.is_empty() {
        return Ok(());
    }
    let path = get_state_file_path(thread_id)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

impl BashStateSnapshot {
    pub fn from_state(
        cwd: &str,
        workspace_root: &str,
        mode: &Modes,
        bash_command_mode: &BashCommandMode,
        file_edit_mode: &FileEditMode,
        write_if_empty_mode: &WriteIfEmptyMode,
        whitelist: &HashMap<String, FileWhitelistData>,
        thread_id: &str,
    ) -> Self {
        Self {
            bash_command_mode: BashCommandModeSnapshot::from(bash_command_mode),
            file_edit_mode: FileEditModeSnapshot::from(file_edit_mode),
            write_if_empty_mode: WriteIfEmptyModeSnapshot::from(write_if_empty_mode),
            whitelist_for_overwrite: whitelist
                .iter()
                .map(|(k, v)| (k.clone(), FileWhitelistDataSnapshot::from(v)))
                .collect(),
            mode: mode.to_string(),
            workspace_root: workspace_root.to_string(),
            chat_id: thread_id.to_string(),
            cwd: if cwd == workspace_root { String::new() } else { cwd.to_string() },
        }
    }

    pub fn to_state_components(
        &self,
    ) -> (
        String,
        String,
        Modes,
        BashCommandMode,
        FileEditMode,
        WriteIfEmptyMode,
        HashMap<String, FileWhitelistData>,
        String,
    ) {
        let mode = match self.mode.as_str() {
            "architect" => Modes::Architect,
            "code_writer" => Modes::CodeWriter,
            _ => Modes::Wcgw,
        };
        let whitelist = self
            .whitelist_for_overwrite
            .iter()
            .map(|(k, v)| (k.clone(), v.to_whitelist_data()))
            .collect();
        let cwd = if self.cwd.is_empty() { self.workspace_root.clone() } else { self.cwd.clone() };
        (
            cwd,
            self.workspace_root.clone(),
            mode,
            self.bash_command_mode.to_bash_command_mode(),
            self.file_edit_mode.to_file_edit_mode(),
            self.write_if_empty_mode.to_write_if_empty_mode(),
            whitelist,
            self.chat_id.clone(),
        )
    }
}

impl From<&BashCommandMode> for BashCommandModeSnapshot {
    fn from(mode: &BashCommandMode) -> Self {
        Self {
            bash_mode: match mode.bash_mode {
                BashMode::RestrictedMode => "restricted_mode".to_string(),
                _ => "normal_mode".to_string(),
            },
            allowed_commands: match &mode.allowed_commands {
                AllowedCommands::All(s) => AllowedCommandsSnapshot::All(s.clone()),
                AllowedCommands::List(l) => AllowedCommandsSnapshot::List(l.clone()),
            },
        }
    }
}

impl BashCommandModeSnapshot {
    fn to_bash_command_mode(&self) -> BashCommandMode {
        BashCommandMode {
            bash_mode: if self.bash_mode == "restricted_mode" {
                BashMode::RestrictedMode
            } else {
                BashMode::NormalMode
            },
            allowed_commands: match &self.allowed_commands {
                AllowedCommandsSnapshot::All(s) => AllowedCommands::All(s.clone()),
                AllowedCommandsSnapshot::List(l) => AllowedCommands::List(l.clone()),
            },
        }
    }
}

impl From<&FileEditMode> for FileEditModeSnapshot {
    fn from(mode: &FileEditMode) -> Self {
        Self {
            allowed_globs: match &mode.allowed_globs {
                AllowedGlobs::All(s) => AllowedGlobsSnapshot::All(s.clone()),
                AllowedGlobs::List(l) => AllowedGlobsSnapshot::List(l.clone()),
            },
        }
    }
}

impl FileEditModeSnapshot {
    fn to_file_edit_mode(&self) -> FileEditMode {
        FileEditMode {
            allowed_globs: match &self.allowed_globs {
                AllowedGlobsSnapshot::All(s) => AllowedGlobs::All(s.clone()),
                AllowedGlobsSnapshot::List(l) => AllowedGlobs::List(l.clone()),
            },
        }
    }
}

impl From<&WriteIfEmptyMode> for WriteIfEmptyModeSnapshot {
    fn from(mode: &WriteIfEmptyMode) -> Self {
        Self {
            allowed_globs: match &mode.allowed_globs {
                AllowedGlobs::All(s) => AllowedGlobsSnapshot::All(s.clone()),
                AllowedGlobs::List(l) => AllowedGlobsSnapshot::List(l.clone()),
            },
        }
    }
}

impl WriteIfEmptyModeSnapshot {
    fn to_write_if_empty_mode(&self) -> WriteIfEmptyMode {
        WriteIfEmptyMode {
            allowed_globs: match &self.allowed_globs {
                AllowedGlobsSnapshot::All(s) => AllowedGlobs::All(s.clone()),
                AllowedGlobsSnapshot::List(l) => AllowedGlobs::List(l.clone()),
            },
        }
    }
}

impl From<&FileWhitelistData> for FileWhitelistDataSnapshot {
    fn from(d: &FileWhitelistData) -> Self {
        Self {
            file_hash: d.file_hash.clone(),
            line_ranges_read: d.line_ranges_read.clone(),
            total_lines: d.total_lines,
        }
    }
}

impl FileWhitelistDataSnapshot {
    fn to_whitelist_data(&self) -> FileWhitelistData {
        FileWhitelistData {
            file_hash: self.file_hash.clone(),
            line_ranges_read: self.line_ranges_read.clone(),
            total_lines: self.total_lines,
        }
    }
}
