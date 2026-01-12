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

use crate::types::{AllowedCommands, AllowedGlobs, BashCommandMode, BashMode, FileEditMode, Modes, WriteIfEmptyMode};

use super::bash_state::FileWhitelistData;

/// Snapshot of `BashState` that can be serialized to disk
/// Compatible with WCGW Python's bash state format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashStateSnapshot {
    /// Bash command mode configuration
    pub bash_command_mode: BashCommandModeSnapshot,
    /// File edit mode configuration
    pub file_edit_mode: FileEditModeSnapshot,
    /// Write if empty mode configuration
    pub write_if_empty_mode: WriteIfEmptyModeSnapshot,
    /// Whitelist for file overwriting
    pub whitelist_for_overwrite: HashMap<String, FileWhitelistDataSnapshot>,
    /// Operation mode (wcgw, architect, `code_writer`)
    pub mode: String,
    /// Workspace root directory
    pub workspace_root: String,
    /// Thread/chat ID for this state
    pub chat_id: String,
    /// Current working directory (optional for backward compatibility with wcgw Python)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
}

/// Serializable version of `BashCommandMode`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashCommandModeSnapshot {
    pub bash_mode: String,
    pub allowed_commands: AllowedCommandsSnapshot,
}

/// Serializable version of `AllowedCommands`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AllowedCommandsSnapshot {
    All(String),
    List(Vec<String>),
}

/// Serializable version of `FileEditMode`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEditModeSnapshot {
    pub allowed_globs: AllowedGlobsSnapshot,
}

/// Serializable version of `WriteIfEmptyMode`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteIfEmptyModeSnapshot {
    pub allowed_globs: AllowedGlobsSnapshot,
}

/// Serializable version of `AllowedGlobs`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AllowedGlobsSnapshot {
    All(String),
    List(Vec<String>),
}

/// Serializable version of `FileWhitelistData`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWhitelistDataSnapshot {
    pub file_hash: String,
    pub line_ranges_read: Vec<(usize, usize)>,
    pub total_lines: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_read_percentage: Option<f64>,
}

/// Get the XDG data directory for storing bash state
/// Returns `~/.local/share/wcgw/bash_state/`
pub fn get_state_dir() -> Result<PathBuf> {
    let data_dir = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = home::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
            home.join(".local/share")
        });

    let bash_state_dir = data_dir.join("wcgw").join("bash_state");

    // Create directory if it doesn't exist
    if !bash_state_dir.exists() {
        fs::create_dir_all(&bash_state_dir)
            .with_context(|| format!("Failed to create bash state directory: {bash_state_dir:?}"))?;
        debug!("Created bash state directory: {:?}", bash_state_dir);
    }

    Ok(bash_state_dir)
}

/// Get the path to the state file for a given thread ID
fn get_state_file_path(thread_id: &str) -> Result<PathBuf> {
    let state_dir = get_state_dir()?;
    Ok(state_dir.join(format!("{thread_id}_bash_state.json")))
}

/// Save bash state to disk
///
/// # Arguments
/// * `thread_id` - The thread/chat ID to save state for
/// * `state` - The `BashStateSnapshot` to save
///
/// # Returns
/// * `Ok(())` if the state was saved successfully
/// * `Err` if there was an error saving the state
pub fn save_bash_state(thread_id: &str, state: &BashStateSnapshot) -> Result<()> {
    if thread_id.is_empty() {
        warn!("Attempted to save bash state with empty thread_id");
        return Ok(());
    }

    let state_file = get_state_file_path(thread_id)?;

    let json = serde_json::to_string_pretty(state)
        .with_context(|| "Failed to serialize bash state to JSON")?;

    fs::write(&state_file, json)
        .with_context(|| format!("Failed to write bash state to file: {state_file:?}"))?;

    debug!("Saved bash state for thread_id '{}' to {:?}", thread_id, state_file);
    Ok(())
}

/// Load bash state from disk
///
/// # Arguments
/// * `thread_id` - The thread/chat ID to load state for
///
/// # Returns
/// * `Ok(Some(state))` if the state was loaded successfully
/// * `Ok(None)` if no state file exists for the given thread ID
/// * `Err` if there was an error loading the state
pub fn load_bash_state(thread_id: &str) -> Result<Option<BashStateSnapshot>> {
    if thread_id.is_empty() {
        return Ok(None);
    }

    let state_file = get_state_file_path(thread_id)?;

    if !state_file.exists() {
        debug!("No saved state found for thread_id '{}'", thread_id);
        return Ok(None);
    }

    let json = fs::read_to_string(&state_file)
        .with_context(|| format!("Failed to read bash state from file: {state_file:?}"))?;

    let state: BashStateSnapshot = serde_json::from_str(&json)
        .with_context(|| format!("Failed to parse bash state JSON from file: {state_file:?}"))?;

    info!("Loaded bash state for thread_id '{}' from {:?}", thread_id, state_file);
    Ok(Some(state))
}

