//! Managed workspace-local temporary artifacts for model-driven helpers.
//!
//! `Initialize` advertises one stable directory per Winx session under
//! `<workspace>/.winx/tmp/`. The directory is intentionally not created until a
//! file tool actually needs it, so merely inspecting a repository does not
//! dirty its working tree. File tools enforce a small path and storage budget;
//! Bash remains governed by its own operator-selected command policy.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tracing::{debug, info};

use crate::errors::{Result, WinxError};

const TEMP_ROOT: &str = ".winx/tmp";
const SESSION_PREFIX: &str = "session-";
const SESSION_HASH_BYTES: usize = 8;

/// Session directories with no filesystem activity for this long are pruned
/// when another session initializes in the same workspace.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// Workspace-wide budget shared by all managed temporary sessions.
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
/// One active conversation cannot consume the whole workspace-wide budget.
pub const MAX_SESSION_BYTES: u64 = 64 * 1024 * 1024;
/// One helper should remain small enough to inspect and remove cheaply.
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// Hundreds of one-shot carriers are almost always an orchestration failure.
pub const MAX_SESSION_FILES: usize = 128;
/// Start reclaiming old active-session helpers before the hard file cap.
pub const ACTIVE_SESSION_PRUNE_TRIGGER_FILES: usize = MAX_SESSION_FILES * 3 / 4;
/// Stop file-count reclamation once the session has comfortable headroom.
pub const ACTIVE_SESSION_PRUNE_TARGET_FILES: usize = MAX_SESSION_FILES / 2;
/// Start reclaiming old active-session helpers before the hard byte cap.
pub const ACTIVE_SESSION_PRUNE_TRIGGER_BYTES: u64 = MAX_SESSION_BYTES * 3 / 4;
/// Stop byte reclamation once the session has comfortable headroom.
pub const ACTIVE_SESSION_PRUNE_TARGET_BYTES: u64 = MAX_SESSION_BYTES / 2;
/// Derived syntax maps are intentionally much smaller than canonical maps.
pub const MAX_DERIVED_CODE_MAP_PAYLOAD_BYTES: usize = 12 * 1024;
/// A session should reuse a small working set instead of minting new carriers.
pub const MAX_DERIVED_CODE_MAP_UNIQUE_FILES: usize = 24;
/// Bound aggregate helper-map churn while leaving canonical `CodeMap` unlimited.
pub const MAX_DERIVED_CODE_MAP_CALLS: usize = 64;
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
    pub max_session_bytes: u64,
    pub max_file_bytes: u64,
    pub max_session_files: usize,
    pub stale_pruned_files: usize,
    pub stale_pruned_bytes: u64,
}

/// Bounded filesystem audit attached to Bash results. It contains counts and
/// sizes only, never helper contents.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TemporaryArtifactUsage {
    pub temporary_artifact_dir: PathBuf,
    pub total_bytes: u64,
    pub max_total_bytes: u64,
    pub session_bytes: u64,
    pub max_session_bytes: u64,
    pub session_files: usize,
    pub max_session_files: usize,
    pub largest_file_bytes: u64,
    pub max_file_bytes: u64,
    pub stale_pruned_files: usize,
    pub stale_pruned_bytes: u64,
    pub over_budget: bool,
}

impl TemporaryArtifactUsage {
    pub fn requires_cleanup(&self) -> bool {
        self.over_budget
    }

    fn as_budget_error(&self) -> WinxError {
        WinxError::TemporaryArtifactBudgetExceeded {
            temporary_artifact_dir: self.temporary_artifact_dir.clone(),
            total_bytes: self.total_bytes,
            max_total_bytes: self.max_total_bytes,
            session_bytes: self.session_bytes,
            max_session_bytes: self.max_session_bytes,
            session_files: self.session_files,
            max_session_files: self.max_session_files,
            largest_file_bytes: self.largest_file_bytes,
            max_file_bytes: self.max_file_bytes,
        }
    }
}

/// In-memory budget for syntax navigation over non-canonical helpers.
///
/// It deliberately resets with the server process. Filesystem quotas remain the
/// durable backstop across restarts, while this keeps the hot session path free
/// from another persistence write after every read-only `CodeMap` call.
#[derive(Clone, Debug, Default)]
pub struct DerivedCodeMapUsage {
    mapped_files: BTreeSet<PathBuf>,
    calls: usize,
}

/// Counters returned with an accepted derived-helper map.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DerivedCodeMapPermit {
    pub calls_used: usize,
    pub calls_limit: usize,
    pub unique_files_used: usize,
    pub unique_files_limit: usize,
}

/// A resolved edit and its size change, used to validate a `MultiFileEdit`
/// against the workspace-wide quota before any file is committed.
#[derive(Clone, Copy, Debug)]
pub struct TempEdit<'a> {
    pub path: &'a Path,
    pub previous_bytes: u64,
    pub new_bytes: u64,
    pub is_new: bool,
}

