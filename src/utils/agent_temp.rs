//! Managed workspace-local temporary artifacts for model-driven helpers.
//!
//! `Initialize` advertises one stable directory per Winx session under
//! `<workspace>/.winx/tmp/`. The directory is intentionally not created until a
//! file tool actually needs it, so merely inspecting a repository does not
//! dirty its working tree. File tools enforce a small path and storage budget;
//! Bash remains governed by its own operator-selected command policy.

use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use tracing::debug;

use crate::errors::{Result, WinxError};

const TEMP_ROOT: &str = ".winx/tmp";
const SESSION_PREFIX: &str = "session-";
const SESSION_HASH_BYTES: usize = 8;

/// Session directories with no filesystem activity for this long are pruned
/// when another session initializes in the same workspace.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// Workspace-wide budget shared by all managed temporary sessions.
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
/// One helper should remain small enough to inspect and remove cheaply.
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// Depth beneath the session directory, including the target filename.
pub const MAX_RELATIVE_COMPONENTS: usize = 8;
/// Prevent payloads from being smuggled into a filename or directory component.
pub const MAX_COMPONENT_BYTES: usize = 120;
/// Bound the whole session-relative path independently of component count.
pub const MAX_RELATIVE_PATH_BYTES: usize = 512;

const MAX_SCAN_ENTRIES: usize = 50_000;
const MAX_SCAN_DEPTH: usize = 16;
static TEMP_IO_LOCK: Mutex<()> = Mutex::new(());

/// Policy information returned by `Initialize` and reused by file-tool errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentTempInfo {
    pub directory: PathBuf,
    pub ttl_seconds: u64,
    pub max_total_bytes: u64,
    pub max_file_bytes: u64,
}

/// A resolved edit and its size change, used to validate a `MultiFileEdit`
/// against the workspace-wide quota before any file is committed.
#[derive(Clone, Copy, Debug)]
pub struct TempEdit<'a> {
    pub path: &'a Path,
    pub previous_bytes: u64,
    pub new_bytes: u64,
}

/// Compute the stable session directory without touching the filesystem.
pub fn session_info(workspace_root: &Path, thread_id: &str) -> AgentTempInfo {
    let workspace = workspace_root.canonicalize().unwrap_or_else(|_| workspace_root.to_path_buf());
    AgentTempInfo {
        directory: workspace.join(TEMP_ROOT).join(session_name(thread_id)),
        ttl_seconds: MAX_AGE.as_secs(),
        max_total_bytes: MAX_TOTAL_BYTES,
        max_file_bytes: MAX_FILE_BYTES,
    }
}

/// Return the session policy and best-effort prune expired sibling sessions.
/// The active directory is not created here; `FileWriteOrEdit` creates it on
/// demand through its normal parent-directory path.
pub fn prepare_session(workspace_root: &Path, thread_id: &str) -> AgentTempInfo {
    let info = session_info(workspace_root, thread_id);
    let _guard = TEMP_IO_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    prune_sessions_unlocked(workspace_root, &info.directory, MAX_AGE);
    info
}