/// Delete bash state from disk
///
/// # Arguments
/// * `thread_id` - The thread/chat ID to delete state for
///
/// # Returns
/// * `Ok(())` if the state was deleted successfully or didn't exist
/// * `Err` if there was an error deleting the state
pub fn delete_bash_state(thread_id: &str) -> Result<()> {
    if thread_id.is_empty() {
        return Ok(());
    }

    let state_file = get_state_file_path(thread_id)?;

    if state_file.exists() {
        fs::remove_file(&state_file)
            .with_context(|| format!("Failed to delete bash state file: {state_file:?}"))?;
        info!("Deleted bash state for thread_id '{}' from {:?}", thread_id, state_file);
    }

    Ok(())
}

/// List all saved bash state thread IDs
///
/// # Returns
/// * `Ok(Vec<String>)` - List of thread IDs with saved states
/// * `Err` if there was an error reading the state directory
pub fn list_saved_states() -> Result<Vec<String>> {
    let state_dir = get_state_dir()?;
    let mut thread_ids = Vec::new();

    if let Ok(entries) = fs::read_dir(&state_dir) {
        for entry in entries.flatten() {
            if let Some(file_name) = entry.file_name().to_str() {
                if file_name.ends_with("_bash_state.json") {
                    let thread_id = file_name.trim_end_matches("_bash_state.json").to_string();
                    thread_ids.push(thread_id);
                }
            }
        }
    }

    debug!("Found {} saved bash states", thread_ids.len());
    Ok(thread_ids)
}

// Conversion implementations

impl BashStateSnapshot {
    /// Create a snapshot from `BashState` components
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
            // Only store cwd if different from workspace_root (for backward compatibility)
            cwd: if cwd == workspace_root {
                String::new()
            } else {
                cwd.to_string()
            },
        }
    }

    /// Convert snapshot back to `BashState` components
    pub fn to_state_components(
        &self,
    ) -> (
        String,                                    // cwd
        String,                                    // workspace_root
        Modes,                                     // mode
        BashCommandMode,                           // bash_command_mode
        FileEditMode,                              // file_edit_mode
        WriteIfEmptyMode,                          // write_if_empty_mode
        HashMap<String, FileWhitelistData>,        // whitelist
        String,                                    // thread_id
    ) {
        let mode = match self.mode.as_str() {
            "wcgw" => Modes::Wcgw,
            "architect" => Modes::Architect,
            "code_writer" => Modes::CodeWriter,
            _ => Modes::Wcgw,
        };

        let whitelist = self
            .whitelist_for_overwrite
            .iter()
            .map(|(k, v)| (k.clone(), v.to_whitelist_data()))
            .collect();

        // Use cwd if present, otherwise fall back to workspace_root
        let cwd = if self.cwd.is_empty() {
            self.workspace_root.clone()
        } else {
            self.cwd.clone()
        };

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
                BashMode::NormalMode => "normal_mode".to_string(),
                BashMode::RestrictedMode => "restricted_mode".to_string(),
            },
            allowed_commands: match &mode.allowed_commands {
                AllowedCommands::All(s) => AllowedCommandsSnapshot::All(s.clone()),
                AllowedCommands::List(list) => AllowedCommandsSnapshot::List(list.clone()),
            },
        }
    }
}

impl BashCommandModeSnapshot {
    fn to_bash_command_mode(&self) -> BashCommandMode {
        BashCommandMode {
            bash_mode: match self.bash_mode.as_str() {
                "restricted_mode" => BashMode::RestrictedMode,
                _ => BashMode::NormalMode,
            },
            allowed_commands: match &self.allowed_commands {
                AllowedCommandsSnapshot::All(s) => AllowedCommands::All(s.clone()),
                AllowedCommandsSnapshot::List(list) => AllowedCommands::List(list.clone()),
            },
        }
    }
}

impl From<&FileEditMode> for FileEditModeSnapshot {
    fn from(mode: &FileEditMode) -> Self {
        Self {
            allowed_globs: match &mode.allowed_globs {
                AllowedGlobs::All(s) => AllowedGlobsSnapshot::All(s.clone()),
                AllowedGlobs::List(list) => AllowedGlobsSnapshot::List(list.clone()),
            },
        }
    }
}

impl FileEditModeSnapshot {
    fn to_file_edit_mode(&self) -> FileEditMode {
        FileEditMode {
            allowed_globs: match &self.allowed_globs {
                AllowedGlobsSnapshot::All(s) => AllowedGlobs::All(s.clone()),
                AllowedGlobsSnapshot::List(list) => AllowedGlobs::List(list.clone()),
            },
        }
    }
}

impl From<&WriteIfEmptyMode> for WriteIfEmptyModeSnapshot {
    fn from(mode: &WriteIfEmptyMode) -> Self {
        Self {
            allowed_globs: match &mode.allowed_globs {
                AllowedGlobs::All(s) => AllowedGlobsSnapshot::All(s.clone()),
                AllowedGlobs::List(list) => AllowedGlobsSnapshot::List(list.clone()),
            },
        }
    }
}

