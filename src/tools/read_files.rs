//! Implementation of the ReadFiles tool.
//!
//! This module provides the implementation for the ReadFiles tool, which is used
//! to read and display the contents of files, optionally with line numbers and
//! line range filtering.

#[allow(unused_imports)]
use anyhow::Context as AnyhowContext;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info, instrument, warn};

use crate::errors::{ErrorRecovery, Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::ReadFiles;
use crate::utils::file_cache::FileCache;
use crate::utils::file_extensions::FileExtensionAnalyzer;
use crate::utils::mmap::{read_file_optimized, read_file_to_string};
use crate::utils::path::expand_user;
use crate::utils::resource_allocator::{
    get_global_allocator, request_file_allocation, AllocationGuard,
};

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
const MAX_FILE_SIZE: u64 = 50_000_000; // Increased to 50MB

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

/// Read a large file using streaming with chunk-based processing
///
/// This function reads large files in chunks to reduce memory usage.
/// It's designed for files that exceed normal memory allocation limits.
///
/// # Arguments
///
/// * `path` - Path to the file to read
/// * `chunk_size` - Size of each chunk to read
///
/// # Returns
///
/// The complete file content as a string
///
/// # Errors
///
/// Returns an error if the file cannot be read or if streaming fails
async fn read_file_with_streaming(path: &Path, chunk_size: usize) -> Result<String> {
    use std::fs::File;
    use std::io::{BufReader, Read};

    debug!(
        "Streaming file {} with chunk size {}",
        path.display(),
        chunk_size
    );

    let file = File::open(path).map_err(|e| WinxError::FileAccessError {
        path: path.to_path_buf(),
        message: format!("Failed to open file for streaming: {}", e),
    })?;

    let mut reader = BufReader::new(file);
    let mut content = String::new();
    let mut buffer = vec![0; chunk_size];

    loop {
        let bytes_read = reader
            .read(&mut buffer)
            .map_err(|e| WinxError::FileAccessError {
                path: path.to_path_buf(),
                message: format!("Failed to read chunk from file: {}", e),
            })?;

        if bytes_read == 0 {
            break; // End of file
        }

        // Convert chunk to string, handling potential UTF-8 issues
        match std::str::from_utf8(&buffer[..bytes_read]) {
            Ok(chunk_str) => content.push_str(chunk_str),
            Err(e) => {
                // Try to recover from UTF-8 errors by using lossy conversion
                warn!("UTF-8 error in chunk, using lossy conversion: {}", e);
                let chunk_str = String::from_utf8_lossy(&buffer[..bytes_read]);
                content.push_str(&chunk_str);
            }
        }

        // Yield to allow other tasks to run during large file processing
        tokio::task::yield_now().await;
    }

    info!(
        "Successfully streamed {} bytes from {}",
        content.len(),
        path.display()
    );
    Ok(content)
}

/// Read a single file with optional line range filtering
///
/// This function reads a file and returns its contents, with support for
/// showing line numbers and filtering by line range. Uses smart resource
/// allocation to optimize memory usage based on file size.
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
async fn read_file(
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
        let error = WinxError::FileAccessError {
            path: path.clone(),
            message: "File does not exist".to_string(),
        };

        // Provide helpful suggestions for common errors
        return Err(ErrorRecovery::suggest(
            error,
            &format!(
                "Check the file path. If it's a newly created file, make sure it has been saved. Current path: {}",
                path.display()
            ),
        ));
    }

    // Ensure it's a file
    if !path.is_file() {
        let error = WinxError::FileAccessError {
            path: path.clone(),
            message: "Path exists but is not a file".to_string(),
        };

        // If it's a directory, suggest using a specific file
        if path.is_dir() {
            return Err(ErrorRecovery::suggest(
                error,
                "The specified path is a directory. Please specify a file within this directory instead. Try using the Glob tool to list files in this directory.",
            ));
        }

        return Err(error);
    }

    // Get file metadata
    let metadata = match fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) => {
            let error = WinxError::FileAccessError {
                path: path.clone(),
                message: format!("Failed to get file metadata: {}", e),
            };

            return Err(ErrorRecovery::suggest(
                error,
                "Check file permissions and ensure the file is not locked by another process",
            ));
        }
    };

    // Smart resource allocation based on file size
    let file_size = metadata.len();
    let allocation = match request_file_allocation(&path, file_size).await {
        Ok(alloc) => alloc,
        Err(e) => {
            // If allocation fails due to resource constraints, provide alternatives
            return Err(ErrorRecovery::suggest(
                e,
                &format!(
                    "System is under memory pressure. Try reading smaller sections using line ranges (e.g., {}:1-1000) or wait for other operations to complete",
                    file_path
                ),
            ));
        }
    };

    // Create allocation guard for automatic cleanup
    let _guard = AllocationGuard::new(path.clone(), allocation.clone()).await;

    // Check file size against allocation
    if file_size > allocation.allocated_memory as u64 && !allocation.should_use_streaming {
        warn!("File size exceeds allocated memory: {} bytes", file_size);
        let error = WinxError::FileTooLarge {
            path: path.clone(),
            size: file_size,
            max_size: allocation.allocated_memory as u64,
        };

        return Err(ErrorRecovery::suggest(
            error,
            &format!(
                "File is too large for current memory allocation. Try reading parts of this file by specifying a line range (e.g., {}:1-1000)",
                file_path
            ),
        ));
    }

    // Read file content using smart allocation strategy
    let content = if allocation.should_use_streaming {
        // Use streaming for large files
        debug!("Using streaming read for large file: {} bytes", file_size);
        match read_file_with_streaming(&path, allocation.chunk_size.unwrap_or(1024 * 1024)).await {
            Ok(c) => c,
            Err(e) => {
                return Err(ErrorRecovery::suggest(
                    e,
                    "Try reading the file with smaller line ranges or check if it contains binary content",
                ));
            }
        }
    } else {
        // Use standard memory-mapped reading for smaller files
        debug!("Using standard read for file: {} bytes", file_size);
        match read_file_to_string(&path, allocation.allocated_memory as u64) {
            Ok(c) => c,
            Err(e) => {
                return Err(ErrorRecovery::suggest(
                    e,
                    "Try reading the file with smaller line ranges or check if it contains binary content",
                ));
            }
        }
    };

    // Use more efficient line handling with better memory characteristics
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len() + if content.ends_with('\n') { 1 } else { 0 };

    // Apply line range filtering with bounds checking
    let start_idx = start_line_num.map_or(0, |n| n.saturating_sub(1).min(lines.len()));
    let end_idx = end_line_num.map_or(lines.len(), |n| n.min(lines.len()));

    // Validate line range
    if start_idx >= lines.len() || start_idx >= end_idx {
        let error = ErrorRecovery::param_error(
            "line_range",
            &format!(
                "Invalid line range: start={:?}, end={:?}, file has {} lines",
                start_line_num,
                end_line_num,
                lines.len()
            ),
        );

        return Err(ErrorRecovery::suggest(
            error,
            &format!(
                "File has {} lines. Please specify a valid line range (e.g., {}:1-{})",
                lines.len(),
                file_path,
                lines.len().min(1000)
            ),
        ));
    }

    // Effective line numbers for tracking (1-indexed)
    let effective_start = start_line_num.unwrap_or(1);
    let mut effective_end = end_line_num.unwrap_or(total_lines);

    // Extract the requested lines - allocate with capacity for better performance
    let filtered_lines = &lines[start_idx..end_idx];

    // Pre-calculate the approximate size needed for the result
    let approx_size = filtered_lines
        .iter()
        .map(|line| line.len() + 1)
        .sum::<usize>();
    let mut result_content = String::with_capacity(approx_size);

    // Create content string with or without line numbers
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
        // Using character count as a rough approximation of tokens
        tokens_count = result_content.len();

        if tokens_count > max_tokens {
            // Use an efficient truncation strategy for large strings
            let mut char_count = 0;
            let mut truncation_point = 0;

            for (idx, _) in result_content.char_indices() {
                char_count += 1;
                if char_count > max_tokens {
                    truncation_point = idx;
                    break;
                }
            }

            if truncation_point > 0 {
                result_content.truncate(truncation_point);
            }

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

    // Get canonicalized path string - does this asynchronously to avoid blocking
    let path_clone = path.clone();
    let canon_path = match path.canonicalize() {
        Ok(canon) => canon.to_string_lossy().to_string(),
        Err(_) => path_clone.to_string_lossy().to_string(),
    };

    // Mark read operation as successful
    let allocator = get_global_allocator();
    allocator.mark_read_success().await;

    debug!(
        "Successfully read file: {} ({} bytes, {} lines)",
        canon_path,
        content.len(),
        total_lines
    );

    Ok((
        result_content,
        truncated,
        tokens_count,
        canon_path,
        (effective_start, effective_end),
    ))
}

