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

/// Most-active files for repo context, using wcgw's scoring: `reads*2 + edits +
/// writes`, top 5 (see `repo_context.py:222-238`). Kept separate from
/// [`active_files`] so the standalone status view can use its own weighting.
pub fn active_files_for_context(root: &Path) -> Vec<String> {
    const CONTEXT_ACTIVE_FILES: usize = 5;
    let Ok(stats) = load(root) else {
        return Vec::new();
    };

    let mut files = stats.files.into_iter().collect::<Vec<_>>();
    files.sort_by_key(|(path, stats)| {
        let score = (stats.reads * 2) + stats.edits + stats.writes;
        (std::cmp::Reverse(score), path.clone())
    });
    files.truncate(CONTEXT_ACTIVE_FILES);
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
    // Stored outside the repo (XDG data dir), keyed by a hash of the absolute
    // workspace path — survives wiping the repo and never pollutes it. Mirrors
    // wcgw's `~/.local/share/wcgw/workspace_stats/<name>_<hash>.json`.
    data_base().join("winx").join("workspace_stats").join(format!("{}.json", stats_key(root)))
}

/// XDG data base dir (`$XDG_DATA_HOME` or `~/.local/share`).
fn data_base() -> PathBuf {
    match std::env::var("XDG_DATA_HOME") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home::home_dir()
            .map_or_else(|| PathBuf::from("."), |home| home.join(".local").join("share")),
    }
}

/// Stable per-workspace filename: `<dir-name>_<hash-of-absolute-path>`.
fn stats_key(root: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let abs = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let name = abs.file_name().and_then(|n| n.to_str()).unwrap_or("workspace");
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    abs.to_string_lossy().hash(&mut hasher);
    format!("{name}_{:016x}", hasher.finish())
}