/// Validate one `FileWriteOrEdit` target. Non-temporary project files keep their
/// existing behavior; only legacy Winx root artifacts and `.winx/tmp` receive
/// this additional governance.
pub fn validate_edit_target(
    workspace_root: &Path,
    thread_id: &str,
    requested_path: &Path,
    resolved_path: &Path,
    previous_bytes: Option<u64>,
    new_bytes: u64,
) -> Result<()> {
    let info = session_info(workspace_root, thread_id);
    let workspace = workspace_root.canonicalize().unwrap_or_else(|_| workspace_root.to_path_buf());
    // On macOS, tempfile and callers may spell the same workspace as `/var/...`
    // while canonicalization produces `/private/var/...`. Accept either spelling
    // at this boundary, then rebase policy checks onto the canonical workspace.
    // The resolved path from the file-safety layer is normally canonical already;
    // keeping the lexical fallback makes this helper safe and independently
    // testable without allowing an alias to bypass temporary-artifact policy.
    let requested_relative = workspace_relative(requested_path, workspace_root, &workspace);
    let resolved_relative = workspace_relative(resolved_path, workspace_root, &workspace);

    if let Some(relative) = resolved_relative.as_deref() {
        reject_legacy_root_artifact(relative, previous_bytes.is_none(), requested_path, &info)?;
    }
    if requested_relative != resolved_relative {
        if let Some(relative) = requested_relative.as_deref() {
            reject_legacy_root_artifact(relative, previous_bytes.is_none(), requested_path, &info)?;
        }
    }

    let requested_temp = requested_relative.as_deref().is_some_and(is_temp_relative);
    let resolved_temp = resolved_relative.as_deref().is_some_and(is_temp_relative);
    if !requested_temp && !resolved_temp {
        return Ok(());
    }

    reject_symlinked_temp_root(&workspace, requested_path, &info)?;
    let policy_path = resolved_relative
        .as_deref()
        .map_or_else(|| resolved_path.to_path_buf(), |relative| workspace.join(relative));
    validate_session_relative_path(&policy_path, &info)?;
    if new_bytes > MAX_FILE_BYTES {
        return Err(policy_error(
            &policy_path,
            &info,
            format!(
                "helper content is {new_bytes} bytes; one temporary file is limited to \
                 {MAX_FILE_BYTES} bytes"
            ),
        ));
    }

    Ok(())
}

/// Reject statically visible shell writes that bypass the managed session area.
///
/// Bash remains a general-purpose operator-controlled capability, so this is not
/// presented as a filesystem sandbox. It covers the high-confidence cases an
/// agent can correct immediately: literal output redirects, common destination
/// arguments, and embedded language file-writer calls that name a Winx helper.
pub fn validate_bash_command(
    workspace_root: &Path,
    cwd: &Path,
    thread_id: &str,
    command: &str,
) -> Result<()> {
    let info = session_info(workspace_root, thread_id);
    let mut paths = crate::utils::bash_parser::extract_static_write_paths(command);
    paths.extend(embedded_writer_paths(command));
    paths.sort();
    paths.dedup();

    for path in paths {
        validate_bash_write_target(workspace_root, cwd, &path, &info)?;
    }
    Ok(())
}

fn validate_bash_write_target(
    workspace_root: &Path,
    cwd: &Path,
    path: &str,
    info: &AgentTempInfo,
) -> Result<()> {
    let expanded = crate::utils::path::expand_user(path);
    let requested = if Path::new(&expanded).is_absolute() {
        PathBuf::from(&expanded)
    } else {
        cwd.join(&expanded)
    };
    let resolved = crate::utils::path::resolve_in_workspace(path, cwd, workspace_root)
        .unwrap_or_else(|_| requested.clone());
    let workspace = workspace_root.canonicalize().unwrap_or_else(|_| workspace_root.to_path_buf());

    let requested_relative = workspace_relative(&requested, workspace_root, &workspace);
    let resolved_relative = workspace_relative(&resolved, workspace_root, &workspace);
    for relative in
        [requested_relative.as_deref(), resolved_relative.as_deref()].into_iter().flatten()
    {
        let Some(Component::Normal(first)) = relative.components().next() else { continue };
        let first = first.to_string_lossy();
        if first == ".winx_tmp" || first.starts_with(".winx-") {
            return Err(policy_error(
                &requested,
                info,
                format!(
                    "shell writes to Winx helper artifacts at the workspace root are not \
                     allowed; use WINX_TEMP_DIR={} instead",
                    info.directory.display()
                ),
            ));
        }
        if is_temp_relative(relative) {
            reject_symlinked_temp_root(&workspace, &requested, info)?;
            validate_session_relative_path(&workspace.join(relative), info)?;
        }
    }
    Ok(())
}

