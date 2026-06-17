//! Offload the dropped head of an over-long PTY output to a scratch file.
//!
//! When a command's output grows past the in-memory cap the PTY keeps only the
//! tail and drops the oldest bytes (see the truncation paths in `pty.rs`). That
//! dropped head is the part the agent can no longer see. Instead of losing it,
//! we append it to a file under `<workspace>/.winx/scratch/` so the agent can
//! recover it with ReadFiles/SearchFiles. The file lives inside the workspace on
//! purpose: those read tools are workspace-confined and pipe through the
//! redaction layer, so a leaked secret in the log is still scrubbed on the way
//! back out.
//!
//! Everything here is best-effort: any IO error is logged at debug and swallowed
//! so output offload can never crash the server (release builds are
//! `panic = "abort"`).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::debug;

const SCRATCH_SUBDIR: &str = ".winx/scratch";

/// Monotonic per-process counter folded into scratch filenames so two shells
/// that ask for a path within the same nanosecond can't collide on the timestamp
/// and interleave their output into one file.
static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// Scratch files older than this are pruned when a new one is created.
const SCRATCH_MAX_AGE: Duration = Duration::from_secs(3600);

fn scratch_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(SCRATCH_SUBDIR)
}

/// A fresh, unique scratch-file path under the workspace, creating the directory
/// if needed and pruning stale files first so the dir does not grow without
/// bound. Returns `None` on any IO error (offload is best-effort).
pub fn new_scratch_path(workspace_root: &Path) -> Option<PathBuf> {
    let dir = scratch_dir(workspace_root);
    if let Err(e) = fs::create_dir_all(&dir) {
        debug!("scratch: create_dir_all {} failed: {e}", dir.display());
        return None;
    }
    prune_scratch_dir(workspace_root, SCRATCH_MAX_AGE);
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_nanos();
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    Some(dir.join(format!("bash-output-{}-{nonce:x}-{seq:x}.txt", std::process::id())))
}

/// Append `bytes` to the scratch file, creating it if absent.
pub fn append_scratch(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(bytes)
}

/// Remove scratch files whose mtime is older than `max_age`. Best-effort:
/// per-entry failures are logged at debug and skipped, and a missing dir is a
/// no-op.
pub fn prune_scratch_dir(workspace_root: &Path, max_age: Duration) {
    let dir = scratch_dir(workspace_root);
    let Ok(entries) = fs::read_dir(&dir) else { return };
    let cutoff = SystemTime::now().checked_sub(max_age).unwrap_or(UNIX_EPOCH);
    for entry in entries.flatten() {
        let path = entry.path();
        let stale =
            fs::metadata(&path).and_then(|meta| meta.modified()).is_ok_and(|mtime| mtime < cutoff);
        if stale {
            if let Err(e) = fs::remove_file(&path) {
                debug!("scratch: prune remove {} failed: {e}", path.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::thread::sleep;
    use tempfile::TempDir;

    #[test]
    fn new_scratch_path_is_under_winx_scratch() {
        let ws = TempDir::new().unwrap();
        let path = new_scratch_path(ws.path()).expect("path");
        assert!(
            path.starts_with(ws.path().join(".winx/scratch")),
            "path outside scratch: {path:?}"
        );
        assert!(path.parent().is_some_and(Path::exists), "scratch dir not created");
    }

    #[test]
    fn new_scratch_path_is_unique_per_call() {
        // Two calls (possibly within the same nanosecond) must not collide, or
        // two shells would interleave output into one file.
        let ws = TempDir::new().unwrap();
        let a = new_scratch_path(ws.path()).unwrap();
        let b = new_scratch_path(ws.path()).unwrap();
        assert_ne!(a, b, "consecutive scratch paths must be unique");
    }

    #[test]
    fn append_scratch_accumulates() {
        let ws = TempDir::new().unwrap();
        let path = new_scratch_path(ws.path()).unwrap();
        append_scratch(&path, b"head-one\n").unwrap();
        append_scratch(&path, b"head-two\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "head-one\nhead-two\n");
    }

    #[test]
    fn prune_removes_stale_keeps_fresh() {
        let ws = TempDir::new().unwrap();
        let old = new_scratch_path(ws.path()).unwrap();
        append_scratch(&old, b"old").unwrap();
        sleep(Duration::from_millis(15));
        // Prune anything older than 1ms: the 15ms-old file goes, a brand-new one stays.
        prune_scratch_dir(ws.path(), Duration::from_millis(1));
        assert!(!old.exists(), "stale scratch file should be pruned");
        let fresh = new_scratch_path(ws.path()).unwrap();
        append_scratch(&fresh, b"fresh").unwrap();
        prune_scratch_dir(ws.path(), Duration::from_secs(3600));
        assert!(fresh.exists(), "fresh scratch file must survive");
    }

    #[test]
    fn new_scratch_path_returns_none_on_unwritable_root() {
        // A workspace root that cannot hold a child dir must yield None, not panic.
        let path = new_scratch_path(Path::new("/proc/winx-nonexistent-xyz"));
        assert!(path.is_none(), "expected None for unwritable root");
    }
}
