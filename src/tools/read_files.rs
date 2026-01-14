//! Implementation of the `ReadFiles` tool.
//!
//! This module provides the implementation for the `ReadFiles` tool, which is used
//! to read and display the contents of files, optionally with line numbers and
//! line range filtering.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, instrument, warn};

use crate::errors::{ErrorRecovery, Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::ReadFiles;
use crate::utils::file_cache::FileCache;
use crate::utils::mmap::read_file_to_string;
use crate::utils::path::{expand_user, validate_path_in_workspace};

/// Default token limits for file reading
const DEFAULT_MAX_TOKENS: usize = 24000;

/// Type alias for file reading result
type FileReadResult = (String, bool, usize, String, (usize, usize));

/// Maximum amount of data to read from a file
const MAX_FILE_SIZE: u64 = 50_000_000;

fn range_format(start_line_num: Option<usize>, end_line_num: Option<usize>) -> String {
    let st = start_line_num.map_or(String::new(), |n| n.to_string());
    let end = end_line_num.map_or(String::new(), |n| n.to_string());

    if st.is_empty() && end.is_empty() {
        String::new()
    } else {
        format!(":{st}-{end}")
    }
}

#[instrument(level = "debug", skip(file_path))]
async fn read_file(
    file_path: &str,
    max_tokens: Option<usize>,
    cwd: &Path,
    workspace_root: &Path,
    show_line_numbers: bool,
    start_line_num: Option<usize>,
    end_line_num: Option<usize>,
) -> Result<FileReadResult> {
    let file_path_expanded = expand_user(file_path);
    let path = if Path::new(&file_path_expanded).is_absolute() {
        PathBuf::from(&file_path_expanded)
    } else {
        cwd.join(&file_path_expanded)
    };

    if !path.exists() {
        return Err(WinxError::FileAccessError {
            path: path.clone(),
            message: "File does not exist".to_string(),
        });
    }

    let path = match validate_path_in_workspace(&path, workspace_root) {
        Ok(canonical) => canonical,
        Err(security_err) => {
            return Err(WinxError::PathSecurityError {
                path: path.clone(),
                message: security_err.to_string(),
            });
        }
    };

    if !path.is_file() {
        return Err(WinxError::FileAccessError {
            path: path.clone(),
            message: "Path exists but is not a file".to_string(),
        });
    }

    let content = read_file_to_string(&path, MAX_FILE_SIZE)?;
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len() + usize::from(content.ends_with('\n'));

    let start_idx = start_line_num.map_or(0, |n| n.saturating_sub(1).min(lines.len()));
    let end_idx = end_line_num.map_or(lines.len(), |n| n.min(lines.len()));

    if start_idx > lines.len() || (end_idx > 0 && start_idx > end_idx) {
        return Err(ErrorRecovery::param_error(
            "line_range",
            &format!("Invalid line range for file with {} lines", lines.len()),
        ));
    }

    let effective_start = start_line_num.unwrap_or(1);
    let effective_end = end_line_num.unwrap_or(total_lines);

    let filtered_lines =
        if lines.is_empty() { &[] } else { &lines[start_idx..end_idx.min(lines.len())] };
    let mut result_content = String::new();

    if show_line_numbers {
        for (i, line) in filtered_lines.iter().enumerate() {
            let line_num = start_idx + i + 1;
            result_content.push_str(&format!("{line_num} {line}\n"));
        }
    } else {
        for line in filtered_lines {
            result_content.push_str(line);
            result_content.push('\n');
        }
    }

    let mut truncated = false;
    let tokens_count = result_content.len();
    let max_tokens = max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);

    if tokens_count > max_tokens {
        let truncation_point =
            result_content.char_indices().nth(max_tokens).map_or(result_content.len(), |(i, _)| i);
        result_content.truncate(truncation_point);
        result_content.push_str("\n(...truncated due to token limit)");
        truncated = true;
    }

    let canon_path = path.to_string_lossy().to_string();

    Ok((result_content, truncated, tokens_count, canon_path, (effective_start, effective_end)))
}

pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    read_files: ReadFiles,
) -> Result<String> {
    let (cwd, workspace_root) = {
        let bash_state_guard = bash_state_arc.lock().await;
        let bash_state = bash_state_guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        (bash_state.cwd.clone(), bash_state.workspace_root.clone())
    };

    let mut message = String::new();
    let cache = FileCache::global();
    let mut file_ranges_dict: HashMap<String, Vec<(usize, usize)>> = HashMap::new();

    for file_path in &read_files.file_paths {
        match read_file(file_path, None, &cwd, &workspace_root, true, None, None).await {
            Ok((content, truncated, _, canon_path, line_range)) => {
                file_ranges_dict.entry(canon_path.clone()).or_default().push(line_range);
                message.push_str(&format!("\n{file_path}\n```\n{content}\n```"));

                let _ = cache.record_read_range(Path::new(&canon_path), line_range.0, line_range.1);

                if truncated {
                    break;
                }
            }
            Err(e) => {
                message.push_str(&format!("\nError reading {file_path}: {e}"));
            }
        }
    }

    let mut bash_state_guard = bash_state_arc.lock().await;
    if let Some(bash_state) = bash_state_guard.as_mut() {
        for (path, ranges) in file_ranges_dict {
            let file_hash = cache.get_cached_hash(Path::new(&path)).unwrap_or_default();
            let total_lines = cache
                .get_unread_ranges(Path::new(&path))
                .iter()
                .map(|&(_, end)| end)
                .max()
                .unwrap_or(0);

            bash_state.whitelist_for_overwrite.insert(
                path.clone(),
                crate::state::bash_state::FileWhitelistData::new(file_hash, ranges, total_lines),
            );
        }
    }

    Ok(message)
}