fn embedded_writer_paths(command: &str) -> Vec<String> {
    let lower = command.to_ascii_lowercase();
    let explicit_writer = [
        ".write_text(",
        ".write_bytes(",
        "writefilesync(",
        "appendfilesync(",
        "writefile(",
        "appendfile(",
        "bun.write(",
        "deno.write",
        "file.write(",
        "file.binwrite(",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    let writable_open = lower.contains("open(")
        && [", 'w", ", \"w", ", 'a", ", \"a", ", 'x", ", \"x", ", 'r+", ", \"r+"]
            .iter()
            .any(|mode| lower.contains(mode));
    if !explicit_writer && !writable_open {
        return Vec::new();
    }

    quoted_literals(command).into_iter().filter(|literal| literal.contains(".winx")).collect()
}

fn quoted_literals(value: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\'' && character != '"' {
            continue;
        }
        let quote = character;
        let mut literal = String::new();
        let mut escaped = false;
        for character in chars.by_ref() {
            if escaped {
                literal.push(character);
                escaped = false;
            } else if character == '\\' && quote == '"' {
                escaped = true;
            } else if character == quote {
                break;
            } else {
                literal.push(character);
            }
        }
        if !literal.is_empty() {
            literals.push(literal);
        }
    }
    literals
}

fn workspace_relative(
    path: &Path,
    workspace_root: &Path,
    canonical_workspace: &Path,
) -> Option<PathBuf> {
    path.strip_prefix(canonical_workspace)
        .or_else(|_| path.strip_prefix(workspace_root))
        .ok()
        .map(Path::to_path_buf)
}

/// Validate the aggregate size of a `MultiFileEdit` after every target has been
/// planned and duplicate paths have been rejected.
pub fn validate_batch_quota(
    workspace_root: &Path,
    thread_id: &str,
    edits: &[TempEdit<'_>],
) -> Result<()> {
    let info = session_info(workspace_root, thread_id);
    let managed: Vec<_> =
        edits.iter().filter(|edit| edit.path.starts_with(&info.directory)).collect();
    if managed.is_empty() {
        return Ok(());
    }
    let representative = managed[0].path;

    let workspace = workspace_root.canonicalize().unwrap_or_else(|_| workspace_root.to_path_buf());
    let _guard = TEMP_IO_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut projected = temp_tree_size(&workspace.join(TEMP_ROOT)).ok_or_else(|| {
        policy_error(
            representative,
            &info,
            "temporary storage could not be measured safely; remove malformed or excessive \
             artifacts before retrying"
                .to_string(),
        )
    })?;
    for edit in managed {
        projected = projected.saturating_sub(edit.previous_bytes).saturating_add(edit.new_bytes);
    }
    validate_projected_size(representative, &info, projected)
}

fn session_name(thread_id: &str) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(thread_id.as_bytes());
    let mut suffix = String::with_capacity(SESSION_HASH_BYTES * 2);
    for byte in &digest[..SESSION_HASH_BYTES] {
        let _ = write!(suffix, "{byte:02x}");
    }
    format!("{SESSION_PREFIX}{suffix}")
}

fn reject_legacy_root_artifact(
    relative: &Path,
    is_new: bool,
    requested_path: &Path,
    info: &AgentTempInfo,
) -> Result<()> {
    let Some(Component::Normal(first)) = relative.components().next() else { return Ok(()) };
    let first = first.to_string_lossy();
    if first == ".winx_tmp" || (is_new && first.starts_with(".winx-")) {
        return Err(policy_error(
            requested_path,
            info,
            format!(
                "ad hoc Winx artifacts at the workspace root are not allowed; use {} with a \
                 short descriptive filename",
                info.directory.display()
            ),
        ));
    }
    Ok(())
}

fn is_temp_relative(relative: &Path) -> bool {
    let mut components = relative.components();
    matches!(components.next(), Some(Component::Normal(value)) if value == OsStr::new(".winx"))
        && matches!(components.next(), Some(Component::Normal(value)) if value == OsStr::new("tmp"))
}

fn reject_symlinked_temp_root(workspace: &Path, path: &Path, info: &AgentTempInfo) -> Result<()> {
    let temp_root = workspace.join(TEMP_ROOT);
    if fs::symlink_metadata(&temp_root).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(policy_error(
            path,
            info,
            format!(
                "{} is a symlink; the managed temporary root must be a real directory inside \
                 the workspace",
                temp_root.display()
            ),
        ));
    }
    Ok(())
}

