//! Implementation of the ReadFiles tool.
//!
//! This module provides the implementation for the ReadFiles tool, which is used
//! to read and display the contents of files, optionally with line numbers and
//! line range filtering.

use anyhow::Context as AnyhowContext;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info, instrument, warn};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::ReadFiles;
use crate::utils::path::expand_user;

/// Type alias for file reading result
///
/// Contains:
/// - The file content as a string
/// - Whether the content was truncated due to token limit
/// - The token count of the content
/// - The canonicalized file path
/// - The effective line range that was read
type FileReadResult = (String, bool, usize, String, (usize, usize));

/// Maximum amount of data to read from a file to prevent memory issues
const MAX_FILE_SIZE: u64 = 10_000_000; // 10MB

/// Format a line range specification for display
///
/// This formats a start and end line number into a string like ":1-10"
/// for display in file paths.
///
/// # Arguments
///
/// * `start_line_num` - Optional start line number
/// * `end_line_num` - Optional end line number
///
/// # Returns
///
/// A formatted string representing the line range
fn range_format(start_line_num: Option<usize>, end_line_num: Option<usize>) -> String {
    let st = start_line_num.map_or(String::new(), |n| n.to_string());
    let end = end_line_num.map_or(String::new(), |n| n.to_string());

    if st.is_empty() && end.is_empty() {
        String::new()
    } else {
        format!(":{}-{}", st, end)
    }
}

/// Read a single file with optional line range filtering
///
/// This function reads a file and returns its contents, with support for
/// showing line numbers and filtering by line range.
///
/// # Arguments
///
/// * `file_path` - Path to the file to read
/// * `max_tokens` - Optional maximum number of tokens to include
/// * `show_line_numbers` - Whether to include line numbers in the output
/// * `start_line_num` - Optional start line number for filtering (1-indexed)
/// * `end_line_num` - Optional end line number for filtering (1-indexed, inclusive)
///
/// # Returns
///
/// A tuple containing:
/// - The file content as a string
/// - Whether the content was truncated due to token limit
/// - The token count of the content
/// - The canonicalized file path
/// - The effective line range that was read
///
/// # Errors
///
/// Returns an error if the file cannot be accessed or read
#[instrument(level = "debug", skip(file_path))]
fn read_file(
    file_path: &str,
    max_tokens: Option<usize>,
    cwd: &Path,
    show_line_numbers: bool,
    start_line_num: Option<usize>,
    end_line_num: Option<usize>,
) -> Result<FileReadResult> {
    debug!("Reading file: {}", file_path);

    // Expand the path
    let file_path = expand_user(file_path);

    // Ensure path is absolute
    let path = if Path::new(&file_path).is_absolute() {
        PathBuf::from(&file_path)
    } else {
        // Use current working directory if path is relative
        cwd.join(&file_path)
    };

    // Check if path exists
    if !path.exists() {
        return Err(WinxError::FileAccessError {
            path: path.clone(),
            message: "File does not exist".to_string(),
        });
    }

    // Ensure it's a file
    if !path.is_file() {
        return Err(WinxError::FileAccessError {
            path: path.clone(),
            message: "Path exists but is not a file".to_string(),
        });
    }

    // Get file metadata
    let metadata = fs::metadata(&path).context("Failed to get file metadata")?;

    // Check file size
    if metadata.len() > MAX_FILE_SIZE {
        warn!("File size exceeds limit: {} bytes", metadata.len());
        return Err(WinxError::FileAccessError {
            path: path.clone(),
            message: format!(
                "File is too large: {} bytes (max {})",
                metadata.len(),
                MAX_FILE_SIZE
            ),
        });
    }

    // Read file content
    let content = fs::read_to_string(&path).map_err(|e| WinxError::FileAccessError {
        path: path.clone(),
        message: format!("Error reading file: {}", e),
    })?;

    // Split into lines, ensuring we capture an empty line at the end if needed
    let mut all_lines: Vec<&str> = content.lines().collect();
    if content.ends_with('\n') {
        all_lines.push("");
    }

    let total_lines = all_lines.len();

    // Apply line range filtering
    let start_idx = start_line_num.map_or(0, |n| n.saturating_sub(1).min(total_lines));
    let end_idx = end_line_num.map_or(total_lines, |n| n.min(total_lines));

    // Effective line numbers for tracking (1-indexed)
    let effective_start = start_line_num.unwrap_or(1);
    let mut effective_end = end_line_num.unwrap_or(total_lines);

    // Extract the requested lines
    let filtered_lines = &all_lines[start_idx..end_idx];

    // Create content string with or without line numbers
    let mut result_content = String::new();
    if show_line_numbers {
        for (i, line) in filtered_lines.iter().enumerate() {
            let line_num = start_idx + i + 1; // Convert to 1-indexed
            result_content.push_str(&format!("{} {}\n", line_num, line));
        }
    } else {
        for line in filtered_lines {
            result_content.push_str(line);
            result_content.push('\n');
        }
    }

    // Default return values
    let mut truncated = false;
    let mut tokens_count = 0;

    // Handle token limiting if specified
    if let Some(max_tokens) = max_tokens {
        // NOTE: We're not actually counting tokens here since we don't have the encoder
        // Just using character count as a rough approximation
        tokens_count = result_content.len();

        if tokens_count > max_tokens {
            // Simple truncation at character boundary
            // In a real implementation, truncate at token boundary using encoder
            result_content = result_content.chars().take(max_tokens).collect();

            // Count how many lines we kept after truncation
            let line_count = result_content.matches('\n').count();

            // Calculate the last line number shown (1-indexed)
            let last_line_shown = start_idx + line_count;

            // Add informative message about truncation
            result_content.push_str(&format!(
                "\n(...truncated) Only showing till line number {} of {} total lines due to the token limit, please continue reading from {} if required",
                last_line_shown, total_lines, last_line_shown + 1
            ));

            truncated = true;
            effective_end = last_line_shown;
        }
    }

    // Get canonicalized path string
    let canon_path = path
        .canonicalize()
        .unwrap_or(path.clone())
        .to_string_lossy()
        .to_string();

    Ok((
        result_content,
        truncated,
        tokens_count,
        canon_path,
        (effective_start, effective_end),
    ))
}

