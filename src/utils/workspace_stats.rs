use crate::errors::{Result, WinxError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_ACTIVE_FILES: usize = 30;
/// Bound the persisted per-workspace history. This is deliberately much larger
/// than either consumer's top-N view, while preventing monorepos and long-lived
/// plugin sessions from growing the JSON file forever.
const MAX_TRACKED_FILES: usize = 4_096;

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
    record(root, path, Activity::Read)
}

/// Record one `ReadFiles` batch with a single load/save cycle.
pub fn record_reads(root: &Path, paths: &[PathBuf]) -> Result<()> {
    record_many(root, paths.iter().map(PathBuf::as_path), Activity::Read)
}

pub fn record_write(root: &Path, path: &Path) -> Result<()> {
    record(root, path, Activity::Write)
}

pub fn record_edit(root: &Path, path: &Path) -> Result<()> {
    record(root, path, Activity::Edit)
}

pub fn active_files(root: &Path) -> Vec<String> {
    let Ok(stats) = load(root) else {
        return Vec::new();
    };

    let mut files = stats.files.into_iter().collect::<Vec<_>>();
    files.sort_by_key(|(path, stats)| {
        let score = stats
            .reads
            .saturating_add(stats.edits.saturating_mul(4))
            .saturating_add(stats.writes.saturating_mul(3));
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
        let score = stats
            .reads
            .saturating_mul(2)
            .saturating_add(stats.edits)
            .saturating_add(stats.writes);
        (std::cmp::Reverse(score), path.clone())
    });
    files.truncate(CONTEXT_ACTIVE_FILES);
    files.into_iter().map(|(path, _)| path).collect()
}

#[derive(Clone, Copy)]
enum Activity {
    Read,
    Write,
    Edit,
}

fn record(root: &Path, path: &Path, activity: Activity) -> Result<()> {
    record_many(root, std::iter::once(path), activity)
}

fn record_many<'a>(
    root: &Path,
    paths: impl IntoIterator<Item = &'a Path>,
    activity: Activity,
) -> Result<()> {
    let mut stats = load(root).unwrap_or_default();
    let mut last_recorded = None;
    for path in paths {
        let relative = path.strip_prefix(root).unwrap_or(path).to_string_lossy().to_string();
        let file = stats.files.entry(relative.clone()).or_default();
        match activity {
            Activity::Read => file.reads = file.reads.saturating_add(1),
            Activity::Write => file.writes = file.writes.saturating_add(1),
            Activity::Edit => file.edits = file.edits.saturating_add(1),
        }
        last_recorded = Some(relative);
    }
    let Some(last_recorded) = last_recorded else { return Ok(()) };
    prune_stats(&mut stats, &last_recorded);
    save(root, &stats)
}

fn prune_stats(stats: &mut WorkspaceStats, preserve: &str) {
    let remove_count = stats.files.len().saturating_sub(MAX_TRACKED_FILES);
    if remove_count == 0 {
        return;
    }

    let mut candidates = stats
        .files
        .iter()
        .filter(|(path, _)| path.as_str() != preserve)
        .map(|(path, file)| {
            let activity = file.reads.saturating_add(file.writes).saturating_add(file.edits);
            (activity, path.clone())
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    for (_, path) in candidates.into_iter().take(remove_count) {
        stats.files.remove(&path);
    }
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

#[cfg(test)]
mod tests {
    use super::{prune_stats, FileStats, WorkspaceStats, MAX_TRACKED_FILES};

    #[test]
    fn persisted_history_is_bounded_and_preserves_current_file() {
        let mut stats = WorkspaceStats::default();
        for index in 0..(MAX_TRACKED_FILES + 3) {
            stats.files.insert(
                format!("file-{index:04}.rs"),
                FileStats { reads: index as u64, writes: 0, edits: 0 },
            );
        }
        let current = "file-0000.rs";

        prune_stats(&mut stats, current);

        assert_eq!(stats.files.len(), MAX_TRACKED_FILES);
        assert!(stats.files.contains_key(current));
        assert!(!stats.files.contains_key("file-0001.rs"));
        assert!(!stats.files.contains_key("file-0002.rs"));
        assert!(!stats.files.contains_key("file-0003.rs"));
    }
}