/// Compute the stable session directory without touching the filesystem.
pub fn session_info(workspace_root: &Path, thread_id: &str) -> AgentTempInfo {
    let workspace = workspace_root.canonicalize().unwrap_or_else(|_| workspace_root.to_path_buf());
    AgentTempInfo {
        directory: workspace.join(TEMP_ROOT).join(session_name(thread_id)),
        ttl_seconds: MAX_AGE.as_secs(),
        max_total_bytes: MAX_TOTAL_BYTES,
        max_session_bytes: MAX_SESSION_BYTES,
        max_file_bytes: MAX_FILE_BYTES,
        max_session_files: MAX_SESSION_FILES,
        stale_pruned_files: 0,
        stale_pruned_bytes: 0,
    }
}

/// Return the session policy and best-effort prune expired sibling sessions and
/// stale active-session helpers when the active session crosses a high-water mark.
/// The active directory is not created here; `FileWriteOrEdit` creates it on
/// demand through its normal parent-directory path.
pub fn prepare_session(workspace_root: &Path, thread_id: &str) -> AgentTempInfo {
    let mut info = session_info(workspace_root, thread_id);
    let _guard = TEMP_IO_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    prune_sessions_unlocked(workspace_root, &info.directory, MAX_AGE);
    let pruned = prune_active_session_unlocked(&info.directory, MAX_AGE);
    info.stale_pruned_files = pruned.files;
    info.stale_pruned_bytes = pruned.bytes;
    log_active_prune(&info.directory, pruned);
    info
}

/// Measure the active managed session after a Bash action. This closes the gap
/// left by dynamic loops, scripts, and generated filenames that static command
/// inspection cannot project before the shell runs.
pub fn audit_session(workspace_root: &Path, thread_id: &str) -> Result<TemporaryArtifactUsage> {
    audit_session_impl(workspace_root, thread_id, false)
}

/// Reclaim stale active-session helpers at the configured high-water mark,
/// then return the same bounded audit used after Bash execution.
pub fn maintain_and_audit_session(
    workspace_root: &Path,
    thread_id: &str,
) -> Result<TemporaryArtifactUsage> {
    audit_session_impl(workspace_root, thread_id, true)
}

fn audit_session_impl(
    workspace_root: &Path,
    thread_id: &str,
    maintain: bool,
) -> Result<TemporaryArtifactUsage> {
    let info = session_info(workspace_root, thread_id);
    let workspace = workspace_root.canonicalize().unwrap_or_else(|_| workspace_root.to_path_buf());
    let _guard = TEMP_IO_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let pruned = if maintain {
        prune_sessions_unlocked(workspace_root, &info.directory, MAX_AGE);
        prune_active_session_unlocked(&info.directory, MAX_AGE)
    } else {
        PrunedArtifacts::default()
    };
    log_active_prune(&info.directory, pruned);
    let usage = temp_tree_usage(&workspace.join(TEMP_ROOT), &info.directory).ok_or_else(|| {
        policy_error(
            &info.directory,
            &info,
            "temporary storage could not be measured safely; remove malformed or excessive \
             artifacts before retrying"
                .to_string(),
        )
    })?;
    let over_budget = usage.total_bytes > MAX_TOTAL_BYTES
        || usage.session_bytes > MAX_SESSION_BYTES
        || usage.session_files > MAX_SESSION_FILES
        || usage.largest_file_bytes > MAX_FILE_BYTES;
    Ok(TemporaryArtifactUsage {
        temporary_artifact_dir: info.directory,
        total_bytes: usage.total_bytes,
        max_total_bytes: MAX_TOTAL_BYTES,
        session_bytes: usage.session_bytes,
        max_session_bytes: MAX_SESSION_BYTES,
        session_files: usage.session_files,
        max_session_files: MAX_SESSION_FILES,
        largest_file_bytes: usage.largest_file_bytes,
        max_file_bytes: MAX_FILE_BYTES,
        stale_pruned_files: pruned.files,
        stale_pruned_bytes: pruned.bytes,
        over_budget,
    })
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
    let recovery_command = is_explicit_temp_recovery(command, &info);
    let usage = match maintain_and_audit_session(workspace_root, thread_id) {
        Ok(usage) => usage,
        // A bounded scan can deliberately fail closed for a malformed or
        // excessively large tree. Keep the narrow recovery path available or
        // the agent could never repair the condition that blocked the scan.
        Err(_) if recovery_command => return Ok(()),
        Err(error) => return Err(error),
    };
    if usage.requires_cleanup() && !recovery_command {
        return Err(usage.as_budget_error());
    }
    let mut paths = crate::utils::bash_parser::extract_static_write_paths(command);
    paths.extend(embedded_writer_paths(command));
    paths.sort();
    paths.dedup();

    let mut managed_targets = Vec::new();
    for path in paths {
        if let Some(target) = validate_bash_write_target(workspace_root, cwd, &path, &info)? {
            managed_targets.push(target);
        }
    }
    validate_known_bash_targets_quota(&info, &managed_targets, TempTreeUsage::from(&usage))?;
    Ok(())
}

