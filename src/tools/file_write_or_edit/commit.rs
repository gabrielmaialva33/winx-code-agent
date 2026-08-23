use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracing::debug;

use super::matcher::{apply_blocks_with_unescape_retry, ToleranceKind};
use super::parser::uses_search_replace;
use super::report::operation_result;
use crate::errors::{Result, WinxError};
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
}

impl PlannedEdit {
    pub(crate) fn target(&self) -> &str {
        &self.file_path_str
    }
}

pub(crate) fn plan_edit(
    bash_state: &BashState,
    file_path: &str,
    percentage_to_change: u32,
    blocks: &str,
) -> Result<PlannedEdit> {
    let expanded_path = expand_user(file_path);
    let path = if Path::new(&expanded_path).is_absolute() {
        PathBuf::from(&expanded_path)
    } else {
        bash_state.cwd.join(&expanded_path)
    };
    let path = validate_path_in_workspace(&path, &bash_state.workspace_root).map_err(|error| {
        WinxError::PathSecurityError { path: path.clone(), message: error.to_string() }
    })?;
    let file_path_str = path.to_string_lossy().to_string();

    let search_replace = uses_search_replace(percentage_to_change, blocks);
    let operation_allowed = if search_replace {
        bash_state.is_file_edit_allowed(&file_path_str)
    } else {
        bash_state.is_file_write_allowed(&file_path_str)
    };
    if !operation_allowed {
        return Err(WinxError::FileAccessError {
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
                WinxError::FileAccessError {
                    path: path.clone(),
                    message: format!(
                        "This file exists but hasn't been read in this session. Call ReadFiles on \
                     {file_path_str} first, then retry the edit (winx requires a fresh read so \
                     edits are never applied blind)."
                    ),
                }
            })?;
        if whitelist.file_hash != hash_content(original) {
            return Err(WinxError::FileAccessError {
                path,
                message: format!(
                    "{file_path_str} changed on disk since you last read it. Call ReadFiles again \
                     to get the current content, then retry the edit."
                ),
            });
        }
        if !search_replace && !whitelist.is_read_enough() {
            return Err(WinxError::FileAccessError {
                path,
                message: format!(
                    "Read more of the file before overwriting. Unread line ranges: {}",
                    format_unread_ranges(whitelist)
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

    Ok(PlannedEdit {
        path,
        file_path_str,
        action,
        new_content,
        previous,
        tolerances,
        uses_search_replace: search_replace,
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
    } = planned;

    ensure_parent_dirs(&path)?;
    write_no_follow(&path, new_content.as_bytes())?;

    if let Some(prior_content) = &previous {
        let prior_whitelist = bash_state.whitelist_for_overwrite.get(&file_path_str).cloned();
        bash_state.push_edit_checkpoint(EditCheckpoint {
            file_path_str: file_path_str.clone(),
            path: path.clone(),
            prior_content: prior_content.clone(),
            prior_whitelist,
        });
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
    );
    Ok(result)
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

/// Atomically replace a path with a same-directory temporary file, preserving
/// existing permissions and never following a target symlink.
pub(crate) fn write_no_follow(path: &Path, content: &[u8]) -> std::io::Result<()> {
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
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn hash_content(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    digest.iter().fold(String::with_capacity(digest.len() * 2), |mut hash, byte| {
        let _ = write!(hash, "{byte:02x}");
        hash
    })
}

fn format_unread_ranges(whitelist: &FileWhitelistData) -> String {
    whitelist
        .get_unread_ranges()
        .into_iter()
        .map(|(start, end)| if start == end { start.to_string() } else { format!("{start}-{end}") })
        .collect::<Vec<_>>()
        .join(", ")
}

fn refresh_whitelist_and_stats(
    bash_state: &mut BashState,
    file_path_str: &str,
    path: &Path,
    new_content: &str,
    uses_search_replace: bool,
) {
    let hash = hash_content(new_content);
    let total_lines = new_content.lines().count();
    bash_state.set_whitelist_entry(
        file_path_str,
        FileWhitelistData::new(hash, vec![(1, total_lines)], total_lines),
    );

    let (kind, stats) = if uses_search_replace {
        ("edit", crate::utils::workspace_stats::record_edit(&bash_state.workspace_root, path))
    } else {
        ("write", crate::utils::workspace_stats::record_write(&bash_state.workspace_root, path))
    };
    if let Err(error) = stats {
        debug!("failed to record {kind} stats: {error}");
    }
}
