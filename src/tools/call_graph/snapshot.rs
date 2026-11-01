//! Background freshness detection; content hashes for git dirty files, stat fallback.
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, UNIX_EPOCH};

use tokio::io::AsyncReadExt as _;
use tokio::process::Command;

use super::index::safe_source_path;

pub(super) fn command(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .env_clear()
        .env("PATH", if cfg!(windows) { "C:\\Windows\\System32" } else { "/usr/bin:/bin" });
    #[cfg(windows)]
    if let Some(value) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", value);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.as_std_mut().process_group(0);
    }
    command.stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    command
}

pub(super) async fn terminate(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(group) = child.id().and_then(|id| i32::try_from(id).ok()) {
        let _ = crate::os::unix::signal_group(group, libc::SIGKILL);
    }
    let _ = child.kill().await;
}

async fn git(root: &Path, args: &[&str]) -> Result<Option<Vec<u8>>, &'static str> {
    let executable = if cfg!(windows) { Path::new("git.exe") } else { Path::new("/usr/bin/git") };
    let mut cmd = command(executable);
    cmd.args([
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
        "-C",
    ])
    .arg(root)
    .args(args)
    .stdout(Stdio::piped());
    let Ok(mut child) = cmd.spawn() else { return Ok(None) };
    let Some(stdout) = child.stdout.take() else { return Err("snapshot_failed") };
    let operation = async {
        let mut bytes = Vec::new();
        stdout
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "snapshot_failed")?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err("snapshot_oversize");
        }
        let status = child.wait().await.map_err(|_| "snapshot_failed")?;
        Ok(status.success().then_some(bytes))
    };
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(2), operation).await {
        result
    } else {
        terminate(&mut child).await;
        Err("snapshot_timeout")
    }
}

pub(super) async fn fingerprint(root: &Path) -> Result<String, &'static str> {
    if let Some(head) = git(root, &["rev-parse", "--verify", "HEAD"]).await? {
        for marker in ["MERGE_HEAD", "rebase-merge", "rebase-apply"] {
            let value =
                git(root, &["rev-parse", "--git-path", marker]).await?.ok_or("snapshot_failed")?;
            let path = String::from_utf8(value).map_err(|_| "snapshot_failed")?;
            if root.join(path.trim()).exists() {
                return Err("repository_busy");
            }
        }
        let mut files = git(
            root,
            &["diff", "--no-ext-diff", "--no-textconv", "--name-only", "-z", "HEAD", "--"],
        )
        .await?
        .ok_or("snapshot_failed")?;
        files.extend(
            git(root, &["ls-files", "--others", "--exclude-standard", "-z"])
                .await?
                .ok_or("snapshot_failed")?,
        );
        let root = root.to_owned();
        tokio::task::spawn_blocking(move || hash_dirty(&root, &head, &files))
            .await
            .map_err(|_| "snapshot_failed")?
    } else {
        let root = root.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut paths = crate::utils::repo::walk_workspace_files(&root);
            paths.sort();
            let mut hasher = DefaultHasher::new();
            for path in paths {
                path.strip_prefix(&root).map_err(|_| "snapshot_failed")?.hash(&mut hasher);
                let metadata = path.metadata().map_err(|_| "snapshot_failed")?;
                metadata.len().hash(&mut hasher);
                metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .hash(&mut hasher);
            }
            Ok(format!("stat-v1:{:016x}", hasher.finish()))
        })
        .await
        .map_err(|_| "snapshot_failed")?
    }
}