/// Validate ReadFiles parameters and provide defaults for missing values
///
/// This function performs validation on the ReadFiles struct and returns
/// a Result containing either a validated ReadFiles struct or an appropriate error.
fn validate_read_files(read_files: ReadFiles) -> Result<ReadFiles> {
    // Check if file_paths is empty
    if read_files.file_paths.is_empty() {
        return Err(ErrorRecovery::suggest(
            ErrorRecovery::missing_param(
                "file_paths",
                "No file paths provided. Please specify at least one file to read.",
            ),
            "Example: { \"file_paths\": [\"/path/to/your/file.txt\"] }",
        ));
    }

    // Check if any file_path is null, undefined or an empty string
    for (i, path) in read_files.file_paths.iter().enumerate() {
        if path.trim().is_empty() {
            return Err(ErrorRecovery::suggest(
                ErrorRecovery::param_error(
                    &format!("file_paths[{}]", i),
                    "File path cannot be empty",
                ),
                "Please provide a valid file path for each element in the file_paths array",
            ));
        }
    }

    // Ensure start_line_nums and end_line_nums are properly sized if provided
    let mut validated = read_files.clone();
    if !validated.start_line_nums.is_empty()
        && validated.start_line_nums.len() < validated.file_paths.len()
    {
        // Extend start_line_nums with None values
        validated
            .start_line_nums
            .resize(validated.file_paths.len(), None);
    }

    if !validated.end_line_nums.is_empty()
        && validated.end_line_nums.len() < validated.file_paths.len()
    {
        // Extend end_line_nums with None values
        validated
            .end_line_nums
            .resize(validated.file_paths.len(), None);
    }

    // Initialize start_line_nums and end_line_nums if they're empty
    if validated.start_line_nums.is_empty() {
        validated.start_line_nums = vec![None; validated.file_paths.len()];
    }

    if validated.end_line_nums.is_empty() {
        validated.end_line_nums = vec![None; validated.file_paths.len()];
    }

    Ok(validated)
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

    // Validate parameters and provide default values for missing ones
    let validated_read_files = match validate_read_files(read_files) {
        Ok(validated) => validated,
        Err(e) => {
            error!("Failed to validate ReadFiles parameters: {}", e);
            return Err(e);
        }
    };

    // Extract data from the bash state before any async operations
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
                return Err(ErrorRecovery::suggest(
                    WinxError::BashStateNotInitialized,
                    "Please call Initialize first with type=\"first_call\" and a valid workspace path",
                ));
            }
        };

        // Extract needed data
        cwd = bash_state.cwd.clone();
    }

    // Process file paths and line ranges
    // Create a vector of file reading parameters for parallel processing
    let mut file_params = Vec::with_capacity(validated_read_files.file_paths.len());
    let show_line_numbers = validated_read_files.show_line_numbers();

    // Use intelligent token allocation based on file types
    let file_extension_analyzer = FileExtensionAnalyzer::new();
    let max_tokens_per_file = validated_read_files.max_tokens;

    // If we have a token limit, allocate intelligently across files
    let token_allocations = if let Some(total_tokens) = max_tokens_per_file {
        let file_names: Vec<String> = validated_read_files
            .file_paths
            .iter()
            .map(|path| {
                // Extract just the filename for analysis
                Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(path)
                    .to_string()
            })
            .collect();

        file_extension_analyzer
            .allocate_token_budget(&file_names, total_tokens)
            .into_iter()
            .collect::<HashMap<String, usize>>()
    } else {
        HashMap::new()
    };

    // Prepare file parameters
    for (i, file_path) in validated_read_files.file_paths.iter().enumerate() {
        // Parse path for line ranges
        let mut start_line_num = validated_read_files
            .start_line_nums
            .get(i)
            .copied()
            .unwrap_or(None);
        let mut end_line_num = validated_read_files
            .end_line_nums
            .get(i)
            .copied()
            .unwrap_or(None);
        let mut clean_path = file_path.clone();

        // Extract line range from path if present
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

        // Get allocated tokens for this file
        let file_name = Path::new(&clean_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&clean_path)
            .to_string();

        let allocated_tokens = token_allocations
            .get(&file_name)
            .copied()
            .or(max_tokens_per_file); // Fallback to global limit if no allocation

        // Store file params for parallel processing
        file_params.push((
            clean_path,
            file_path.clone(),
            start_line_num,
            end_line_num,
            allocated_tokens,
        ));
    }

    // Build a structure to hold results
    struct FileReadInfo {
        original_path: String,
        result: Result<FileReadResult>,
    }

    // Process files in parallel using tokio tasks for I/O bound operations
    let mut file_read_tasks = Vec::with_capacity(file_params.len());

    // Clone all file parameters outside the closure to avoid lifetime issues
    let cloned_params = file_params;

    // Limit parallel file reading to avoid overwhelming the system
    // Typically processors have 8-32 cores, so 8 is a reasonable default
    const MAX_PARALLEL_READS: usize = 8;
    let chunk_size = (cloned_params.len() + MAX_PARALLEL_READS - 1) / MAX_PARALLEL_READS.max(1);

    // Process files in chunks
    for chunk in cloned_params.chunks(chunk_size.max(1)) {
        let chunk_tasks = chunk
            .iter()
            .map(
                |(clean_path, original_path, start_line_num, end_line_num, allocated_tokens)| {
                    let clean_path = clean_path.clone();
                    let original_path = original_path.clone();
                    let cwd = cwd.clone();
                    let start = *start_line_num;
                    let end = *end_line_num;
                    let file_tokens = *allocated_tokens;

                    tokio::spawn(async move {
                        let result = read_file(
                            &clean_path,
                            file_tokens,
                            &cwd,
                            show_line_numbers,
                            start,
                            end,
                        )
                        .await;

                        // Track errors if read failed
                        if result.is_err() {
                            let allocator = get_global_allocator();
                            allocator.mark_read_failure().await;
                        }

                        FileReadInfo {
                            original_path,
                            result,
                        }
                    })
                },
            )
            .collect::<Vec<_>>();

        // Wait for this chunk to complete
        for task in chunk_tasks {
            file_read_tasks.push(task.await.unwrap_or_else(|e| FileReadInfo {
                original_path: "unknown".to_string(),
                result: Err(WinxError::CommandExecutionError(format!(
                    "Task panicked: {}",
                    e
                ))),
            }));
        }
    }

    // Process results
    let mut message = String::new();
    let mut file_ranges_dict: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    let mut remaining_tokens = max_tokens_per_file;
    let mut should_stop = false;
    let mut had_errors = false;

    for (i, file_info) in file_read_tasks.into_iter().enumerate() {
        if should_stop {
            continue;
        }

        let file_path = &file_info.original_path;

        match file_info.result {
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
                let start_line_num = if i < cloned_params.len() {
                    cloned_params[i].2
                } else {
                    None
                };
                let end_line_num = if i < cloned_params.len() {
                    cloned_params[i].3
                } else {
                    None
                };
                let range_formatted = range_format(start_line_num, end_line_num);
                message.push_str(&format!(
                    "\n{}{}\n```\n{}\n",
                    file_path, range_formatted, content
                ));

                // Check if we need to stop due to truncation or token limit
                if file_truncated || remaining_tokens == Some(0) {
                    should_stop = true;

                    // Mention files we're not reading if any remain
                    let remaining_files: Vec<String> = validated_read_files
                        .file_paths
                        .iter()
                        .skip(i + 1)
                        .cloned()
                        .collect();
                    if !remaining_files.is_empty() {
                        message.push_str(&format!(
                            "\nNot reading the rest of the files: {} due to token limit, please call again",
                            remaining_files.join(", ")
                        ));
                    }
                } else {
                    message.push_str("```");
                }
            }
            Err(e) => {
                // Log the error but continue with other files
                error!("Error reading file {}: {}", file_path, e);

                // Format error messages more helpfully
                let error_msg = match &e {
                    WinxError::FileAccessError { path, message } => {
                        format!("Error accessing file: {} - {}", path.display(), message)
                    }
                    WinxError::RecoverableSuggestionError {
                        message,
                        suggestion,
                    } => {
                        format!("{} - Suggestion: {}", message, suggestion)
                    }
                    _ => format!("{}", e),
                };

                message.push_str(&format!("\n{}: {}\n", file_path, error_msg));
                had_errors = true;
            }
        }
    }

    // Add warning/error information if there were errors
    if had_errors && message.is_empty() {
        return Err(ErrorRecovery::suggest(
            ErrorRecovery::param_error(
                "file_paths",
                "All file read operations failed. Please check the file paths and permissions.",
            ),
            "Ensure that the file paths are correct and the files exist",
        ));
    } else if had_errors {
        message.push_str(
            "\n\nNOTE: Some files could not be read. Check the error messages above for details.",
        );
    }

    // If no content was read at all
    if message.is_empty() {
        message = "No files were read or all files were empty.".to_string();
    }

    // Use the file cache to record read ranges
    let cache = FileCache::global();

    // For each file that was read, record the ranges in the cache
    for (file_path, ranges) in &file_ranges_dict {
        for &(start, end) in ranges {
            if let Err(e) = cache.record_read_range(Path::new(file_path), start, end) {
                warn!("Failed to record read range for {}: {}", file_path, e);
            }
        }
    }

    // Update whitelist data in bash state
    tokio::task::spawn_blocking({
        let bash_state_arc = Arc::clone(bash_state_arc);
        let whitelist_data: HashMap<String, Vec<(usize, usize)>> = file_ranges_dict.clone();

        move || {
            if let Ok(mut bash_state_guard) = bash_state_arc.lock()
                && let Some(bash_state) = bash_state_guard.as_mut() {
                    for (file_path, ranges) in whitelist_data {
                        // The cache already has the file hash and metadata,
                        // so we just need to ensure it's in the whitelist

                        // Get the hash from the cache
                        let file_hash = cache
                            .get_cached_hash(Path::new(&file_path))
                            .unwrap_or_else(|| {
                                // If not in cache (shouldn't happen), calculate it
                                match read_file_optimized(Path::new(&file_path), MAX_FILE_SIZE) {
                                    Ok(content) => {
                                        let mut hasher = Sha256::new();
                                        hasher.update(&content);
                                        format!("{:x}", hasher.finalize())
                                    }
                                    Err(_) => String::new(),
                                }
                            });

                        // Add or update the whitelist entry
                        if let Some(existing) =
                            bash_state.whitelist_for_overwrite.get_mut(&file_path)
                        {
                            existing.file_hash = file_hash.clone();

                            // Get total lines from the cache
                            let total_lines = cache
                                .get_unread_ranges(Path::new(&file_path))
                                .iter()
                                .map(|&(_, end)| end)
                                .max()
                                .unwrap_or(0);

                            if total_lines > 0 {
                                existing.total_lines = total_lines;
                            }

                            for range in ranges {
                                existing.add_range(range.0, range.1);
                            }
                        } else {
                            // Create new entry
                            let total_lines = cache
                                .get_unread_ranges(Path::new(&file_path))
                                .iter()
                                .map(|&(_, end)| end)
                                .max()
                                .unwrap_or(ranges.iter().map(|&(_, end)| end).max().unwrap_or(0));

                            bash_state.whitelist_for_overwrite.insert(
                                file_path.clone(),
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
    });

    Ok(message)
}