fn validate_session_relative_path(path: &Path, info: &AgentTempInfo) -> Result<()> {
    let relative = path.strip_prefix(&info.directory).map_err(|_| {
        policy_error(
            path,
            info,
            format!(
                "temporary helpers are session-scoped; use the exact temporary_artifact_dir {} \
                 returned by Initialize",
                info.directory.display()
            ),
        )
    })?;
    let relative_bytes = relative.as_os_str().as_encoded_bytes().len();
    if relative_bytes == 0 {
        return Err(policy_error(path, info, "a helper filename is required".to_string()));
    }
    if relative_bytes > MAX_RELATIVE_PATH_BYTES {
        return Err(policy_error(
            path,
            info,
            format!(
                "the session-relative path is {relative_bytes} bytes; temporary paths are limited \
                 to {MAX_RELATIVE_PATH_BYTES} bytes"
            ),
        ));
    }

    let mut count = 0usize;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(policy_error(
                path,
                info,
                "temporary paths may contain only normal filename components".to_string(),
            ));
        };
        count += 1;
        let bytes = name.as_encoded_bytes().len();
        if bytes > MAX_COMPONENT_BYTES {
            return Err(policy_error(
                path,
                info,
                format!(
                    "a temporary path component is {bytes} bytes; components are limited to \
                     {MAX_COMPONENT_BYTES} bytes so content is never encoded in filesystem names"
                ),
            ));
        }
    }
    if count > MAX_RELATIVE_COMPONENTS {
        return Err(policy_error(
            path,
            info,
            format!(
                "temporary path depth is {count}; at most {MAX_RELATIVE_COMPONENTS} components \
                 are allowed beneath the session directory"
            ),
        ));
    }
    Ok(())
}

fn validate_projected_size(path: &Path, info: &AgentTempInfo, projected: u64) -> Result<()> {
    if projected <= MAX_TOTAL_BYTES {
        return Ok(());
    }
    Err(policy_error(
        path,
        info,
        format!(
            "this write would grow managed temporary storage to {projected} bytes; the \
             workspace-wide limit is {MAX_TOTAL_BYTES} bytes. Remove helpers that are no longer \
             needed, then retry with corrected content"
        ),
    ))
}

fn policy_error(path: &Path, info: &AgentTempInfo, message: String) -> WinxError {
    WinxError::TemporaryArtifactPolicy {
        path: path.to_path_buf(),
        temporary_artifact_dir: info.directory.clone(),
        message,
    }
}

fn temp_tree_size(root: &Path) -> Option<u64> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(0),
        Err(_) => return None,
    };
    if metadata.file_type().is_symlink() {
        return None;
    }

    let mut total = 0u64;
    let mut seen = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((path, depth)) = stack.pop() {
        if seen >= MAX_SCAN_ENTRIES || depth > MAX_SCAN_DEPTH {
            return None;
        }
        seen += 1;
        let metadata = fs::symlink_metadata(&path).ok()?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
            continue;
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(&path).ok()? {
                stack.push((entry.ok()?.path(), depth + 1));
            }
        }
    }
    Some(total)
}

fn prune_sessions_unlocked(workspace_root: &Path, active: &Path, max_age: Duration) {
    let workspace = workspace_root.canonicalize().unwrap_or_else(|_| workspace_root.to_path_buf());
    let root = workspace.join(TEMP_ROOT);
    if fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return;
    }
    let Ok(entries) = fs::read_dir(&root) else { return };
    let cutoff = SystemTime::now().checked_sub(max_age).unwrap_or(UNIX_EPOCH);
    for entry in entries.flatten() {
        let path = entry.path();
        if path == active || !is_managed_session_entry(&entry) || !session_is_stale(&path, cutoff) {
            continue;
        }
        if let Err(error) = fs::remove_dir_all(&path) {
            debug!(path = %path.display(), %error, "failed to prune expired agent temp session");
        }
    }
}

