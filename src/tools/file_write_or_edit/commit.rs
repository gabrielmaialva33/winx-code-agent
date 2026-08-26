use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracing::debug;

use super::matcher::{apply_blocks_with_unescape_retry, ToleranceKind};
#[cfg(test)]
use super::parser::uses_search_replace;
use super::report::operation_result;
use crate::errors::{ReadRequirement, Result, WinxError};
use crate::state::bash_state::{BashState, EditCheckpoint, FileWhitelistData};
use crate::utils::path::{expand_user, validate_path_in_workspace};

pub(crate) struct PlannedEdit {
    path: PathBuf,
    file_path_str: String,
    action: &'static str,
    new_content: String,
    previous: Option<String>,
    tolerances: Vec<ToleranceKind>,
    uses_search_replace: bool,
    post_edit_read_all: bool,
}

pub(crate) fn resolve_edit_path(
    bash_state: &BashState,
    file_path: &str,
) -> Result<(PathBuf, PathBuf)> {
    let expanded_path = expand_user(file_path);
    let requested_path = if Path::new(&expanded_path).is_absolute() {
        PathBuf::from(&expanded_path)
    } else {
        bash_state.cwd.join(&expanded_path)
    };
    let path = validate_path_in_workspace(&requested_path, &bash_state.workspace_root).map_err(
        |error| WinxError::PathSecurityError {
            path: requested_path.clone(),
            message: error.to_string(),
        },
    )?;
    Ok((requested_path, path))
}

/// Revoke the exact-text evidence that led to a failed SEARCH match. The next
/// edit must be preceded by a visible `ReadFiles` call, which repopulates the
/// whitelist with the current file hash and prevents blind retry loops.
pub(crate) fn invalidate_edit_read_permit_at_target(
    bash_state: &mut BashState,
    target: &Path,
) -> bool {
    bash_state.remove_whitelist_entry(target.to_string_lossy().as_ref()).is_some()
}

