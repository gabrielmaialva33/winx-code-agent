//! Implementation of the `ReadFiles` tool.
//!
//! This module provides the implementation for the `ReadFiles` tool, which is used
//! to read and display the contents of files, optionally with line numbers and
//! line range filtering.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use tracing::{debug, error, info, instrument, warn};

use crate::errors::{ErrorRecovery, Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::ReadFiles;
use crate::utils::mmap::read_file_to_string;
use crate::utils::path::{expand_user, validate_path_in_workspace};

/// Default token limits for file reading
const CODING_MAX_TOKENS: usize = 24_000;
const NONCODING_MAX_TOKENS: usize = 8_000;

/// Type alias for file reading result
type FileReadResult = (String, bool, usize, String, (usize, usize), String, usize);
type ReadCoverage = (Vec<(usize, usize)>, String, usize);

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
    let file_hash = hash_content(&content);
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    let start_idx = start_line_num.map_or(0, |n| n.saturating_sub(1).min(lines.len()));
    let end_idx = end_line_num.map_or(lines.len(), |n| n.min(lines.len()));

    if start_idx > lines.len() || start_idx > end_idx {
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
            let _ = writeln!(result_content, "{line_num} {line}");
        }
    } else {
        for line in filtered_lines {
            result_content.push_str(line);
            result_content.push('\n');
        }
    }

    let mut truncated = false;
    let max_tokens = max_tokens.unwrap_or_else(|| select_max_tokens(file_path));
    // Tokenize once; reuse the ids for both the count and the truncation below,
    // instead of encoding the (possibly large) content a second time on truncate.
    let token_ids = crate::utils::encoder::encode_ids(&result_content);
    let tokens_count = token_ids
        .as_ref()
        .map_or_else(|| crate::utils::encoder::estimate_tokens(&result_content), Vec::len);

    if tokens_count > max_tokens {
        truncate_to_token_budget(&mut result_content, max_tokens, token_ids);
        // Tell the agent exactly where to resume so the tail isn't silently lost.
        let kept_lines = result_content.lines().count();
        let last_shown = (start_idx + kept_lines).min(total_lines);
        let resume_from = last_shown + 1;
        let _ = write!(
            result_content,
            "\n(...truncated) Showing up to line {last_shown} of {total_lines} total lines \
             ({tokens_count} tokens exceeded limit {max_tokens}). Continue reading from line \
             {resume_from} using the syntax {file_path}:{resume_from}-{total_lines}"
        );
        truncated = true;
    }

    let canon_path = path.to_string_lossy().to_string();

    Ok((
        result_content,
        truncated,
        tokens_count,
        canon_path,
        (effective_start, effective_end.min(total_lines.max(1))),
        file_hash,
        total_lines,
    ))
}

fn hash_content(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    digest.iter().fold(String::with_capacity(digest.len() * 2), |mut hash, byte| {
        let _ = write!(hash, "{byte:02x}");
        hash
    })
}

fn truncate_to_token_budget(content: &mut String, max_tokens: usize, ids: Option<Vec<u32>>) {
    // `ids` were already computed by the caller (the token count needs them too),
    // so reuse them instead of re-encoding the whole string a second time here.
    let Some(ids) = ids else {
        // No tokenizer available: fall back to a char-count cut.
        let byte_idx = byte_index_for_char_count(content, max_tokens);
        content.truncate(byte_idx);
        return;
    };

    if ids.len() <= max_tokens {
        return;
    }

    if let Some(decoded) = crate::utils::encoder::decode_ids(&ids[..max_tokens]) {
        *content = decoded;
    } else {
        let byte_idx = byte_index_for_char_count(content, max_tokens);
        content.truncate(byte_idx);
    }
}

fn byte_index_for_char_count(content: &str, char_count: usize) -> usize {
    content.char_indices().nth(char_count).map_or(content.len(), |(idx, _)| idx)
}