fn is_managed_session_entry(entry: &fs::DirEntry) -> bool {
    if !entry.file_type().is_ok_and(|kind| kind.is_dir() && !kind.is_symlink()) {
        return false;
    }
    let name = entry.file_name();
    let name = name.to_string_lossy();
    let Some(hash) = name.strip_prefix(SESSION_PREFIX) else { return false };
    hash.len() == SESSION_HASH_BYTES * 2 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Conservative bounded scan: any read/metadata error or an excessive tree
/// retains the session rather than deleting data whose freshness is uncertain.
fn session_is_stale(root: &Path, cutoff: SystemTime) -> bool {
    let mut newest = UNIX_EPOCH;
    let mut seen = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((path, depth)) = stack.pop() {
        if seen >= MAX_SCAN_ENTRIES || depth > MAX_SCAN_DEPTH {
            return false;
        }
        seen += 1;
        let Ok(metadata) = fs::symlink_metadata(&path) else { return false };
        let Ok(modified) = metadata.modified() else { return false };
        newest = newest.max(modified);
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            let Ok(entries) = fs::read_dir(&path) else { return false };
            for entry in entries {
                let Ok(entry) = entry else { return false };
                stack.push((entry.path(), depth + 1));
            }
        }
    }
    newest < cutoff
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use std::thread::sleep;
    use tempfile::TempDir;

    #[test]
    fn session_path_is_stable_short_and_workspace_local() {
        let workspace = TempDir::new().unwrap();
        let first = session_info(workspace.path(), "chat/project:one");
        let repeated = session_info(workspace.path(), "chat/project:one");
        let other = session_info(workspace.path(), "chat/project:two");
        assert_eq!(first, repeated);
        assert_ne!(first.directory, other.directory);
        assert!(first.directory.starts_with(workspace.path().canonicalize().unwrap()));
        let name = first.directory.file_name().unwrap().to_string_lossy();
        assert_eq!(name.len(), SESSION_PREFIX.len() + SESSION_HASH_BYTES * 2);
    }

    #[test]
    fn accepts_short_helpers_only_in_the_active_session() {
        let workspace = TempDir::new().unwrap();
        let info = session_info(workspace.path(), "active");
        let valid = info.directory.join("review/adapter.py");
        validate_edit_target(workspace.path(), "active", &valid, &valid, None, 128).unwrap();

        let other = session_info(workspace.path(), "other").directory.join("adapter.py");
        let error = validate_edit_target(workspace.path(), "active", &other, &other, None, 128)
            .expect_err("cross-session temp write must fail");
        assert!(error.to_string().contains("session-scoped"), "{error}");
    }

    #[test]
    fn bash_writes_accept_only_the_active_session_temp_path() {
        let workspace = TempDir::new().unwrap();
        let info = session_info(workspace.path(), "active");
        let valid = format!("printf x > '{}'", info.directory.join("adapter.ts").display());
        validate_bash_command(workspace.path(), workspace.path(), "active", &valid)
            .expect("active session destination must be accepted");

        for command in [
            "printf x > .winx/tmp/direct.ts",
            "printf x | tee .winx-review-carrier.js",
            "python - <<'PY'\nfrom pathlib import Path\nPath('.winx/tmp/direct.py').write_text('x')\nPY",
        ] {
            let error = validate_bash_command(workspace.path(), workspace.path(), "active", command)
                .expect_err("unmanaged shell helper must fail");
            assert!(
                error.to_string().to_ascii_lowercase().contains("temporary artifact policy"),
                "{error}"
            );
        }
    }

    #[test]
    fn bash_temp_preflight_does_not_block_reads_or_dynamic_active_path() {
        let workspace = TempDir::new().unwrap();
        for command in [
            "rg needle .winx/tmp/direct.ts",
            "cat .winx-review-carrier.js",
            "mkdir -p \"$WINX_TEMP_DIR\" && printf x > \"$WINX_TEMP_DIR/helper.ts\"",
        ] {
            validate_bash_command(workspace.path(), workspace.path(), "active", command)
                .expect("read or runtime-provided destination must remain available");
        }
    }

    #[test]
    fn rejects_legacy_root_artifacts_and_payload_names() {
        let workspace = TempDir::new().unwrap();
        let legacy = workspace.path().join(".winx_tmp/payload/file.txt");
        let error = validate_edit_target(workspace.path(), "active", &legacy, &legacy, None, 10)
            .expect_err("legacy root must fail");
        assert!(error.to_string().contains("workspace root"), "{error}");

        let info = session_info(workspace.path(), "active");
        let encoded = info.directory.join(format!("{}.txt", "x".repeat(MAX_COMPONENT_BYTES + 1)));
        let error = validate_edit_target(workspace.path(), "active", &encoded, &encoded, None, 10)
            .expect_err("long component must fail");
        assert!(error.to_string().contains("filesystem names"), "{error}");
    }

    #[test]
    fn resolved_path_cannot_hide_a_root_artifact_behind_parent_components() {
        let workspace = TempDir::new().unwrap();
        let requested = workspace.path().join("nested/../.winx-review-carrier.py");
        let resolved = workspace.path().join(".winx-review-carrier.py");
        let error =
            validate_edit_target(workspace.path(), "active", &requested, &resolved, None, 10)
                .expect_err("resolved top-level artifact must fail");
        assert!(error.to_string().contains("workspace root"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn lexical_workspace_alias_cannot_bypass_temp_policy() {
        use std::os::unix::fs::symlink;

        let parent = TempDir::new().unwrap();
        let real_workspace = parent.path().join("real-workspace");
        let alias_workspace = parent.path().join("workspace-alias");
        fs::create_dir(&real_workspace).unwrap();
        symlink(&real_workspace, &alias_workspace).unwrap();

        let legacy = alias_workspace.join(".winx_tmp/payload.txt");
        let error = validate_edit_target(&alias_workspace, "active", &legacy, &legacy, None, 10)
            .expect_err("a lexical workspace alias must not bypass root-artifact policy");
        assert!(error.to_string().contains("workspace root"), "{error}");

        let canonical_workspace = alias_workspace.canonicalize().unwrap();
        let canonical_session = session_info(&alias_workspace, "active").directory;
        let relative_session = canonical_session.strip_prefix(&canonical_workspace).unwrap();
        let aliased_helper = alias_workspace.join(relative_session).join("adapter.py");
        validate_edit_target(
            &alias_workspace,
            "active",
            &aliased_helper,
            &aliased_helper,
            None,
            10,
        )
        .expect("the active managed directory must work through the lexical alias");
    }

    #[test]
    fn prune_removes_only_expired_managed_siblings() {
        let workspace = TempDir::new().unwrap();
        let active = session_info(workspace.path(), "active").directory;
        let expired = session_info(workspace.path(), "expired").directory;
        let unmanaged = workspace.path().join(TEMP_ROOT).join("manual-cache");
        fs::create_dir_all(&active).unwrap();
        fs::create_dir_all(&expired).unwrap();
        fs::create_dir_all(&unmanaged).unwrap();
        fs::write(expired.join("old.txt"), "old").unwrap();
        sleep(Duration::from_millis(15));

        let _guard = TEMP_IO_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        prune_sessions_unlocked(workspace.path(), &active, Duration::from_millis(1));
        assert!(active.exists());
        assert!(!expired.exists());
        assert!(unmanaged.exists());
    }

    #[test]
    fn quota_projection_accounts_for_replacements() {
        let workspace = TempDir::new().unwrap();
        let info = session_info(workspace.path(), "active");
        let path = info.directory.join("large.bin");
        assert!(validate_projected_size(&path, &info, MAX_TOTAL_BYTES).is_ok());
        assert!(validate_projected_size(&path, &info, MAX_TOTAL_BYTES + 1).is_err());
    }

    #[test]
    fn batch_quota_counts_every_planned_helper_before_committing() {
        let workspace = TempDir::new().unwrap();
        let info = session_info(workspace.path(), "active");
        let paths = (0..9)
            .map(|index| info.directory.join(format!("helper-{index}.bin")))
            .collect::<Vec<_>>();
        let edits = paths
            .iter()
            .map(|path| TempEdit { path, previous_bytes: 0, new_bytes: MAX_FILE_BYTES })
            .collect::<Vec<_>>();

        let error = validate_batch_quota(workspace.path(), "active", &edits)
            .expect_err("aggregate batch must honor the workspace quota");
        assert!(error.to_string().contains("workspace-wide limit"), "{error}");
    }
}