/// Once a dynamic Bash command has exceeded the active-session quota, admit
/// only narrow inspection/cleanup commands until the directory is back under budget.
/// This is a recovery gate, not a shell sandbox; the normal mode allowlist still
/// authorizes the command itself.
fn is_explicit_temp_recovery(command: &str, info: &AgentTempInfo) -> bool {
    let directory = info.directory.to_string_lossy();
    let names_active_dir = command.contains("$WINX_TEMP_DIR")
        || command.contains("${WINX_TEMP_DIR}")
        || command.contains(directory.as_ref());
    if !names_active_dir {
        return false;
    }
    let Ok(commands) = crate::utils::bash_parser::extract_command_texts(command) else {
        return false;
    };
    !commands.is_empty()
        && commands.iter().all(|statement| {
            let executable = statement
                .split_whitespace()
                .next()
                .and_then(|word| Path::new(word).file_name())
                .and_then(OsStr::to_str)
                .unwrap_or_default();
            matches!(
                executable,
                "rm" | "unlink" | "rmdir" | "du" | "ls" | "stat" | "sort" | "head" | "tail" | "wc"
            ) || (executable == "find" && !statement.contains("-exec"))
        })
}

fn validate_bash_write_target(
    workspace_root: &Path,
    cwd: &Path,
    path: &str,
    info: &AgentTempInfo,
) -> Result<Option<PathBuf>> {
    let managed_env_path = expand_temp_env_path(path, info);
    let expanded: String = managed_env_path.as_ref().map_or_else(
        || crate::utils::path::expand_user(path),
        |path| path.to_string_lossy().into_owned(),
    );
    let requested = if Path::new(&expanded).is_absolute() {
        PathBuf::from(&expanded)
    } else {
        cwd.join(&expanded)
    };
    let resolved = if managed_env_path.is_some() {
        requested.clone()
    } else {
        crate::utils::path::resolve_in_workspace(path, cwd, workspace_root)
            .unwrap_or_else(|_| requested.clone())
    };
    let workspace = workspace_root.canonicalize().unwrap_or_else(|_| workspace_root.to_path_buf());

    let requested_relative = workspace_relative(&requested, workspace_root, &workspace);
    let resolved_relative = workspace_relative(&resolved, workspace_root, &workspace);
    let mut managed_target = None;
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
            let target = workspace.join(relative);
            if target != info.directory {
                validate_session_relative_path(&target, info)?;
                managed_target = Some(target);
            }
        }
    }
    Ok(managed_target)
}