/// Handle the ReadFiles tool call
///
/// This function processes the ReadFiles tool call, which reads the contents
/// of one or more files and returns them with optional line numbers and filtering.
///
/// # Arguments
///
/// * `bash_state_arc` - Shared reference to the bash state
/// * `read_files` - The read files parameters
///
/// # Returns
///
/// A Result containing the response message to send to the client
///
/// # Errors
///
/// Returns an error if any file cannot be accessed or read
#[instrument(level = "info", skip(bash_state_arc, read_files))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    read_files: ReadFiles,
) -> Result<String> {
    info!("ReadFiles tool called with: {:?}", read_files);

    // We need to extract data from the bash state before awaiting
    // to avoid holding the MutexGuard across await points
    let cwd: PathBuf;

    // Lock bash state to extract data
    {
        let bash_state_guard = bash_state_arc.lock().map_err(|e| {
            WinxError::BashStateLockError(format!("Failed to lock bash state: {}", e))
        })?;

        // Ensure bash state is initialized
        let bash_state = match &*bash_state_guard {
            Some(state) => state,
            None => {
                error!("BashState not initialized");
                return Err(WinxError::BashStateNotInitialized);
            }
        };

        // Extract needed data
        cwd = bash_state.cwd.clone();
    }

    let mut message = String::new();
    let mut file_ranges_dict: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    let mut remaining_tokens = read_files.max_tokens;

    // Process each file path, parsing line ranges if needed
    for (i, file_path) in read_files.file_paths.iter().enumerate() {
        // Parse path for line ranges - this applies the same logic as ReadFiles.parse_line_ranges()
        let mut start_line_num = read_files.start_line_nums.get(i).copied().unwrap_or(None);
        let mut end_line_num = read_files.end_line_nums.get(i).copied().unwrap_or(None);
        let mut clean_path = file_path.clone();

        // Check if the path contains a line range specification
        if file_path.contains(':') {
            let parts: Vec<&str> = file_path.rsplitn(2, ':').collect();
            if parts.len() == 2 {
                let potential_path = parts[1];
                let line_spec = parts[0];

                // Check if it's a valid line range format
                if let Ok(line_num) = line_spec.parse::<usize>() {
                    // Format: file.py:10
                    start_line_num = Some(line_num);
                    end_line_num = Some(line_num);
                    clean_path = potential_path.to_string();
                } else if line_spec.contains('-') {
                    // Could be file.py:10-20, file.py:10-, or file.py:-20
                    let line_parts: Vec<&str> = line_spec.split('-').collect();

                    if line_parts[0].is_empty() && !line_parts[1].is_empty() {
                        // Format: file.py:-20
                        if let Ok(end) = line_parts[1].parse::<usize>() {
                            end_line_num = Some(end);
                            clean_path = potential_path.to_string();
                        }
                    } else if !line_parts[0].is_empty() {
                        // Format: file.py:10-20 or file.py:10-
                        if let Ok(start) = line_parts[0].parse::<usize>() {
                            start_line_num = Some(start);

                            if !line_parts[1].is_empty() {
                                // file.py:10-20
                                if let Ok(end) = line_parts[1].parse::<usize>() {
                                    end_line_num = Some(end);
                                }
                            }
                            clean_path = potential_path.to_string();
                        }
                    }
                }
            }
        }

        // Try to read the file
        match read_file(
            &clean_path,
            remaining_tokens,
            &cwd,
            read_files.show_line_numbers(),
            start_line_num,
            end_line_num,
        ) {
            Ok((content, file_truncated, tokens_used, canon_path, line_range)) => {
                // Update tokens used (if limiting)
                if let Some(max_tokens) = remaining_tokens {
                    if tokens_used >= max_tokens {
                        remaining_tokens = Some(0);
                    } else {
                        remaining_tokens = Some(max_tokens - tokens_used);
                    }
                }

                // Add to file ranges dictionary
                if let Some(ranges) = file_ranges_dict.get_mut(&canon_path) {
                    ranges.push(line_range);
                } else {
                    file_ranges_dict.insert(canon_path.clone(), vec![line_range]);
                }

                // Add content to message
                let range_formatted = range_format(start_line_num, end_line_num);
                message.push_str(&format!(
                    "\n{}{}\n```\n{}\n",
                    file_path, range_formatted, content
                ));

                // Check if we need to stop due to truncation or token limit
                if file_truncated || remaining_tokens == Some(0) {
                    // Mention files we're not reading if any remain
                    let remaining_files: Vec<String> =
                        read_files.file_paths.iter().skip(i + 1).cloned().collect();
                    if !remaining_files.is_empty() {
                        message.push_str(&format!(
                            "\nNot reading the rest of the files: {} due to token limit, please call again",
                            remaining_files.join(", ")
                        ));
                    }
                    break;
                } else {
                    message.push_str("```");
                }
            }
            Err(e) => {
                // Log the error but continue with other files
                error!("Error reading file {}: {}", file_path, e);
                message.push_str(&format!("\n{}: {}\n", file_path, e));
            }
        }
    }

    // Track file accesses in whitelist for later editing
    // Note: In a real implementation, this would update the BashState whitelist_for_overwrite
    // but we need to refactor that to either:
    // 1. Return the updated hashmap from this function, or
    // 2. Create an async update method that can be called without holding the lock

    // Build the whitelist data for file paths
    let whitelist_data: HashMap<String, Vec<(usize, usize)>> = file_ranges_dict.clone();

    // Add a transaction to update the bash state with the whitelist data
    // This would typically need to run without blocking the async function
    tokio::task::spawn_blocking({
        let bash_state_arc = Arc::clone(bash_state_arc);
        let whitelist_data = whitelist_data.clone();
        move || {
            if let Ok(mut bash_state_guard) = bash_state_arc.lock() {
                if let Some(bash_state) = bash_state_guard.as_mut() {
                    for (file_path, ranges) in whitelist_data {
                        // Read file content to calculate hash
                        if let Ok(file_content) = fs::read(&file_path) {
                            // Calculate file hash
                            let mut hasher = Sha256::new();
                            hasher.update(&file_content);
                            let file_hash = format!("{:x}", hasher.finalize());

                            // Calculate total lines in file
                            let total_lines =
                                file_content.iter().filter(|&&c| c == b'\n').count() + 1;

                            // Add or update the whitelist entry
                            if let Some(existing) =
                                bash_state.whitelist_for_overwrite.get_mut(&file_path)
                            {
                                existing.file_hash = file_hash;
                                existing.total_lines = total_lines;
                                for range in ranges {
                                    existing.add_range(range.0, range.1);
                                }
                            } else {
                                bash_state.whitelist_for_overwrite.insert(
                                    file_path,
                                    crate::state::bash_state::FileWhitelistData::new(
                                        file_hash,
                                        ranges,
                                        total_lines,
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        }
    });

    Ok(message)
}
