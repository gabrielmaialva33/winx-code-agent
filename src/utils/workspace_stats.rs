use crate::errors::{Result, WinxError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_ACTIVE_FILES: usize = 30;

#[derive(Debug, Default, Serialize, Deserialize)]
struct WorkspaceStats {
    files: HashMap<String, FileStats>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileStats {
    reads: u64,
    writes: u64,
    edits: u64,
}

pub fn record_read(root: &Path, path: &Path) -> Result<()> {
    record(root, path, |stats| stats.reads += 1)
}

pub fn record_write(root: &Path, path: &Path) -> Result<()> {
    record(root, path, |stats| stats.writes += 1)
}

pub fn record_edit(root: &Path, path: &Path) -> Result<()> {
    record(root, path, |stats| stats.edits += 1)
}

pub fn active_files(root: &Path) -> Vec<String> {
    let Ok(stats) = load(root) else {
        return Vec::new();
    };

    let mut files = stats.files.into_iter().collect::<Vec<_>>();
    files.sort_by_key(|(path, stats)| {
        let score = stats.reads + (stats.edits * 4) + (stats.writes * 3);
        (std::cmp::Reverse(score), path.clone())
    });
    files.truncate(MAX_ACTIVE_FILES);
    files.into_iter().map(|(path, _)| path).collect()
}

fn record(root: &Path, path: &Path, update: impl FnOnce(&mut FileStats)) -> Result<()> {
    let relative = path.strip_prefix(root).unwrap_or(path).to_string_lossy().to_string();
    let mut stats = load(root).unwrap_or_default();
    update(stats.files.entry(relative).or_default());
    save(root, &stats)
}

fn load(root: &Path) -> Result<WorkspaceStats> {
    let path = stats_path(root);
    if !path.exists() {
        return Ok(WorkspaceStats::default());
    }
    let content = fs::read_to_string(&path).map_err(|e| WinxError::FileAccessError {
        path: path.clone(),
        message: format!("Failed to read workspace stats: {e}"),
    })?;
    serde_json::from_str(&content)
        .map_err(|e| WinxError::SerializationError(format!("Failed to parse workspace stats: {e}")))
}

fn save(root: &Path, stats: &WorkspaceStats) -> Result<()> {
    let path = stats_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| WinxError::FileAccessError {
            path: parent.to_path_buf(),
            message: format!("Failed to create workspace stats directory: {e}"),
        })?;
    }
    let content = serde_json::to_string_pretty(stats)
        .map_err(|e| WinxError::SerializationError(format!("Failed to serialize stats: {e}")))?;
    fs::write(&path, content).map_err(|e| WinxError::FileAccessError {
        path,
        message: format!("Failed to write workspace stats: {e}"),
    })
}

fn stats_path(root: &Path) -> PathBuf {
    root.join(".winx").join("workspace_stats.json")
}