impl PlannedEdit {
    pub(crate) fn target(&self) -> &str {
        &self.file_path_str
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn previous_bytes(&self) -> u64 {
        self.previous.as_ref().map_or(0, |content| content.len() as u64)
    }

    pub(crate) fn new_bytes(&self) -> u64 {
        self.new_content.len() as u64
    }

    pub(crate) fn new_revision(&self) -> String {
        crate::tools::read_files::revision_from_hash(&hash_content(&self.new_content))
    }

    pub(crate) fn new_hash(&self) -> String {
        hash_content(&self.new_content)
    }
}

#[cfg(test)]
pub(crate) fn plan_edit(
    bash_state: &BashState,
    file_path: &str,
    percentage_to_change: u32,
    blocks: &str,
    validate_temp_quota: bool,
) -> Result<PlannedEdit> {
    let search_replace = uses_search_replace(percentage_to_change, blocks);
    plan_explicit_text_edit(bash_state, file_path, search_replace, blocks, validate_temp_quota)
}

/// Plan a text edit whose semantic mode was selected explicitly by the typed
/// `EditFiles` domain layer. Legacy callers continue to use `plan_edit`, which
/// preserves the historical percentage/marker heuristic exactly.
#[cfg(test)]
fn plan_explicit_text_edit(
    bash_state: &BashState,
    file_path: &str,
    search_replace: bool,
    blocks: &str,
    validate_temp_quota: bool,
) -> Result<PlannedEdit> {
    let (requested_path, path) = resolve_edit_path(bash_state, file_path)?;
    plan_explicit_text_edit_resolved(
        bash_state,
        &requested_path,
        path,
        search_replace,
        blocks,
        validate_temp_quota,
    )
}

/// Plan against the exact canonical target captured by unified edit preflight.
/// This deliberately performs no path/cwd/symlink resolution.
pub(crate) fn plan_explicit_text_edit_at_target(
    bash_state: &BashState,
    target: &Path,
    search_replace: bool,
    blocks: &str,
    validate_temp_quota: bool,
) -> Result<PlannedEdit> {
    plan_explicit_text_edit_resolved(
        bash_state,
        target,
        target.to_path_buf(),
        search_replace,
        blocks,
        validate_temp_quota,
    )
}

fn plan_explicit_text_edit_resolved(
    bash_state: &BashState,
    requested_path: &Path,
    path: PathBuf,
    search_replace: bool,
    blocks: &str,
    validate_temp_quota: bool,
) -> Result<PlannedEdit> {
    let file_path_str = path.to_string_lossy().to_string();
    let operation_allowed = if search_replace {
        bash_state.is_file_edit_allowed(&file_path_str)
    } else {
        bash_state.is_file_write_allowed(&file_path_str)
    };
    if !operation_allowed {
        return Err(WinxError::FileOperationDenied {
            path,
            message: "File operation not allowed in current mode.".to_string(),
        });
    }

    // These exact bytes feed the hash gate, matcher and diff. Reading once avoids
    // applying an edit to content that was never validated.
    let previous = if path.exists() {
        Some(fs::read_to_string(&path).map_err(|error| WinxError::FileAccessError {
            path: path.clone(),
            message: format!("reading existing file before edit: {error}"),
        })?)
    } else {
        None
    };

    if let Some(original) = previous.as_deref() {
        let whitelist =
            bash_state.whitelist_for_overwrite.get(&file_path_str).ok_or_else(|| {
                WinxError::FileReadRequired {
                    path: path.clone(),
                    reason: ReadRequirement::NeverRead,
                    ranges: Vec::new(),
                    message: format!(
                        "This file exists but hasn't been read in this session. Call ReadFiles on \
                     {file_path_str} first, then retry the edit (winx requires a fresh read so \
                     edits are never applied blind)."
                    ),
                }
            })?;
        if whitelist.file_hash != hash_content(original) {
            return Err(WinxError::FileReadRequired {
                path,
                reason: ReadRequirement::Stale,
                ranges: Vec::new(),
                message: format!(
                    "{file_path_str} changed on disk since you last read it. Call ReadFiles again \
                     to get the current content, then retry the edit."
                ),
            });
        }
        if !search_replace && !whitelist.is_read_enough() {
            let ranges = unread_ranges(whitelist);
            return Err(WinxError::FileReadRequired {
                path,
                reason: ReadRequirement::InsufficientCoverage,
                ranges: ranges.clone(),
                message: format!(
                    "Read more of the file before overwriting. Unread line ranges: {}",
                    ranges.join(", ")
                ),
            });
        }
    }

    let (action, new_content, tolerances) = if search_replace {
        let original = previous.as_deref().unwrap_or_default();
        let (new_content, tolerances) = apply_blocks_with_unescape_retry(original, blocks)?;
        ("edited", new_content, tolerances)
    } else {
        ("wrote", blocks.to_string(), Vec::new())
    };

    let post_edit_read_all = !search_replace
        || previous.is_none()
        || bash_state.whitelist_for_overwrite[&file_path_str].is_read_enough();
    finalize_planned_edit(
        bash_state,
        requested_path,
        path,
        file_path_str,
        action,
        new_content,
        previous,
        tolerances,
        search_replace,
        post_edit_read_all,
        validate_temp_quota,
    )
}

/// Plan a line patch against an exact `ReadFiles` revision. Only lines that
/// were actually visible for that revision may be touched.
/// Revision-edit counterpart to `plan_explicit_text_edit_at_target`.
pub(crate) fn plan_revision_edit_at_target(
    bash_state: &BashState,
    target: &Path,
    expected_revision: &str,
    required_ranges: &[(usize, usize)],
    build_content: impl FnOnce(&str) -> Result<String>,
) -> Result<PlannedEdit> {
    plan_revision_edit_resolved(
        bash_state,
        target,
        target.to_path_buf(),
        expected_revision,
        required_ranges,
        build_content,
    )
}

fn plan_revision_edit_resolved(
    bash_state: &BashState,
    requested_path: &Path,
    path: PathBuf,
    expected_revision: &str,
    required_ranges: &[(usize, usize)],
    build_content: impl FnOnce(&str) -> Result<String>,
) -> Result<PlannedEdit> {
    let file_path_str = path.to_string_lossy().to_string();
    if !bash_state.is_file_edit_allowed(&file_path_str) {
        return Err(WinxError::FileOperationDenied {
            path,
            message: "File patch not allowed in current mode.".to_string(),
        });
    }
    let previous = fs::read_to_string(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => WinxError::FileNotFound { path: path.clone() },
        std::io::ErrorKind::PermissionDenied => WinxError::FileOperationDenied {
            path: path.clone(),
            message: format!("reading existing file before patch: {error}"),
        },
        _ => WinxError::FileAccessError {
            path: path.clone(),
            message: format!("reading existing file before patch: {error}"),
        },
    })?;
    let file_hash = hash_content(&previous);
    let current_revision = crate::tools::read_files::revision_from_hash(&file_hash);
    if current_revision != expected_revision {
        return Err(WinxError::FileRevisionMismatch {
            path,
            expected: expected_revision.to_string(),
            actual: current_revision,
        });
    }
    let whitelist = bash_state.whitelist_for_overwrite.get(&file_path_str).ok_or_else(|| {
        WinxError::FileReadRequired {
            path: path.clone(),
            reason: ReadRequirement::NeverRead,
            ranges: Vec::new(),
            message: "ApplyPatch requires the matching ReadFiles receipt in this session."
                .to_string(),
        }
    })?;
    if whitelist.file_hash != file_hash {
        return Err(WinxError::FileReadRequired {
            path,
            reason: ReadRequirement::Stale,
            ranges: Vec::new(),
            message: "The file changed since ReadFiles produced this revision.".to_string(),
        });
    }
    let effective_ranges = required_ranges
        .iter()
        .copied()
        .filter_map(|(start, end)| {
            if start == whitelist.total_lines.saturating_add(1) && start == end {
                (whitelist.total_lines > 0)
                    .then_some((whitelist.total_lines, whitelist.total_lines))
            } else {
                Some((start, end))
            }
        })
        .collect::<Vec<_>>();
    let unread = effective_ranges
        .iter()
        .copied()
        .filter(|(start, end)| !whitelist.covers_range(*start, *end))
        .map(|(start, end)| if start == end { start.to_string() } else { format!("{start}-{end}") })
        .collect::<Vec<_>>();
    if !unread.is_empty() {
        return Err(WinxError::FileReadRequired {
            path,
            reason: ReadRequirement::InsufficientCoverage,
            ranges: unread,
            message: "ApplyPatch may only touch lines visible in the matching ReadFiles response."
                .to_string(),
        });
    }
    let post_edit_read_all = whitelist.is_read_enough();
    let new_content = build_content(&previous)?;
    finalize_planned_edit(
        bash_state,
        requested_path,
        path,
        file_path_str,
        "patched",
        new_content,
        Some(previous),
        Vec::new(),
        true,
        post_edit_read_all,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn finalize_planned_edit(
    bash_state: &BashState,
    requested_path: &Path,
    path: PathBuf,
    file_path_str: String,
    action: &'static str,
    new_content: String,
    previous: Option<String>,
    tolerances: Vec<ToleranceKind>,
    uses_search_replace: bool,
    post_edit_read_all: bool,
    validate_temp_quota: bool,
) -> Result<PlannedEdit> {
    crate::utils::agent_temp::validate_edit_target(
        &bash_state.workspace_root,
        &bash_state.current_thread_id,
        requested_path,
        &path,
        previous.as_ref().map(|content| content.len() as u64),
        new_content.len() as u64,
    )?;
    if validate_temp_quota {
        crate::utils::agent_temp::validate_batch_quota(
            &bash_state.workspace_root,
            &bash_state.current_thread_id,
            &[crate::utils::agent_temp::TempEdit {
                path: &path,
                previous_bytes: previous.as_ref().map_or(0, |content| content.len() as u64),
                new_bytes: new_content.len() as u64,
                is_new: previous.is_none(),
            }],
        )?;
    }

    Ok(PlannedEdit {
        path,
        file_path_str,
        action,
        new_content,
        previous,
        tolerances,
        uses_search_replace,
        post_edit_read_all,
    })
}

pub(crate) fn commit_edit(bash_state: &mut BashState, planned: PlannedEdit) -> Result<String> {
    let PlannedEdit {
        path,
        file_path_str,
        action,
        new_content,
        previous,
        tolerances,
        uses_search_replace,
        post_edit_read_all,
    } = planned;

    ensure_parent_dirs(&path)?;
    write_no_follow_if_unchanged(&path, new_content.as_bytes(), previous.as_deref())?;

    if let Some(prior_content) = &previous {
        let prior_whitelist = bash_state.whitelist_for_overwrite.get(&file_path_str).cloned();
        let _ = bash_state.push_receipt_bound_edit_checkpoint(
            EditCheckpoint {
                file_path_str: file_path_str.clone(),
                path: path.clone(),
                prior_content: prior_content.clone(),
                prior_whitelist,
            },
            hash_content(&new_content),
        );
    }

    let result = operation_result(
        action,
        &file_path_str,
        &path,
        &new_content,
        &tolerances,
        previous.as_deref(),
    );
    refresh_whitelist_and_stats(
        bash_state,
        &file_path_str,
        &path,
        &new_content,
        uses_search_replace,
        post_edit_read_all,
    );
    Ok(result)
}

fn ensure_edit_precondition(path: &Path, previous: Option<&str>) -> Result<()> {
    match previous {
        Some(expected) => {
            let current = fs::read_to_string(path).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => {
                    WinxError::ConcurrentFileModification { path: path.to_path_buf(), attempts: 1 }
                }
                std::io::ErrorKind::PermissionDenied => WinxError::FileOperationDenied {
                    path: path.to_path_buf(),
                    message: format!("checking the edit precondition: {error}"),
                },
                _ => WinxError::FileAccessError {
                    path: path.to_path_buf(),
                    message: format!("checking the edit precondition: {error}"),
                },
            })?;
            if current != expected {
                return Err(WinxError::FileReadRequired {
                    path: path.to_path_buf(),
                    reason: ReadRequirement::Stale,
                    ranges: Vec::new(),
                    message: "The file changed after edit planning and before commit.".to_string(),
                });
            }
        }
        None => match fs::symlink_metadata(path) {
            Ok(_) => {
                return Err(WinxError::ConcurrentFileModification {
                    path: path.to_path_buf(),
                    attempts: 1,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(WinxError::FileAccessError {
                    path: path.to_path_buf(),
                    message: format!("checking the create precondition: {error}"),
                });
            }
        },
    }
    Ok(())
}

pub(crate) fn ensure_parent_dirs(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent).map_err(|error| WinxError::FileAccessError {
                path: path.to_path_buf(),
                message: format!("Failed to create parent directories: {error}"),
            })?;
        }
    }
    Ok(())
}