fn select_max_tokens(file_path: &str) -> usize {
    // Budgets are overridable per deployment via env vars so large-context
    // clients can pull more of each file into context (defaults match wcgw).
    if is_source_code_file(file_path) {
        crate::utils::encoder::budget_from_env("WINX_CODING_TOKEN_BUDGET", CODING_MAX_TOKENS)
    } else {
        crate::utils::encoder::budget_from_env("WINX_NONCODING_TOKEN_BUDGET", NONCODING_MAX_TOKENS)
    }
}

fn is_source_code_file(file_path: &str) -> bool {
    let path = Path::new(file_path);
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default();

    matches!(file_name, "Makefile" | "Dockerfile" | "Jenkinsfile")
        || matches!(
            extension,
            "py" | "pyx"
                | "pyi"
                | "pyw"
                | "js"
                | "jsx"
                | "ts"
                | "tsx"
                | "mjs"
                | "cjs"
                | "html"
                | "css"
                | "scss"
                | "sass"
                | "less"
                | "c"
                | "h"
                | "cpp"
                | "cxx"
                | "cc"
                | "hpp"
                | "java"
                | "kt"
                | "go"
                | "rs"
                | "rb"
                | "php"
                | "sh"
                | "bash"
                | "zsh"
                | "sql"
                | "xml"
                | "json"
                | "yaml"
                | "yml"
                | "toml"
                | "md"
                | "ex"
                | "exs"
        )
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
    let mut file_ranges_dict: HashMap<String, ReadCoverage> = HashMap::new();

    for (index, file_path) in read_files.file_paths.iter().enumerate() {
        let clean_path = read_files.get_clean_path(index);
        let start_line_num = read_files.start_line_nums.get(index).copied().flatten();
        let end_line_num = read_files.end_line_nums.get(index).copied().flatten();

        match read_file(
            &clean_path,
            Some(select_max_tokens(&clean_path)),
            &cwd,
            &workspace_root,
            read_files.show_line_numbers(),
            start_line_num,
            end_line_num,
        )
        .await
        {
            Ok((content, truncated, _, canon_path, line_range, file_hash, total_lines)) => {
                let entry = file_ranges_dict
                    .entry(canon_path.clone())
                    .or_insert_with(|| (Vec::new(), file_hash.clone(), total_lines));
                entry.0.push(line_range);
                entry.1 = file_hash;
                entry.2 = total_lines;
                let _ = write!(
                    message,
                    "\n{}{}\n```\n{content}\n```",
                    clean_path,
                    range_format(start_line_num, end_line_num)
                );

                if let Err(e) = crate::utils::workspace_stats::record_read(
                    &workspace_root,
                    Path::new(&canon_path),
                ) {
                    debug!("failed to record read stats: {e}");
                }

                if truncated {
                    let remaining = read_files.file_paths.len().saturating_sub(index + 1);
                    if remaining > 0 {
                        let _ = write!(
                            message,
                            "\n\n(Not reading the remaining {remaining} file(s) due to the token \
                             limit. Call ReadFiles again for them.)"
                        );
                    }
                    break;
                }
            }
            Err(e) => {
                let _ = write!(message, "\nError reading {file_path}: {e}");
            }
        }
    }

    let mut bash_state_guard = bash_state_arc.lock().await;
    if let Some(bash_state) = bash_state_guard.as_mut() {
        for (path, (ranges, file_hash, total_lines)) in file_ranges_dict {
            bash_state
                .whitelist_for_overwrite
                .entry(path)
                .and_modify(|existing| {
                    existing.file_hash.clone_from(&file_hash);
                    existing.total_lines = total_lines;
                    existing.line_ranges_read.extend(ranges.iter().copied());
                })
                .or_insert_with(|| {
                    crate::state::bash_state::FileWhitelistData::new(file_hash, ranges, total_lines)
                });
        }
    }

    Ok(message)
}