fn hash_dirty(root: &Path, head: &[u8], files: &[u8]) -> Result<String, &'static str> {
    let mut hasher = DefaultHasher::new();
    head.hash(&mut hasher);
    let mut names: Vec<_> =
        files.split(|byte| *byte == 0).filter(|name| !name.is_empty()).collect();
    names.sort_unstable();
    names.dedup();
    if names.len() > 20_000 {
        return Err("snapshot_oversize");
    }
    let mut total = 0u64;
    for name in names {
        let name = std::str::from_utf8(name).map_err(|_| "snapshot_non_utf8")?;
        // Agent artifacts never invalidate their own index.
        if name.starts_with(".winx/") {
            continue;
        }
        if !safe_source_path(name) {
            return Err("snapshot_unsafe_path");
        }
        name.hash(&mut hasher);
        let path: PathBuf = root.join(name);
        if !path.exists() {
            "deleted".hash(&mut hasher);
            continue;
        }
        let canonical = path.canonicalize().map_err(|_| "snapshot_failed")?;
        if !canonical.starts_with(root) {
            return Err("snapshot_unsafe_path");
        }
        let mut file = crate::os::open_read_nofollow(&path).map_err(|_| "snapshot_unsafe_path")?;
        let size = file.metadata().map_err(|_| "snapshot_failed")?.len();
        total = total.saturating_add(size);
        if total > 256 * 1024 * 1024 {
            return Err("snapshot_oversize");
        }
        size.hash(&mut hasher);
        let mut buffer = vec![0u8; 64 * 1024];
        let mut limited = (&mut file).take(size + 1);
        loop {
            let read = limited.read(&mut buffer).map_err(|_| "snapshot_failed")?;
            if read == 0 {
                break;
            }
            hasher.write(&buffer[..read]);
        }
        if limited.limit() == 0 {
            return Err("snapshot_changed");
        }
    }
    Ok(format!("git-v1:{:016x}", hasher.finish()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[tokio::test]
    async fn git_fingerprint_hashes_dirty_content_and_defers_merge_rebase() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        if git(&root, &["init", "-q"]).await.unwrap().is_none() {
            return;
        }
        std::fs::write(root.join("a.rs"), "fn aaa() {}\n").unwrap();
        assert!(git(&root, &["add", "."]).await.unwrap().is_some());
        assert!(git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "-qm",
                "fixture"
            ]
        )
        .await
        .unwrap()
        .is_some());
        let clean = fingerprint(&root).await.unwrap();
        std::fs::write(root.join("a.rs"), "fn bbb() {}\n").unwrap();
        let dirty = fingerprint(&root).await.unwrap();
        assert_ne!(clean, dirty);
        std::fs::write(root.join("a.rs"), "fn ccc() {}\n").unwrap();
        assert_ne!(dirty, fingerprint(&root).await.unwrap());
        assert!(git(&root, &["add", "."]).await.unwrap().is_some());
        let staged = fingerprint(&root).await.unwrap();
        std::fs::write(root.join("new.rs"), "fn new() {}\n").unwrap();
        assert_ne!(staged, fingerprint(&root).await.unwrap());
        for marker in ["MERGE_HEAD", "rebase-merge", "rebase-apply"] {
            let path = root.join(".git").join(marker);
            std::fs::write(&path, "fixture").unwrap();
            assert_eq!(fingerprint(&root).await.unwrap_err(), "repository_busy");
            std::fs::remove_file(path).unwrap();
        }
        std::fs::remove_file(root.join("a.rs")).unwrap();
        assert_ne!(staged, fingerprint(&root).await.unwrap());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod environment_tests {
    #[tokio::test]
    async fn backend_child_never_inherits_server_credentials_or_telemetry() -> std::io::Result<()> {
        const FLAG: &str = "WINX_TEST_GRAPH_ENVIRONMENT";
        if std::env::var_os(FLAG).is_some() {
            let output = super::command(std::path::Path::new("/usr/bin/env")).output().await?;
            let text = String::from_utf8_lossy(&output.stdout);
            for key in crate::config::SERVER_ONLY_ENV {
                assert!(!text.contains(key));
            }
            assert!(!text.contains("fixture-secret"));
            return Ok(());
        }
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "tools::call_graph::snapshot::environment_tests::backend_child_never_inherits_server_credentials_or_telemetry"])
            .env(FLAG, "1").env("WINX_HTTP_TOKEN", "fixture-secret")
            .env("WINX_USAGE_LOG", "fixture-telemetry").status()?;
        assert!(status.success());
        Ok(())
    }
}