/// Prepare and synchronize the replacement, then revalidate the planned bytes
/// at the last safe point before the atomic rename.
pub(crate) fn write_no_follow_if_unchanged(
    path: &Path,
    content: &[u8],
    previous: Option<&str>,
) -> Result<()> {
    write_no_follow_if_unchanged_with_hook(path, content, previous, || {})
}

fn write_no_follow_if_unchanged_with_hook(
    path: &Path,
    content: &[u8],
    previous: Option<&str>,
    before_commit: impl FnOnce(),
) -> Result<()> {
    let temporary =
        prepare_atomic_write(path, content).map_err(|error| WinxError::FileAccessError {
            path: path.to_path_buf(),
            message: format!("preparing atomic replacement: {error}"),
        })?;
    before_commit();
    ensure_edit_precondition(path, previous)?;
    temporary.persist(path).map_err(|error| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("persisting atomic replacement: {}", error.error),
    })?;
    Ok(())
}

fn prepare_atomic_write(path: &Path, content: &[u8]) -> std::io::Result<tempfile::NamedTempFile> {
    use std::io::Error;

    let parent =
        path.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut temporary =
        tempfile::Builder::new().prefix(".winx-tmp-").tempfile_in(parent).map_err(|error| {
            Error::new(error.kind(), format!("create temp file in {}: {error}", parent.display()))
        })?;

    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_file() {
            let _ = temporary.as_file().set_permissions(metadata.permissions());
        }
    }
    temporary.write_all(content)?;
    temporary.as_file().sync_all()?;
    Ok(temporary)
}

