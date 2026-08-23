//! Implementation of the `ReadFiles` tool.
//!
//! This module provides the implementation for the `ReadFiles` tool, which is used
//! to read and display the contents of files, optionally with line numbers and
//! line range filtering.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, instrument};

use crate::errors::{ErrorRecovery, Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::ReadFiles;
use crate::utils::mmap::read_file_to_string;
use crate::utils::path::{expand_user, validate_path_in_workspace};

/// Default token limits for file reading
const CODING_MAX_TOKENS: usize = 24_000;
const NONCODING_MAX_TOKENS: usize = 8_000;
const DEFAULT_READ_PARALLELISM: usize = 4;
const MAX_READ_PARALLELISM: usize = 32;

/// Type alias for file reading result
type FileReadResult = (String, bool, usize, String, (usize, usize), String, usize);
type ReadCoverage = (Vec<(usize, usize)>, String, usize);

#[derive(Clone)]
struct FileReadRequest {
    index: usize,
    requested_path: String,
    clean_path: String,
    start_line_num: Option<usize>,
    end_line_num: Option<usize>,
}

/// Complete result of one batched read. The MCP adapter uses the per-file
/// errors to return an honest `isError: true` result while retaining any
/// successfully read content in the same response.
#[derive(Debug)]
pub struct ReadFilesOutcome {
    pub text: String,
    pub successful_files: usize,
    pub errors: Vec<WinxError>,
}

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
fn read_file(
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
    let mut last_shown = effective_end;
    let max_tokens = max_tokens.unwrap_or_else(|| select_max_tokens(file_path));
    // Byte-level BPE emits at most one token per input byte. Small payloads are
    // therefore proven to fit without initializing the tokenizer; larger ones
    // still get exact counting and token-boundary truncation.
    let (tokens_count, token_ids) =
        if crate::utils::encoder::definitely_fits_token_budget(&result_content, max_tokens) {
            (crate::utils::encoder::estimate_tokens(&result_content), None)
        } else {
            let ids = crate::utils::encoder::encode_ids(&result_content);
            let count = ids
                .as_ref()
                .map_or_else(|| crate::utils::encoder::estimate_tokens(&result_content), Vec::len);
            (count, ids)
        };

    if tokens_count > max_tokens {
        truncate_to_token_budget(&mut result_content, max_tokens, token_ids);
        // Tell the agent exactly where to resume so the tail isn't silently lost.
        let kept_lines = result_content.lines().count();
        last_shown = (start_idx + kept_lines).min(total_lines);
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
    let effective_end_line = if truncated { last_shown } else { effective_end };

    Ok((
        result_content,
        truncated,
        tokens_count,
        canon_path,
        (effective_start, effective_end_line.min(total_lines.max(1))),
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
        trim_to_last_line_boundary(content);
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
    trim_to_last_line_boundary(content);
}

fn trim_to_last_line_boundary(content: &mut String) {
    if let Some(last_nl) = content.rfind('\n') {
        content.truncate(last_nl + 1);
    } else {
        // A partial first line is not safe to record as read: the continuation
        // would skip its unseen suffix. Keep no content/range and resume from
        // the same line instead.
        content.clear();
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

fn read_parallelism() -> usize {
    parse_read_parallelism(crate::config::env_text("WINX_READ_PARALLELISM").as_deref())
}

fn parse_read_parallelism(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|parallelism| *parallelism > 0)
        .unwrap_or(DEFAULT_READ_PARALLELISM)
        .min(MAX_READ_PARALLELISM)
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
    let outcome = handle_tool_call_detailed(bash_state_arc, read_files).await?;
    if outcome.successful_files == 0 {
        if let Some(error) = outcome.errors.into_iter().next() {
            return Err(error);
        }
    }
    Ok(outcome.text)
}

pub async fn handle_tool_call_detailed(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    read_files: ReadFiles,
) -> Result<ReadFilesOutcome> {
    let (cwd, workspace_root) = {
        let bash_state_guard = bash_state_arc.lock().await;
        let bash_state = bash_state_guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        (bash_state.cwd.clone(), bash_state.workspace_root.clone())
    };

    let mut message = String::new();
    let mut file_ranges_dict: HashMap<String, ReadCoverage> = HashMap::new();
    let mut stats_paths = Vec::new();
    let mut successful_files = 0usize;
    let mut errors = Vec::new();

    let requests = read_files
        .file_paths
        .iter()
        .enumerate()
        .map(|(index, file_path)| FileReadRequest {
            index,
            requested_path: file_path.clone(),
            clean_path: read_files.get_clean_path(index),
            start_line_num: read_files.start_line_nums.get(index).copied().flatten(),
            end_line_num: read_files.end_line_nums.get(index).copied().flatten(),
        })
        .collect::<Vec<_>>();
    let show_line_numbers = read_files.show_line_numbers();

    // Files are independent, so perform blocking filesystem/tokenizer work in a
    // bounded pool. Results are still consumed in request order, preserving the
    // stable response and the rule that a truncated file stops the visible batch.
    'batches: for batch in requests.chunks(read_parallelism()) {
        let mut tasks = Vec::with_capacity(batch.len());
        for request in batch.iter().cloned() {
            let worker_path = request.clean_path.clone();
            let worker_cwd = cwd.clone();
            let worker_root = workspace_root.clone();
            let start_line_num = request.start_line_num;
            let end_line_num = request.end_line_num;
            let task = tokio::task::spawn_blocking(move || {
                read_file(
                    &worker_path,
                    Some(select_max_tokens(&worker_path)),
                    &worker_cwd,
                    &worker_root,
                    show_line_numbers,
                    start_line_num,
                    end_line_num,
                )
            });
            tasks.push((request, task));
        }

        let mut tasks = tasks.into_iter();
        while let Some((request, task)) = tasks.next() {
            let result = task.await.unwrap_or_else(|error| {
                Err(WinxError::CommandExecutionError(format!(
                    "ReadFiles worker failed for {}: {error}",
                    request.clean_path
                )))
            });
            match result {
                Ok((content, truncated, _, canon_path, line_range, file_hash, total_lines)) => {
                    successful_files = successful_files.saturating_add(1);
                    let entry = file_ranges_dict
                        .entry(canon_path.clone())
                        .or_insert_with(|| (Vec::new(), file_hash.clone(), total_lines));
                    if entry.1 != file_hash || entry.2 != total_lines {
                        // The same path can be requested more than once in one
                        // batch. If it changed between reads, keep coverage only
                        // for the version whose hash will guard the next edit.
                        *entry = (Vec::new(), file_hash.clone(), total_lines);
                    }
                    entry.0.push(line_range);
                    let _ = write!(
                        message,
                        "\n{}{}\n```\n{content}\n```",
                        request.clean_path,
                        range_format(request.start_line_num, request.end_line_num)
                    );
                    stats_paths.push(PathBuf::from(&canon_path));

                    if truncated {
                        let remaining =
                            read_files.file_paths.len().saturating_sub(request.index + 1);
                        if remaining > 0 {
                            let _ = write!(
                                message,
                                "\n\n(Not reading the remaining {remaining} file(s) due to the \
                                 token limit. Call ReadFiles again for them.)"
                            );
                        }
                        // `spawn_blocking` jobs already running cannot be cancelled,
                        // but abort prevents queued work from starting. No result is
                        // recorded or whitelisted unless it was returned to the caller.
                        for (_, pending) in tasks {
                            pending.abort();
                        }
                        break 'batches;
                    }
                }
                Err(error) => {
                    let _ = write!(message, "\nError reading {}: {error}", request.requested_path);
                    errors.push(error);
                }
            }
        }
    }

    if !stats_paths.is_empty() {
        let stats_root = workspace_root.clone();
        let stats_result = tokio::task::spawn_blocking(move || {
            crate::utils::workspace_stats::record_reads(&stats_root, &stats_paths)
        })
        .await;
        match stats_result {
            Ok(Err(error)) => debug!("failed to record read stats: {error}"),
            Err(error) => debug!("read stats worker failed: {error}"),
            Ok(Ok(())) => {}
        }
    }

    let mut bash_state_guard = bash_state_arc.lock().await;
    if let Some(bash_state) = bash_state_guard.as_mut() {
        for (path, (ranges, file_hash, total_lines)) in file_ranges_dict {
            bash_state.record_read_coverage(&path, ranges, file_hash, total_lines);
        }
    }

    Ok(ReadFilesOutcome { text: message, successful_files, errors })
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::{parse_read_parallelism, read_file, trim_to_last_line_boundary};
    use crate::state::bash_state::FileWhitelistData;

    #[test]
    fn token_cut_never_exposes_a_partial_line() {
        let mut content = "complete line\npartial line suffix".to_string();
        content.truncate("complete line\npartial".len());
        trim_to_last_line_boundary(&mut content);
        assert_eq!(content, "complete line\n");

        let mut first_line_only = "partial first line".to_string();
        first_line_only.truncate(7);
        trim_to_last_line_boundary(&mut first_line_only);
        assert!(first_line_only.is_empty());
    }

    #[test]
    fn token_truncation_whitelists_only_complete_visible_lines() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("large.txt");
        let mut source = String::new();
        for line in 1..=200 {
            writeln!(source, "line {line}: enough repeated content to consume tokens quickly")?;
        }
        std::fs::write(&path, source)?;
        let path = path.to_str().ok_or_else(|| anyhow::anyhow!("fixture path is not UTF-8"))?;

        let (content, truncated, _, _, range, hash, total_lines) =
            read_file(path, Some(50), temp.path(), temp.path(), false, None, None)?;

        assert!(truncated);
        let visible = content
            .split_once("\n(...truncated)")
            .map(|(visible, _)| visible)
            .ok_or_else(|| anyhow::anyhow!("truncation marker is missing"))?;
        let visible_lines = visible.lines().count();
        assert_eq!(range, (1, visible_lines));
        let coverage = FileWhitelistData::new(hash, vec![range], total_lines);
        assert!(!coverage.is_read_enough());
        assert_eq!(coverage.get_unread_ranges(), vec![(visible_lines + 1, total_lines)]);
        Ok(())
    }

    #[test]
    fn oversized_first_line_records_no_read_coverage() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("single-line.txt");
        std::fs::write(&path, "x".repeat(10_000))?;
        let path = path.to_str().ok_or_else(|| anyhow::anyhow!("fixture path is not UTF-8"))?;

        let (content, truncated, _, _, range, hash, total_lines) =
            read_file(path, Some(10), temp.path(), temp.path(), false, None, None)?;

        assert!(truncated);
        assert!(content.starts_with("\n(...truncated) Showing up to line 0"));
        assert_eq!(range, (1, 0));
        let coverage = FileWhitelistData::new(hash, vec![range], total_lines);
        assert!(coverage.line_ranges_read.is_empty());
        assert_eq!(coverage.get_unread_ranges(), vec![(1, 1)]);
        Ok(())
    }

    #[test]
    fn parallelism_uses_safe_defaults_and_bounds() {
        assert_eq!(parse_read_parallelism(None), 4);
        assert_eq!(parse_read_parallelism(Some("invalid")), 4);
        assert_eq!(parse_read_parallelism(Some("0")), 4);
        assert_eq!(parse_read_parallelism(Some("1")), 1);
        assert_eq!(parse_read_parallelism(Some("8")), 8);
        assert_eq!(parse_read_parallelism(Some("999")), 32);
    }
}