impl WriteIfEmptyModeSnapshot {
    fn to_write_if_empty_mode(&self) -> WriteIfEmptyMode {
        WriteIfEmptyMode {
            allowed_globs: match &self.allowed_globs {
                AllowedGlobsSnapshot::All(s) => AllowedGlobs::All(s.clone()),
                AllowedGlobsSnapshot::List(list) => AllowedGlobs::List(list.clone()),
            },
        }
    }
}

impl From<&FileWhitelistData> for FileWhitelistDataSnapshot {
    fn from(data: &FileWhitelistData) -> Self {
        Self {
            file_hash: data.file_hash.clone(),
            line_ranges_read: data.line_ranges_read.clone(),
            total_lines: data.total_lines,
            content_hash: data.content_hash.clone(),
            min_read_percentage: Some(data.min_read_percentage),
        }
    }
}

impl FileWhitelistDataSnapshot {
    fn to_whitelist_data(&self) -> FileWhitelistData {
        FileWhitelistData {
            file_hash: self.file_hash.clone(),
            line_ranges_read: self.line_ranges_read.clone(),
            total_lines: self.total_lines,
            content_hash: self.content_hash.clone(),
            last_read_time: None,
            modified_since_read: false,
            min_read_percentage: self.min_read_percentage.unwrap_or(99.0),
        }
    }
}

impl std::fmt::Display for Modes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Modes::Wcgw => write!(f, "wcgw"),
            Modes::Architect => write!(f, "architect"),
            Modes::CodeWriter => write!(f, "code_writer"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_dir_creation() {
        let dir = get_state_dir();
        assert!(dir.is_ok());
    }

    #[test]
    fn test_snapshot_serialization() {
        let snapshot = BashStateSnapshot {
            bash_command_mode: BashCommandModeSnapshot {
                bash_mode: "normal_mode".to_string(),
                allowed_commands: AllowedCommandsSnapshot::All("all".to_string()),
            },
            file_edit_mode: FileEditModeSnapshot {
                allowed_globs: AllowedGlobsSnapshot::All("all".to_string()),
            },
            write_if_empty_mode: WriteIfEmptyModeSnapshot {
                allowed_globs: AllowedGlobsSnapshot::All("all".to_string()),
            },
            whitelist_for_overwrite: HashMap::new(),
            mode: "wcgw".to_string(),
            workspace_root: "/home/test".to_string(),
            chat_id: "i1234".to_string(),
            cwd: "/tmp".to_string(),
        };

        let json = serde_json::to_string(&snapshot);
        assert!(json.is_ok());

        let parsed: Result<BashStateSnapshot, _> = serde_json::from_str(&json.unwrap());
        assert!(parsed.is_ok());
    }

    #[test]
    fn test_round_trip_conversion() {
        let cwd = "/test/dir";
        let workspace_root = "/test";
        let mode = Modes::Wcgw;
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
        let whitelist = HashMap::new();
        let thread_id = "i5678";

        let snapshot = BashStateSnapshot::from_state(
            cwd,
            workspace_root,
            &mode,
            &bash_command_mode,
            &file_edit_mode,
            &write_if_empty_mode,
            &whitelist,
            thread_id,
        );

        let (
            restored_cwd,
            restored_workspace_root,
            restored_mode,
            _restored_bash_command_mode,
            _restored_file_edit_mode,
            _restored_write_if_empty_mode,
            _restored_whitelist,
            restored_thread_id,
        ) = snapshot.to_state_components();

        assert_eq!(cwd, restored_cwd);
        assert_eq!(workspace_root, restored_workspace_root);
        assert!(matches!(restored_mode, Modes::Wcgw));
        assert_eq!(thread_id, restored_thread_id);
    }

    #[test]
    fn test_wcgw_python_compatibility() {
        // Test parsing WCGW Python format (without cwd field)
        let wcgw_json = r#"{
            "bash_command_mode": {
                "bash_mode": "normal_mode",
                "allowed_commands": "all"
            },
            "file_edit_mode": {
                "allowed_globs": "all"
            },
            "write_if_empty_mode": {
                "allowed_globs": "all"
            },
            "whitelist_for_overwrite": {},
            "mode": "wcgw",
            "workspace_root": "/tmp/test",
            "chat_id": "i1234"
        }"#;

        let parsed: Result<BashStateSnapshot, _> = serde_json::from_str(wcgw_json);
        assert!(parsed.is_ok());

        let snapshot = parsed.unwrap();
        assert_eq!(snapshot.workspace_root, "/tmp/test");
        assert_eq!(snapshot.chat_id, "i1234");
        assert!(snapshot.cwd.is_empty()); // cwd should default to empty

        // When converting to components, cwd should fall back to workspace_root
        let (cwd, workspace_root, ..) = snapshot.to_state_components();
        assert_eq!(cwd, "/tmp/test");
        assert_eq!(workspace_root, "/tmp/test");
    }
}