pub(crate) fn hash_content(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    digest.iter().fold(String::with_capacity(digest.len() * 2), |mut hash, byte| {
        let _ = write!(hash, "{byte:02x}");
        hash
    })
}

fn unread_ranges(whitelist: &FileWhitelistData) -> Vec<String> {
    whitelist
        .get_unread_ranges()
        .into_iter()
        .map(|(start, end)| if start == end { start.to_string() } else { format!("{start}-{end}") })
        .collect()
}

fn refresh_whitelist_and_stats(
    bash_state: &mut BashState,
    file_path_str: &str,
    path: &Path,
    new_content: &str,
    uses_search_replace: bool,
    post_edit_read_all: bool,
) {
    let hash = hash_content(new_content);
    let total_lines = new_content.lines().count();
    if post_edit_read_all {
        bash_state.set_whitelist_entry(
            file_path_str,
            FileWhitelistData::new(hash, vec![(1, total_lines)], total_lines),
        );
    } else {
        // A partial SEARCH/range edit proves the changed region, not every
        // unseen line in the new version. Fail closed until a fresh ReadFiles.
        bash_state.remove_whitelist_entry(file_path_str);
    }

    let (kind, stats) = if uses_search_replace {
        ("edit", crate::utils::workspace_stats::record_edit(&bash_state.workspace_root, path))
    } else {
        ("write", crate::utils::workspace_stats::record_write(&bash_state.workspace_root, path))
    };
    if let Err(error) = stats {
        debug!("failed to record {kind} stats: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_change_after_temp_sync_is_not_overwritten() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("target.txt");
        fs::write(&path, "planned revision\n")?;

        let result = write_no_follow_if_unchanged_with_hook(
            &path,
            b"agent replacement\n",
            Some("planned revision\n"),
            || {
                let changed = fs::write(&path, "external revision\n");
                assert!(changed.is_ok(), "failed to update test fixture: {changed:?}");
            },
        );

        assert!(matches!(
            result,
            Err(WinxError::FileReadRequired { reason: ReadRequirement::Stale, .. })
        ));
        assert_eq!(fs::read_to_string(path)?, "external revision\n");
        Ok(())
    }

    #[test]
    fn concurrent_creation_after_temp_sync_is_not_overwritten() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("target.txt");

        let result =
            write_no_follow_if_unchanged_with_hook(&path, b"agent creation\n", None, || {
                let created = fs::write(&path, "external creation\n");
                assert!(created.is_ok(), "failed to create test fixture: {created:?}");
            });

        assert!(matches!(result, Err(WinxError::ConcurrentFileModification { .. })));
        assert_eq!(fs::read_to_string(path)?, "external creation\n");
        Ok(())
    }
}
