//! Implementation of the `FileWriteOrEdit` tool.
//!
//! This module provides the implementation for the `FileWriteOrEdit` tool, which is used
//! to write or edit files, with support for both full file content and search/replace blocks.

#![allow(clippy::unwrap_used)]
use regex::Regex;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, instrument, warn};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::{BashState, FileWhitelistData};
use crate::types::FileWriteOrEdit;
use crate::utils::path::{expand_user, validate_path_in_workspace};

fn search_marker() -> &'static Regex {
    lazy_static::lazy_static! {
        static ref REGEX: Regex = Regex::new(r"(?m)^<<<<<<< SEARCH\s*$").unwrap();
    }
    &REGEX
}

fn divider_marker() -> &'static Regex {
    lazy_static::lazy_static! {
        static ref REGEX: Regex = Regex::new(r"(?m)^=======\s*$").unwrap();
    }
    &REGEX
}

fn replace_marker() -> &'static Regex {
    lazy_static::lazy_static! {
        static ref REGEX: Regex = Regex::new(r"(?m)^>>>>>>> REPLACE\s*$").unwrap();
    }
    &REGEX
}

const MAX_FILE_SIZE: u64 = 50_000_000;

fn parse_blocks(content: &str) -> Result<Vec<(String, String)>> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        if search_marker().is_match(lines[i]) {
            i += 1;
            let mut search_lines = Vec::new();
            while i < lines.len() && !divider_marker().is_match(lines[i]) {
                search_lines.push(lines[i]);
                i += 1;
            }

            if i >= lines.len() {
                return Err(WinxError::SearchReplaceSyntaxError(
                    "Missing ======= marker".to_string(),
                ));
            }

            i += 1;
            let mut replace_lines = Vec::new();
            while i < lines.len() && !replace_marker().is_match(lines[i]) {
                replace_lines.push(lines[i]);
                i += 1;
            }

            if i >= lines.len() {
                return Err(WinxError::SearchReplaceSyntaxError(
                    "Missing >>>>>>> REPLACE marker".to_string(),
                ));
            }

            blocks.push((search_lines.join("\n"), replace_lines.join("\n")));
        }
        i += 1;
    }

    if blocks.is_empty() {
        return Err(WinxError::SearchReplaceSyntaxError("No valid blocks found".to_string()));
    }

    Ok(blocks)
}

fn apply_blocks(content: &str, blocks: Vec<(String, String)>) -> Result<String> {
    let mut result = content.to_string();
    for (search, replace) in blocks {
        if !result.contains(&search) {
            return Err(WinxError::SearchBlockNotFound(format!("Block not found: {search}")));
        }

        let count = result.matches(&search).count();
        if count > 1 {
            return Err(WinxError::SearchBlockAmbiguous {
                block_content: search,
                match_count: count,
                suggestions: vec!["Add more context to make the search block unique.".to_string()],
            });
        }

        result = result.replace(&search, &replace);
    }
    Ok(result)
}

#[instrument(level = "info", skip(bash_state_arc, file_write_or_edit))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    file_write_or_edit: FileWriteOrEdit,
) -> Result<String> {
    let mut bash_state_guard = bash_state_arc.lock().await;
    let bash_state = bash_state_guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;

    if file_write_or_edit.thread_id != bash_state.current_thread_id {
        return Err(WinxError::ThreadIdMismatch(file_write_or_edit.thread_id));
    }

    let expanded_path = expand_user(&file_write_or_edit.file_path);
    let path = if Path::new(&expanded_path).is_absolute() {
        PathBuf::from(&expanded_path)
    } else {
        bash_state.cwd.join(&expanded_path)
    };

    let path = validate_path_in_workspace(&path, &bash_state.workspace_root)
        .map_err(|e| WinxError::PathSecurityError { path: path.clone(), message: e.to_string() })?;

    let file_path_str = path.to_string_lossy().to_string();

    // Whitelist check (WCGW style)
    if path.exists() && !bash_state.whitelist_for_overwrite.contains_key(&file_path_str) {
        return Err(WinxError::FileAccessError {
            path: path.clone(),
            message: "Read file first before editing.".to_string(),
        });
    }

    let result = if file_write_or_edit.percentage_to_change <= 50 {
        let original_content = fs::read_to_string(&path)?;
        let blocks = parse_blocks(&file_write_or_edit.text_or_search_replace_blocks)?;
        let new_content = apply_blocks(&original_content, blocks)?;

        fs::write(&path, &new_content)?;
        format!("Successfully edited {file_path_str}")
    } else {
        fs::write(&path, &file_write_or_edit.text_or_search_replace_blocks)?;
        format!("Successfully wrote {file_path_str}")
    };

    // Update whitelist
    let final_content = fs::read_to_string(&path)?;
    let hash = format!("{:x}", Sha256::digest(final_content.as_bytes()));
    let total_lines = final_content.lines().count();

    bash_state
        .whitelist_for_overwrite
        .insert(file_path_str, FileWhitelistData::new(hash, vec![(1, total_lines)], total_lines));

    Ok(result)
}