/// Expand only Winx's server-owned temporary path variable. Other shell
/// expansions remain dynamic and are intentionally left to the shell.
fn expand_temp_env_path(path: &str, info: &AgentTempInfo) -> Option<PathBuf> {
    let path = path.trim();
    let path = if path.len() >= 2
        && matches!(
            (path.as_bytes().first(), path.as_bytes().last()),
            (Some(b'\''), Some(b'\'')) | (Some(b'"'), Some(b'"'))
        ) {
        &path[1..path.len() - 1]
    } else {
        path
    };
    let suffix =
        path.strip_prefix("$WINX_TEMP_DIR").or_else(|| path.strip_prefix("${WINX_TEMP_DIR}"))?;
    if !suffix.is_empty() && !suffix.starts_with('/') {
        return None;
    }
    let suffix = suffix.trim_start_matches('/');
    Some(if suffix.is_empty() { info.directory.clone() } else { info.directory.join(suffix) })
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

    quoted_literals(command)
        .into_iter()
        .filter(|literal| literal.contains(".winx") || literal.contains("$WINX_TEMP_DIR"))
        .collect()
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

/// Enforce the session boundary for an explicit `CodeMap` path.
///
/// Returns `true` only for a helper in the active session. A request spelling a
/// temporary path that resolves elsewhere (for example through a symlink) is
/// rejected instead of being misclassified as canonical source.
pub fn validate_code_map_target(
    workspace_root: &Path,
    thread_id: &str,
    requested_path: &Path,
    resolved_path: &Path,
) -> Result<bool> {
    let info = session_info(workspace_root, thread_id);
    let workspace = workspace_root.canonicalize().unwrap_or_else(|_| workspace_root.to_path_buf());
    let requested_relative = workspace_relative(requested_path, workspace_root, &workspace);
    let resolved_relative = workspace_relative(resolved_path, workspace_root, &workspace);
    let requested_temp = requested_relative.as_deref().is_some_and(is_temp_relative);
    let resolved_temp = resolved_relative.as_deref().is_some_and(is_temp_relative);
    if !requested_temp && !resolved_temp {
        return Ok(false);
    }

    reject_symlinked_temp_root(&workspace, requested_path, &info)?;
    if requested_temp != resolved_temp {
        return Err(policy_error(
            requested_path,
            &info,
            "a temporary helper must not resolve through a symlink outside its managed session"
                .to_string(),
        ));
    }

    let requested_policy = requested_relative
        .as_deref()
        .map_or_else(|| requested_path.to_path_buf(), |relative| workspace.join(relative));
    let resolved_policy = resolved_relative
        .as_deref()
        .map_or_else(|| resolved_path.to_path_buf(), |relative| workspace.join(relative));
    if requested_policy != resolved_policy {
        return Err(policy_error(
            requested_path,
            &info,
            "temporary helper paths must resolve directly without aliases, parent traversal, or symlinks"
                .to_string(),
        ));
    }
    if requested_policy != info.directory {
        validate_session_relative_path(&requested_policy, &info)?;
    }
    Ok(true)
}

/// Reserve one syntax-navigation call over an active non-canonical helper.
pub fn reserve_derived_code_map(
    usage: &mut DerivedCodeMapUsage,
    path: &Path,
    info: &AgentTempInfo,
) -> Result<DerivedCodeMapPermit> {
    let is_new = !usage.mapped_files.contains(path);
    let unique_files = usage.mapped_files.len();
    if usage.calls >= MAX_DERIVED_CODE_MAP_CALLS
        || (is_new && unique_files >= MAX_DERIVED_CODE_MAP_UNIQUE_FILES)
    {
        return Err(WinxError::DerivedCodeMapBudget {
            path: path.to_path_buf(),
            temporary_artifact_dir: info.directory.clone(),
            calls_used: usage.calls,
            calls_limit: MAX_DERIVED_CODE_MAP_CALLS,
            unique_files_used: unique_files,
            unique_files_limit: MAX_DERIVED_CODE_MAP_UNIQUE_FILES,
            message: format!(
                "this session already used {} derived-helper map calls across {} unique files; \
                 reuse prior results and inspect canonical source with CodeMap/ReadFiles or rg \
                 instead of creating another carrier",
                usage.calls, unique_files
            ),
        });
    }

    usage.calls += 1;
    usage.mapped_files.insert(path.to_path_buf());
    Ok(DerivedCodeMapPermit {
        calls_used: usage.calls,
        calls_limit: MAX_DERIVED_CODE_MAP_CALLS,
        unique_files_used: usage.mapped_files.len(),
        unique_files_limit: MAX_DERIVED_CODE_MAP_UNIQUE_FILES,
    })
}

fn validate_known_bash_targets_quota(
    info: &AgentTempInfo,
    targets: &[PathBuf],
    current: TempTreeUsage,
) -> Result<()> {
    if targets.is_empty() {
        return Ok(());
    }
    let new_files = targets.iter().filter(|path| !path.exists()).collect::<BTreeSet<_>>().len();
    let projected =
        TempTreeUsage { session_files: current.session_files.saturating_add(new_files), ..current };
    validate_projected_usage(&targets[0], info, current, projected)
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
    let pruned = prune_active_session_unlocked(&info.directory, MAX_AGE);
    log_active_prune(&info.directory, pruned);
    let current =
        temp_tree_usage(&workspace.join(TEMP_ROOT), &info.directory).ok_or_else(|| {
            policy_error(
                representative,
                &info,
                "temporary storage could not be measured safely; remove malformed or excessive \
             artifacts before retrying"
                    .to_string(),
            )
        })?;
    let mut projected_total_bytes = current.total_bytes;
    let mut projected_session_bytes = current.session_bytes;
    let mut projected_session_files = current.session_files;
    for edit in managed {
        projected_total_bytes = projected_total_bytes
            .saturating_sub(edit.previous_bytes)
            .saturating_add(edit.new_bytes);
        projected_session_bytes = projected_session_bytes
            .saturating_sub(edit.previous_bytes)
            .saturating_add(edit.new_bytes);
        if edit.is_new {
            projected_session_files = projected_session_files.saturating_add(1);
        }
    }
    validate_projected_usage(
        representative,
        &info,
        current,
        TempTreeUsage {
            total_bytes: projected_total_bytes,
            session_bytes: projected_session_bytes,
            session_files: projected_session_files,
            largest_file_bytes: current.largest_file_bytes,
        },
    )
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

fn validate_projected_usage(
    path: &Path,
    info: &AgentTempInfo,
    current: TempTreeUsage,
    projected: TempTreeUsage,
) -> Result<()> {
    let violation =
        if projected.total_bytes > MAX_TOTAL_BYTES && projected.total_bytes > current.total_bytes {
            Some(format!(
                "this write would grow managed temporary storage to {} bytes; the workspace-wide \
             limit is {MAX_TOTAL_BYTES} bytes",
                projected.total_bytes
            ))
        } else if projected.session_bytes > MAX_SESSION_BYTES
            && projected.session_bytes > current.session_bytes
        {
            Some(format!(
                "this write would grow the active session's temporary storage to {} bytes; the \
             per-session limit is {MAX_SESSION_BYTES} bytes",
                projected.session_bytes
            ))
        } else if projected.session_files > MAX_SESSION_FILES
            && projected.session_files > current.session_files
        {
            Some(format!(
            "this write would grow the active session to {} helper files; the per-session limit \
             is {MAX_SESSION_FILES}. Reuse or overwrite a stable helper and remove obsolete \
             carriers instead of creating another file",
            projected.session_files
        ))
        } else {
            None
        };
    match violation {
        Some(message) => Err(policy_error(
            path,
            info,
            format!(
                "{message}. Shrinking existing helpers remains allowed so the session can recover"
            ),
        )),
        None => Ok(()),
    }
}

fn policy_error(path: &Path, info: &AgentTempInfo, message: String) -> WinxError {
    WinxError::TemporaryArtifactPolicy {
        path: path.to_path_buf(),
        temporary_artifact_dir: info.directory.clone(),
        message,
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct TempTreeUsage {
    total_bytes: u64,
    session_bytes: u64,
    session_files: usize,
    largest_file_bytes: u64,
}

impl From<&TemporaryArtifactUsage> for TempTreeUsage {
    fn from(usage: &TemporaryArtifactUsage) -> Self {
        Self {
            total_bytes: usage.total_bytes,
            session_bytes: usage.session_bytes,
            session_files: usage.session_files,
            largest_file_bytes: usage.largest_file_bytes,
        }
    }
}

fn temp_tree_usage(root: &Path, active_session: &Path) -> Option<TempTreeUsage> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(TempTreeUsage::default())
        }
        Err(_) => return None,
    };
    if metadata.file_type().is_symlink() {
        return None;
    }

    let mut usage = TempTreeUsage::default();
    let mut seen = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((path, depth)) = stack.pop() {
        if seen >= MAX_SCAN_ENTRIES || depth > MAX_SCAN_DEPTH {
            return None;
        }
        seen += 1;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        // Count symlinks and special nodes as artifacts without following
        // them; otherwise Bash could evade the file-count budget cheaply.
        if !metadata.is_dir() {
            usage.total_bytes = usage.total_bytes.saturating_add(metadata.len());
            if path.starts_with(active_session) {
                usage.session_bytes = usage.session_bytes.saturating_add(metadata.len());
                usage.session_files = usage.session_files.saturating_add(1);
                usage.largest_file_bytes = usage.largest_file_bytes.max(metadata.len());
            }
            continue;
        }
        if metadata.is_dir() {
            let entries = match fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return None,
            };
            for entry in entries {
                match entry {
                    Ok(entry) => stack.push((entry.path(), depth + 1)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return None,
                }
            }
        }
    }
    Some(usage)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PrunedArtifacts {
    files: usize,
    bytes: u64,
}

#[derive(Debug)]
struct PrunableArtifact {
    path: PathBuf,
    modified: SystemTime,
    bytes: u64,
    is_file: bool,
    is_symlink: bool,
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn artifact_identity(metadata: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;

    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn artifact_identity(_metadata: &fs::Metadata) -> (u64, u64) {
    (0, 0)
}

fn still_matches_prunable_artifact(candidate: &PrunableArtifact, cutoff: SystemTime) -> bool {
    let Ok(metadata) = fs::symlink_metadata(&candidate.path) else { return false };
    let Ok(modified) = metadata.modified() else { return false };
    let (device, inode) = artifact_identity(&metadata);
    modified == candidate.modified
        && modified < cutoff
        && metadata.len() == candidate.bytes
        && metadata.is_file() == candidate.is_file
        && metadata.file_type().is_symlink() == candidate.is_symlink
        && device == candidate.device
        && inode == candidate.inode
}

/// Reclaim only old, server-managed artifacts from the active session. The
/// high-water/target split avoids scanning-triggered deletion churn near the
/// hard cap. Symlinks and special nodes are unlinked but never followed; any
/// uncertain or excessive tree is retained unchanged.
fn prune_active_session_unlocked(active: &Path, max_age: Duration) -> PrunedArtifacts {
    let metadata = match fs::symlink_metadata(active) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return PrunedArtifacts::default();
        }
        Err(_) => return PrunedArtifacts::default(),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return PrunedArtifacts::default();
    }

    let cutoff = SystemTime::now().checked_sub(max_age).unwrap_or(UNIX_EPOCH);
    let mut seen = 0usize;
    let mut stack = vec![(active.to_path_buf(), 0usize)];
    let mut candidates = Vec::new();
    let mut session_files = 0usize;
    let mut session_bytes = 0u64;
    let mut largest_file_bytes = 0u64;

    while let Some((path, depth)) = stack.pop() {
        if seen >= MAX_SCAN_ENTRIES || depth > MAX_SCAN_DEPTH {
            return PrunedArtifacts::default();
        }
        seen += 1;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return PrunedArtifacts::default(),
        };
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            let entries = match fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return PrunedArtifacts::default(),
            };
            for entry in entries {
                match entry {
                    Ok(entry) => stack.push((entry.path(), depth + 1)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return PrunedArtifacts::default(),
                }
            }
            continue;
        }

        session_files = session_files.saturating_add(1);
        session_bytes = session_bytes.saturating_add(metadata.len());
        largest_file_bytes = largest_file_bytes.max(metadata.len());
        if let Ok(modified) = metadata.modified() {
            if modified < cutoff {
                let (device, inode) = artifact_identity(&metadata);
                candidates.push(PrunableArtifact {
                    path,
                    modified,
                    bytes: metadata.len(),
                    is_file: metadata.is_file(),
                    is_symlink: metadata.file_type().is_symlink(),
                    device,
                    inode,
                });
            }
        }
    }

    let crossed_high_water = session_files >= ACTIVE_SESSION_PRUNE_TRIGGER_FILES
        || session_bytes >= ACTIVE_SESSION_PRUNE_TRIGGER_BYTES
        || largest_file_bytes > MAX_FILE_BYTES;
    if !crossed_high_water || candidates.is_empty() {
        return PrunedArtifacts::default();
    }

    candidates.sort_by(|left, right| {
        left.modified.cmp(&right.modified).then_with(|| left.path.cmp(&right.path))
    });
    let mut pruned = PrunedArtifacts::default();
    for candidate in candidates {
        let needs_headroom = session_files > ACTIVE_SESSION_PRUNE_TARGET_FILES
            || session_bytes > ACTIVE_SESSION_PRUNE_TARGET_BYTES;
        if !needs_headroom && candidate.bytes <= MAX_FILE_BYTES {
            continue;
        }
        if !still_matches_prunable_artifact(&candidate, cutoff) {
            continue;
        }
        match fs::remove_file(&candidate.path) {
            Ok(()) => {
                session_files = session_files.saturating_sub(1);
                session_bytes = session_bytes.saturating_sub(candidate.bytes);
                pruned.files = pruned.files.saturating_add(1);
                pruned.bytes = pruned.bytes.saturating_add(candidate.bytes);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => debug!(
                path = %candidate.path.display(),
                %error,
                "failed to prune stale active-session helper"
            ),
        }
    }

    pruned
}

fn log_active_prune(active: &Path, pruned: PrunedArtifacts) {
    if pruned.files > 0 {
        info!(
            temporary_artifact_dir = %active.display(),
            stale_pruned_files = pruned.files,
            stale_pruned_bytes = pruned.bytes,
            "pruned stale active-session temporary artifacts at high water mark"
        );
    }
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
    fn active_session_prunes_only_stale_helpers_after_high_water_mark() {
        let workspace = TempDir::new().unwrap();
        let active = session_info(workspace.path(), "active").directory;
        fs::create_dir_all(active.join("old/nested")).unwrap();
        for index in 0..ACTIVE_SESSION_PRUNE_TRIGGER_FILES {
            fs::write(active.join("old/nested").join(format!("old-{index}.txt")), "old").unwrap();
        }
        sleep(Duration::from_millis(15));
        let fresh =
            (0..8).map(|index| active.join(format!("fresh-{index}.txt"))).collect::<Vec<_>>();
        for path in &fresh {
            fs::write(path, "fresh").unwrap();
        }

        let _guard = TEMP_IO_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let pruned = prune_active_session_unlocked(&active, Duration::from_millis(5));
        assert_eq!(pruned.files, 40);
        // Mirror production (`audit_session_impl`): the scan root must come
        // from the canonicalized workspace, because `active` is canonical too.
        // A literal root diverges when the temp dir sits behind a symlink
        // (containerized CI), making `starts_with(active)` count zero session
        // files while the helpers all still exist.
        let usage_root = workspace
            .path()
            .canonicalize()
            .unwrap_or_else(|_| workspace.path().to_path_buf())
            .join(TEMP_ROOT);
        assert_eq!(
            temp_tree_usage(&usage_root, &active).unwrap().session_files,
            ACTIVE_SESSION_PRUNE_TARGET_FILES
        );
        assert!(fresh.iter().all(|path| path.exists()), "fresh helpers must be retained");
    }

    #[test]
    fn active_session_does_not_prune_stale_helpers_below_high_water_mark() {
        let workspace = TempDir::new().unwrap();
        let active = session_info(workspace.path(), "active").directory;
        fs::create_dir_all(&active).unwrap();
        for index in 0..(ACTIVE_SESSION_PRUNE_TRIGGER_FILES - 1) {
            fs::write(active.join(format!("helper-{index}.txt")), "old").unwrap();
        }
        sleep(Duration::from_millis(10));

        let _guard = TEMP_IO_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let pruned = prune_active_session_unlocked(&active, Duration::from_millis(1));
        assert_eq!(pruned, PrunedArtifacts::default());
        assert_eq!(fs::read_dir(active).unwrap().count(), ACTIVE_SESSION_PRUNE_TRIGGER_FILES - 1);
    }

    #[test]
    fn stale_candidate_revalidation_rejects_a_fresh_replacement() {
        let workspace = TempDir::new().unwrap();
        let path = workspace.path().join("helper.txt");
        fs::write(&path, "old").unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let modified = metadata.modified().unwrap();
        let (device, inode) = artifact_identity(&metadata);
        let candidate = PrunableArtifact {
            path: path.clone(),
            modified,
            bytes: metadata.len(),
            is_file: metadata.is_file(),
            is_symlink: metadata.file_type().is_symlink(),
            device,
            inode,
        };
        sleep(Duration::from_millis(10));
        fs::write(&path, "fresh replacement").unwrap();

        assert!(!still_matches_prunable_artifact(&candidate, SystemTime::now()));
        assert_eq!(fs::read_to_string(path).unwrap(), "fresh replacement");
    }

    #[cfg(unix)]
    #[test]
    fn active_session_pruning_never_follows_symlink_helpers() {
        use std::os::unix::fs::symlink;

        let workspace = TempDir::new().unwrap();
        let active = session_info(workspace.path(), "active").directory;
        let outside = workspace.path().join("canonical.txt");
        fs::create_dir_all(&active).unwrap();
        fs::write(&outside, "keep").unwrap();
        for index in 0..ACTIVE_SESSION_PRUNE_TRIGGER_FILES {
            symlink(&outside, active.join(format!("link-{index}"))).unwrap();
        }
        sleep(Duration::from_millis(10));

        let _guard = TEMP_IO_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let pruned = prune_active_session_unlocked(&active, Duration::from_millis(1));
        assert!(pruned.files > 0);
        assert_eq!(fs::read_to_string(outside).unwrap(), "keep");
    }

    #[test]
    fn quota_projection_accounts_for_replacements() {
        let workspace = TempDir::new().unwrap();
        let info = session_info(workspace.path(), "active");
        let path = info.directory.join("large.bin");
        let current = TempTreeUsage::default();
        assert!(validate_projected_usage(
            &path,
            &info,
            current,
            TempTreeUsage { total_bytes: MAX_TOTAL_BYTES, ..current }
        )
        .is_ok());
        assert!(validate_projected_usage(
            &path,
            &info,
            current,
            TempTreeUsage { total_bytes: MAX_TOTAL_BYTES + 1, ..current }
        )
        .is_err());

        let oversized = TempTreeUsage {
            total_bytes: MAX_TOTAL_BYTES + 10,
            session_bytes: MAX_SESSION_BYTES + 10,
            session_files: MAX_SESSION_FILES + 10,
            largest_file_bytes: MAX_FILE_BYTES + 10,
        };
        let shrinking = TempTreeUsage {
            total_bytes: oversized.total_bytes - 1,
            session_bytes: oversized.session_bytes - 1,
            session_files: oversized.session_files,
            largest_file_bytes: oversized.largest_file_bytes,
        };
        validate_projected_usage(&path, &info, oversized, shrinking)
            .expect("an over-limit session must be allowed to shrink existing helpers");
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
            .map(|path| TempEdit {
                path,
                previous_bytes: 0,
                new_bytes: MAX_FILE_BYTES,
                is_new: true,
            })
            .collect::<Vec<_>>();

        let error = validate_batch_quota(workspace.path(), "active", &edits)
            .expect_err("aggregate batch must honor the workspace quota");
        assert!(error.to_string().contains("workspace-wide limit"), "{error}");
    }

    #[test]
    fn literal_winx_temp_destinations_honor_the_session_file_cap() {
        let workspace = TempDir::new().unwrap();
        let info = session_info(workspace.path(), "active");
        fs::create_dir_all(&info.directory).unwrap();
        for index in 0..MAX_SESSION_FILES {
            fs::write(info.directory.join(format!("helper-{index}.txt")), "x").unwrap();
        }

        validate_bash_command(
            workspace.path(),
            workspace.path(),
            "active",
            "printf replacement > \"$WINX_TEMP_DIR/helper-0.txt\"",
        )
        .expect("overwriting a stable helper must remain possible at the cap");
        let error = validate_bash_command(
            workspace.path(),
            workspace.path(),
            "active",
            "printf new > \"$WINX_TEMP_DIR/one-more.txt\"",
        )
        .expect_err("a visible new shell helper must honor the cap");
        assert!(error.to_string().contains("helper files"), "{error}");
    }

    #[test]
    fn dynamic_bash_overflow_blocks_work_until_explicit_cleanup() {
        let workspace = TempDir::new().unwrap();
        let info = session_info(workspace.path(), "active");
        fs::create_dir_all(&info.directory).unwrap();
        for index in 0..=MAX_SESSION_FILES {
            fs::write(info.directory.join(format!("dynamic-{index}.txt")), "x").unwrap();
        }

        let usage = audit_session(workspace.path(), "active").unwrap();
        assert!(usage.over_budget);
        assert!(usage.requires_cleanup());
        let error =
            validate_bash_command(workspace.path(), workspace.path(), "active", "cargo test --lib")
                .expect_err(
                    "ordinary commands must pause while the active temp session is over budget",
                );
        assert!(matches!(error, WinxError::TemporaryArtifactBudgetExceeded { .. }));

        validate_bash_command(
            workspace.path(),
            workspace.path(),
            "active",
            "find \"$WINX_TEMP_DIR\" -type f -delete",
        )
        .expect("an explicit cleanup-only command must remain available");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_helpers_count_toward_the_session_artifact_budget() {
        use std::os::unix::fs::symlink;

        let workspace = TempDir::new().unwrap();
        let info = session_info(workspace.path(), "active");
        fs::create_dir_all(&info.directory).unwrap();
        for index in 0..=MAX_SESSION_FILES {
            symlink("missing-target", info.directory.join(format!("link-{index}"))).unwrap();
        }

        let usage = audit_session(workspace.path(), "active").unwrap();
        assert_eq!(usage.session_files, MAX_SESSION_FILES + 1);
        assert!(usage.requires_cleanup());
    }

    #[test]
    fn code_map_accepts_only_the_active_direct_helper_path() {
        let workspace = TempDir::new().unwrap();
        let active = session_info(workspace.path(), "active");
        let helper = active.directory.join("adapter.py");
        assert!(validate_code_map_target(workspace.path(), "active", &helper, &helper).unwrap());

        let foreign = session_info(workspace.path(), "other").directory.join("adapter.py");
        let error = validate_code_map_target(workspace.path(), "active", &foreign, &foreign)
            .expect_err("another session's helper must not be mapped");
        assert!(error.to_string().contains("session-scoped"), "{error}");

        let canonical = workspace.path().join("src/lib.rs");
        assert!(
            !validate_code_map_target(workspace.path(), "active", &canonical, &canonical).unwrap()
        );
    }

    #[test]
    fn derived_code_map_budget_rewards_reuse_and_caps_churn() {
        let workspace = TempDir::new().unwrap();
        let info = session_info(workspace.path(), "active");
        let mut usage = DerivedCodeMapUsage::default();
        let stable = info.directory.join("stable.py");
        let first = reserve_derived_code_map(&mut usage, &stable, &info).unwrap();
        let repeated = reserve_derived_code_map(&mut usage, &stable, &info).unwrap();
        assert_eq!(first.unique_files_used, 1);
        assert_eq!(repeated.unique_files_used, 1);
        assert_eq!(repeated.calls_used, 2);

        for index in 1..MAX_DERIVED_CODE_MAP_UNIQUE_FILES {
            reserve_derived_code_map(
                &mut usage,
                &info.directory.join(format!("helper-{index}.py")),
                &info,
            )
            .unwrap();
        }
        let error =
            reserve_derived_code_map(&mut usage, &info.directory.join("one-too-many.py"), &info)
                .expect_err("a new carrier past the unique-file budget must fail");
        assert!(matches!(error, WinxError::DerivedCodeMapBudget { .. }));

        let mut repeated_usage = DerivedCodeMapUsage::default();
        for _ in 0..MAX_DERIVED_CODE_MAP_CALLS {
            reserve_derived_code_map(&mut repeated_usage, &stable, &info).unwrap();
        }
        let error = reserve_derived_code_map(&mut repeated_usage, &stable, &info)
            .expect_err("even one stable helper has an aggregate call budget");
        assert!(matches!(error, WinxError::DerivedCodeMapBudget { .. }));
    }
}
