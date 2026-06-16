//! Bash state persistence module
//!
//! Provides disk persistence for `BashState`, compatible with WCGW Python implementation.
//! State is stored in `~/.local/share/wcgw/bash_state/` as JSON files.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, warn};

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

/// Write `content` to `path` atomically: a uniquely-named temp file in the same
/// directory, renamed over the target. A reader (or a concurrent writer for the
/// same `thread_id`) sees either the complete old file or the complete new one —
/// never a half-written, un-parseable JSON.
///
/// No `fsync`: `persist_state` runs several times per tool call, and a per-write
/// fsync would add real latency there. The atomic rename already prevents torn
/// writes; we trade only durability-against-power-loss, which is fine for
/// resumable session state.
fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent =
        path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".winx-state-")
        .tempfile_in(parent)
        .with_context(|| format!("create temp state file in {}", parent.display()))?;
    tmp.write_all(content)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Parse a state snapshot, quarantining the file on corruption.
///
/// A corrupt state file must not silently re-fail every boot. We move it aside to
/// `<name>.corrupt`, log loudly (the saved session — including its security mode —
/// is gone), and return the parse error. Callers currently treat that as "no
/// saved state"; the loud log makes the loss visible instead of silent.
fn parse_state_or_quarantine(path: &Path, json: &str) -> Result<BashStateSnapshot> {
    match serde_json::from_str(json) {
        Ok(snapshot) => Ok(snapshot),
        Err(parse_err) => {
            let quarantine = path.with_extension("json.corrupt");
            match fs::rename(path, &quarantine) {
                Ok(()) => error!(
                    path = %path.display(),
                    quarantine = %quarantine.display(),
                    "corrupt bash state quarantined; saved session (incl. its mode) lost: {parse_err}"
                ),
                Err(rename_err) => warn!(
                    path = %path.display(),
                    "failed to quarantine corrupt bash state: {rename_err}"
                ),
            }
            Err(parse_err).context("parsing persisted bash state")
        }
    }
}

pub fn save_bash_state(thread_id: &str, state: &BashStateSnapshot) -> Result<()> {
    if thread_id.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_string_pretty(state)?;
    atomic_write(&get_state_file_path(thread_id)?, json.as_bytes())?;
    Ok(())
}

pub fn save_bash_state_to_path(path: &Path, state: &BashStateSnapshot) -> Result<()> {
    let json = serde_json::to_string_pretty(state)?;
    atomic_write(path, json.as_bytes())?;
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
    let json = fs::read_to_string(&path)?;
    Ok(Some(parse_state_or_quarantine(&path, &json)?))
}

pub fn load_bash_state_from_path(path: &Path) -> Result<Option<BashStateSnapshot>> {
    if !path.exists() {
        return Ok(None);
    }
    let json = fs::read_to_string(path)?;
    Ok(Some(parse_state_or_quarantine(path, &json)?))
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
                BashMode::NormalMode => "normal_mode".to_string(),
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

#[cfg(test)]
mod tests {
    use super::{atomic_write, load_bash_state_from_path};
    use anyhow::Result;

    #[test]
    fn atomic_write_round_trips_and_replaces() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("data.bin");
        atomic_write(&path, b"hello atomic")?;
        assert_eq!(std::fs::read(&path)?, b"hello atomic");
        // Overwriting is also atomic and leaves a single clean file.
        atomic_write(&path, b"second")?;
        assert_eq!(std::fs::read(&path)?, b"second");
        Ok(())
    }

    #[test]
    fn missing_state_file_is_ok_none() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("absent_bash_state.json");
        assert!(load_bash_state_from_path(&path)?.is_none());
        Ok(())
    }

    #[test]
    fn corrupt_state_is_quarantined_and_surfaces_error() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("t1_bash_state.json");
        std::fs::write(&path, "{ not valid json ")?;
        // Corruption must NOT degrade into Ok(None) (a silent fresh session).
        assert!(load_bash_state_from_path(&path).is_err());
        // The bad file is moved aside so it doesn't re-fail every boot.
        assert!(!path.exists(), "corrupt file should be quarantined");
        assert!(path.with_extension("json.corrupt").exists(), "quarantine file should exist");
        Ok(())
    }
}
