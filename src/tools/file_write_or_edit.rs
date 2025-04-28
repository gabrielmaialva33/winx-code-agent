//! Implementation of the FileWriteOrEdit tool.
//!
//! This module provides the implementation for the FileWriteOrEdit tool, which is used
//! to write or edit files, with support for both full file content and search/replace blocks.

use anyhow::Context as AnyhowContext;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::task;
use tracing::{debug, error, info, instrument, warn};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::{BashState, FileWhitelistData};
use crate::types::FileWriteOrEdit;
use crate::utils::path::expand_user;
// We'll be using this in future improvements
#[allow(unused_imports)]
use crate::utils::mmap::read_file_to_string;

// Regex patterns for search/replace blocks
// Create these with caching to improve performance
fn search_marker() -> &'static Regex {
    lazy_static::lazy_static! {
        static ref REGEX: Regex = Regex::new(r"^<<<<<<+\s*SEARCH\s*$").unwrap();
    }
    &REGEX
}

fn divider_marker() -> &'static Regex {
    lazy_static::lazy_static! {
        static ref REGEX: Regex = Regex::new(r"^======*\s*$").unwrap();
    }
    &REGEX
}

fn replace_marker() -> &'static Regex {
    lazy_static::lazy_static! {
        static ref REGEX: Regex = Regex::new(r"^>>>>>>+\s*REPLACE\s*$").unwrap();
    }
    &REGEX
}

/// Maximum file size to read
const MAX_FILE_SIZE: u64 = 50_000_000; // Increased to 50MB

/// Helper struct for search/replace operations
#[derive(Debug)]
struct SearchReplaceHelper {
    /// The original content
    original_content: String,
    /// The search/replace blocks
    blocks: Vec<(String, String)>,
    /// Debugging information
    debug_info: Vec<String>,
}

impl SearchReplaceHelper {
    /// Create a new instance from content and search/replace blocks
    fn new(original_content: String, blocks: Vec<(String, String)>) -> Self {
        Self {
            original_content,
            blocks,
            debug_info: Vec::new(),
        }
    }

    /// Apply the search/replace blocks to the original content
    fn apply(mut self) -> Result<String> {
        let mut content = self.original_content.clone();

        // Apply each block sequentially
        for (i, (search, replace)) in self.blocks.iter().enumerate() {
            if !content.contains(search) {
                // Collect debugging information
                self.debug_info
                    .push(format!("Block {} not found in content", i + 1));

                // Try to find approximate matches
                let suggestion = self
                    .find_closest_match(search, &content)
                    .unwrap_or_default();

                return Err(WinxError::SearchBlockNotFound(format!(
                    "Search block not found in content:\n```\n{}\n```\n\n{}\n\nThis might be due to mismatched whitespace, line endings, or the block doesn't exist exactly as specified. Consider using percentage_to_change > 50 to replace the entire file instead.",
                    search.trim(), suggestion
                )));
            }

            content = content.replace(search, replace);
        }

        Ok(content)
    }

    /// Find the closest match for a search block
    fn find_closest_match(&self, search: &str, content: &str) -> Option<String> {
        // Simple heuristic: check if the search block without whitespace is present
        let search_no_whitespace = search.replace(" ", "").replace("\n", "").replace("\t", "");
        let content_no_whitespace = content.replace(" ", "").replace("\n", "").replace("\t", "");

        if content_no_whitespace.contains(&search_no_whitespace) {
            return Some("Suggestion: Your search block might have different whitespace or line endings than the content.".to_string());
        }

        // Check if a substring of the search block is present (for approximate matches)
        let search_lines: Vec<&str> = search.lines().collect();
        if search_lines.len() > 1 {
            for line in search_lines {
                if line.trim().len() > 10 && content.contains(line.trim()) {
                    return Some(format!(
                        "Suggestion: Found one line of your search block in the content: '{}...'",
                        &line.trim()[..20.min(line.trim().len())]
                    ));
                }
            }
        }

        None
    }
}

/// Error raised during search/replace block parsing
#[derive(Debug, thiserror::Error)]
#[error("Search/Replace Syntax Error: {0}")]
struct SearchReplaceSyntaxError(String);

impl SearchReplaceSyntaxError {
    /// Create a new error with a detailed explanation and example
    fn with_help_text(message: impl Into<String>) -> Self {
        let msg = message.into();
        Self(format!(
            "{}\n---\n\nMake sure blocks are in correct sequence, and the markers are in separate lines:\n\n<<<<<<< SEARCH\n example old\n=======\n example new\n>>>>>>> REPLACE",
            msg
        ))
    }
}

/// Convert internal SearchReplaceSyntaxError to WinxError
impl From<SearchReplaceSyntaxError> for WinxError {
    fn from(err: SearchReplaceSyntaxError) -> Self {
        WinxError::SearchReplaceSyntaxError(err.0)
    }
}

/// Check if the content is an edit (search/replace blocks) or full content
///
/// This function examines the content to determine if it contains search/replace blocks
/// based on the specific markers and the percentage_to_change value.
///
/// # Arguments
///
/// * `content` - The content to examine
/// * `percentage` - The percentage of the file that will change
///
/// # Returns
///
/// True if the content contains search/replace blocks, false if it's full content
fn is_edit(content: &str, percentage: u32) -> bool {
    let lines: Vec<&str> = content
        .lstrip_matches(char::is_whitespace)
        .lines()
        .collect();

    if lines.is_empty() {
        return false;
    }

    // Check first line for search marker
    if search_marker().is_match(lines[0]) {
        return true;
    }

    // For lower percentage changes, check for any marker in the content
    if percentage <= 50 {
        for line in &lines {
            if search_marker().is_match(line)
                || divider_marker().is_match(line)
                || replace_marker().is_match(line)
            {
                return true;
            }
        }
    }

    false
}

/// Get context for syntax errors
///
/// This function extracts a section of the file around the errors
/// to provide context for debugging.
///
/// # Arguments
///
/// * `file_content` - The entire file content
/// * `error_line` - The line number where the error occurred
///
/// # Returns
///
/// A string containing the context around the error
#[allow(dead_code)]
fn get_context_for_errors(file_content: &str, error_line: usize) -> String {
    let lines: Vec<&str> = file_content.lines().collect();
    let min_line = error_line.saturating_sub(5);
    let max_line = (error_line + 5).min(lines.len());

    let context_lines = &lines[min_line..max_line];
    format!("```\n{}\n```", context_lines.join("\n"))
}

/// Parse search/replace blocks from content
///
/// This function parses search/replace blocks from the content and returns
/// a vector of (search, replace) tuples.
///
/// # Arguments
///
/// * `content` - The content containing search/replace blocks
///
/// # Returns
///
/// A vector of (search, replace) tuples
///
/// # Errors
///
/// Returns an error if the search/replace blocks are malformed
fn parse_search_replace_blocks(
    content: &str,
) -> std::result::Result<Vec<(String, String)>, SearchReplaceSyntaxError> {
    // Check for empty content first
    if content.trim().is_empty() {
        return Err(SearchReplaceSyntaxError::with_help_text(
            "No search/replace blocks found in empty content",
        ));
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut blocks = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        if search_marker().is_match(lines[i]) {
            let line_num = i + 1;
            let mut search_block = Vec::new();
            i += 1;

            // Read the search block
            while i < lines.len() && !divider_marker().is_match(lines[i]) {
                if search_marker().is_match(lines[i]) || replace_marker().is_match(lines[i]) {
                    return Err(SearchReplaceSyntaxError::with_help_text(format!(
                        "Line {}: Found stray marker in SEARCH block: {}",
                        i + 1,
                        lines[i]
                    )));
                }
                search_block.push(lines[i]);
                i += 1;
            }

            if i >= lines.len() {
                return Err(SearchReplaceSyntaxError::with_help_text(format!(
                    "Line {}: Unclosed SEARCH block - missing ======= marker",
                    line_num
                )));
            }

            if search_block.is_empty() {
                return Err(SearchReplaceSyntaxError::with_help_text(format!(
                    "Line {}: SEARCH block cannot be empty. You must include content to search for between the SEARCH and ======= markers",
                    line_num
                )));
            }

            // Check for whitespace-only search blocks
            let search_string = search_block.join("\n");
            let search_content = search_string.trim();
            if search_content.is_empty() {
                return Err(SearchReplaceSyntaxError::with_help_text(format!(
                    "Line {}: SEARCH block contains only whitespace. You must include non-whitespace content to search for",
                    line_num
                )));
            }

            i += 1; // Skip the divider
            let mut replace_block = Vec::new();

            // Read the replace block
            while i < lines.len() && !replace_marker().is_match(lines[i]) {
                if search_marker().is_match(lines[i]) || divider_marker().is_match(lines[i]) {
                    return Err(SearchReplaceSyntaxError::with_help_text(format!(
                        "Line {}: Found stray marker in REPLACE block: {}",
                        i + 1,
                        lines[i]
                    )));
                }
                replace_block.push(lines[i]);
                i += 1;
            }

            if i >= lines.len() {
                return Err(SearchReplaceSyntaxError::with_help_text(format!(
                    "Line {}: Unclosed block - missing REPLACE marker",
                    line_num
                )));
            }

            i += 1; // Skip the replace marker

            blocks.push((search_block.join("\n"), replace_block.join("\n")));
        } else {
            if replace_marker().is_match(lines[i]) || divider_marker().is_match(lines[i]) {
                return Err(SearchReplaceSyntaxError::with_help_text(format!(
                    "Line {}: Found stray marker outside block: {}",
                    i + 1,
                    lines[i]
                )));
            }
            i += 1;
        }
    }

    if blocks.is_empty() {
        return Err(SearchReplaceSyntaxError::with_help_text(
            "No valid search replace blocks found, ensure your SEARCH/REPLACE blocks are formatted correctly".to_string()
        ));
    }

    Ok(blocks)
}

/// Apply search/replace blocks to content
///
/// This function applies the search/replace blocks to the original content.
///
/// # Arguments
///
/// * `blocks` - Vector of (search, replace) tuples
/// * `original_content` - The original content to modify
///
/// # Returns
///
/// The modified content
fn apply_search_replace_blocks(
    blocks: Vec<(String, String)>,
    original_content: String,
) -> Result<String> {
    // Create a helper and apply the blocks
    let helper = SearchReplaceHelper::new(original_content, blocks);

    // The helper does the actual work and provides better error messages
    helper.apply()
    // We don't need to log here as the error is already logged at the call site
}

/// Write content to a file with optimized buffering
///
/// This function writes content to a file using a buffered writer for better performance,
/// creating parent directories if needed.
///
/// # Arguments
///
/// * `path` - Path to the file
/// * `content` - Content to write
///
/// # Returns
///
/// Result indicating success or failure
fn write_to_file(path: &Path, content: &str) -> Result<()> {
    // Create parent directories if they don't exist
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("Failed to create parent directories")?;
    }

    // Calculate an appropriate buffer size based on content length
    // Min 64KB, max 8MB buffer
    let buffer_size = content.len().clamp(64 * 1024, 8 * 1024 * 1024);

    // Use a buffered writer for performance
    let file = fs::File::create(path).context("Failed to create file")?;
    let mut writer = BufWriter::with_capacity(buffer_size, file);

    // Write content in chunks for large files to avoid excessive memory usage
    let content_bytes = content.as_bytes();
    const CHUNK_SIZE: usize = 1024 * 1024; // 1MB chunks

    if content_bytes.len() > CHUNK_SIZE * 10 {
        // For very large content, write in chunks
        for chunk in content_bytes.chunks(CHUNK_SIZE) {
            writer
                .write_all(chunk)
                .context("Failed to write chunk to file")?;
        }
    } else {
        // For smaller content, write all at once
        writer
            .write_all(content_bytes)
            .context("Failed to write to file")?;
    }

    // Ensure data is flushed to disk
    writer.flush().context("Failed to flush data to file")?;

    Ok(())
}

/// Check if a file can be overwritten
///
/// This function checks if a file can be overwritten based on whitelist data.
///
/// # Arguments
///
/// * `file_path` - Path to the file
/// * `bash_state` - Bash state containing whitelist data
///
/// # Returns
///
/// Ok(()) if the file can be overwritten, or an error if not
fn check_can_overwrite(file_path: &str, bash_state: &BashState) -> Result<()> {
    // If file doesn't exist, no need to check
    if !Path::new(file_path).exists() {
        return Ok(());
    }

    // Check if file is in whitelist
    if !bash_state.whitelist_for_overwrite.contains_key(file_path) {
        return Err(WinxError::FileAccessError {
            path: PathBuf::from(file_path),
            message: "You need to read the file at least once before it can be overwritten. Use the ReadFiles tool with this file path first."
                .to_string(),
        });
    }

    // Check if file has changed since last read
    let file_content = fs::read(file_path).context("Failed to read file")?;
    let curr_hash = format!("{:x}", Sha256::digest(&file_content));

    let whitelist_data = &bash_state.whitelist_for_overwrite[file_path];

    if curr_hash != whitelist_data.file_hash {
        return Err(WinxError::FileAccessError {
            path: PathBuf::from(file_path),
            message: "The file has changed since it was last read. Use the ReadFiles tool to read the current version before modifying.".to_string(),
        });
    }

    // Check if enough of the file has been read
    if !whitelist_data.is_read_enough() {
        let unread_ranges = whitelist_data.get_unread_ranges();
        let ranges_str = unread_ranges
            .iter()
            .map(|(start, end)| format!("{}-{}", start, end))
            .collect::<Vec<_>>()
            .join(", ");

        return Err(WinxError::FileAccessError {
            path: PathBuf::from(file_path),
            message: format!(
                "You need to read more of the file before it can be overwritten. Unread line ranges: {}. Use the ReadFiles tool with line range specifications to read these sections.",
                ranges_str
            ),
        });
    }

    Ok(())
}

/// Check if a file path is allowed by the current mode
///
/// This function checks if a file path is allowed by the current mode's glob patterns.
///
/// # Arguments
///
/// * `file_path` - Path to the file
/// * `bash_state` - Bash state containing mode data
///
/// # Returns
///
/// Ok(()) if the file path is allowed, or an error if not
fn check_path_allowed(file_path: &str, bash_state: &BashState) -> Result<()> {
    use crate::types::AllowedGlobs;

    let allowed_globs = &bash_state.write_if_empty_mode.allowed_globs;

    match allowed_globs {
        AllowedGlobs::All(s) if s == "all" => Ok(()),
        AllowedGlobs::List(globs) => {
            // Check if file path matches any allowed globs
            let path = Path::new(file_path);

            for glob_pattern in globs {
                if glob::Pattern::new(glob_pattern)
                    .map(|pattern| pattern.matches_path(path))
                    .unwrap_or(false)
                {
                    return Ok(());
                }
            }

            Err(WinxError::CommandNotAllowed(format!(
                "Updating file {} not allowed in current mode. Doesn't match allowed globs: {:?}",
                file_path, globs
            )))
        }
        _ => Err(WinxError::CommandNotAllowed(
            "No file paths are allowed in current mode".to_string(),
        )),
    }
}

/// Handle the FileWriteOrEdit tool call
///
/// This function processes the FileWriteOrEdit tool call, which writes or edits files.
///
/// # Arguments
///
/// * `bash_state_arc` - Shared reference to the bash state
/// * `file_write_or_edit` - The file write or edit parameters
///
/// # Returns
///
/// A Result containing the response message to send to the client
///
/// # Errors
///
/// Returns an error if the file operation fails for any reason
#[instrument(level = "info", skip(bash_state_arc, file_write_or_edit))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    file_write_or_edit: FileWriteOrEdit,
) -> Result<String> {
    info!("FileWriteOrEdit tool called with: {:?}", file_write_or_edit);

    // Extract data we need from the bash state before awaiting
    let (chat_id, cwd, file_path);

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
        chat_id = bash_state.current_chat_id.clone();
        cwd = bash_state.cwd.clone();

        // Verify chat ID matches
        if file_write_or_edit.chat_id != chat_id {
            warn!(
                "Chat ID mismatch: expected {}, got {}",
                chat_id, file_write_or_edit.chat_id
            );
            return Err(WinxError::ChatIdMismatch(format!(
                "Error: No saved bash state found for chat ID \"{}\". Please initialize first with this ID.",
                file_write_or_edit.chat_id
            )));
        }

        // Expand the path
        let expanded_path = expand_user(&file_write_or_edit.file_path);

        // Ensure path is absolute
        file_path = if Path::new(&expanded_path).is_absolute() {
            expanded_path
        } else {
            // Use current working directory if path is relative
            cwd.join(&expanded_path).to_string_lossy().to_string()
        };

        // Check if file path is allowed
        check_path_allowed(&file_path, bash_state)?;

        // Check if file can be overwritten (if it exists)
        if Path::new(&file_path).exists() {
            check_can_overwrite(&file_path, bash_state)?;
        }
    }

    // Process based on content type (full content or search/replace blocks)
    let content = &file_write_or_edit.file_content_or_search_replace_blocks;

    // Determine if this is an edit or a full file write
    let is_edit_operation = is_edit(content, file_write_or_edit.percentage_to_change);

    if is_edit_operation {
        // This is a search/replace edit operation
        debug!("Processing as search/replace edit operation");

        // Read the original file content
        let file_path_obj = Path::new(&file_path);
        if !file_path_obj.exists() {
            return Err(WinxError::FileAccessError {
                path: file_path_obj.to_path_buf(),
                message: "File does not exist, cannot perform search/replace edit. Use percentage_to_change > 50 to create a new file.".to_string(),
            });
        }

        // Get file metadata
        let metadata = match fs::metadata(file_path_obj) {
            Ok(m) => m,
            Err(e) => {
                tracing::error!("Failed to get file metadata: {}", e);
                return Err(WinxError::FileAccessError {
                    path: file_path_obj.to_path_buf(),
                    message: format!(
                        "Failed to get file metadata: {}. Check file permissions.",
                        e
                    ),
                });
            }
        };

        // Check file size
        if metadata.len() > MAX_FILE_SIZE {
            return Err(WinxError::FileTooLarge {
                path: file_path_obj.to_path_buf(),
                size: metadata.len(),
                max_size: MAX_FILE_SIZE,
            });
        }

        // Read the file content
        let original_content = match fs::read_to_string(file_path_obj) {
            Ok(content) => content,
            Err(e) => {
                tracing::error!("Failed to read file for search/replace edit: {}", e);
                return Err(WinxError::FileAccessError {
                    path: file_path_obj.to_path_buf(),
                    message: format!("Failed to read file: {}. The file might be binary or have encoding issues.", e),
                });
            }
        };

        // Parse search/replace blocks
        let blocks = match parse_search_replace_blocks(content) {
            Ok(blocks) => {
                tracing::info!("Successfully parsed {} search/replace blocks", blocks.len());
                blocks
            }
            Err(e) => {
                tracing::error!("Error parsing search/replace blocks: {}", e);
                // Convert the error directly using From implementation
                return Err(e.into());
            }
        };

        // Apply search/replace blocks
        let new_content = match apply_search_replace_blocks(blocks, original_content) {
            Ok(content) => content,
            Err(e) => {
                // Only log the error once at this level and avoid duplicating in error message
                tracing::error!(
                    "Error applying search/replace blocks for file {}: {}",
                    file_path_obj.display(),
                    e
                );
                return Err(e);
            }
        };

        // Write the new content to the file
        write_to_file(file_path_obj, &new_content)
            .context("Failed to write edited content to file")?;

        // Count lines for tracking
        let total_lines = new_content.lines().count();

        // Update whitelist data asynchronously
        let file_path_clone = file_path.clone();
        let bash_state_arc_clone = Arc::clone(bash_state_arc);
        task::spawn_blocking(move || {
            if let Ok(mut bash_state_guard) = bash_state_arc_clone.lock() {
                if let Some(bash_state) = bash_state_guard.as_mut() {
                    // Calculate file hash
                    let file_content = fs::read(&file_path_clone).ok()?;
                    let file_hash = format!("{:x}", Sha256::digest(&file_content));

                    // The line range represents the entire file (1 to total_lines)
                    let line_range = (1, total_lines);

                    // Update or create whitelist entry
                    if let Some(whitelist_data) =
                        bash_state.whitelist_for_overwrite.get_mut(&file_path_clone)
                    {
                        whitelist_data.file_hash = file_hash;
                        whitelist_data.total_lines = total_lines;
                        whitelist_data.add_range(line_range.0, line_range.1);
                    } else {
                        bash_state.whitelist_for_overwrite.insert(
                            file_path_clone,
                            FileWhitelistData::new(file_hash, vec![line_range], total_lines),
                        );
                    }
                }
                Some(())
            } else {
                None
            }
        });

        Ok(format!("Successfully edited file {}", file_path))
    } else {
        // This is a full file write operation
        debug!("Processing as full file write operation");

        // Get absolute path
        let file_path_obj = Path::new(&file_path);

        // Write the content to the file
        write_to_file(file_path_obj, content).context("Failed to write content to file")?;

        // Count lines for tracking
        let total_lines = content.lines().count();

        // Update whitelist data asynchronously
        let file_path_clone = file_path.clone();
        let bash_state_arc_clone = Arc::clone(bash_state_arc);
        task::spawn_blocking(move || {
            if let Ok(mut bash_state_guard) = bash_state_arc_clone.lock() {
                if let Some(bash_state) = bash_state_guard.as_mut() {
                    // Calculate file hash
                    let file_content = fs::read(&file_path_clone).ok()?;
                    let file_hash = format!("{:x}", Sha256::digest(&file_content));

                    // The line range represents the entire file (1 to total_lines)
                    let line_range = (1, total_lines);

                    // Update or create whitelist entry
                    if let Some(whitelist_data) =
                        bash_state.whitelist_for_overwrite.get_mut(&file_path_clone)
                    {
                        whitelist_data.file_hash = file_hash;
                        whitelist_data.total_lines = total_lines;
                        whitelist_data.add_range(line_range.0, line_range.1);
                    } else {
                        bash_state.whitelist_for_overwrite.insert(
                            file_path_clone,
                            FileWhitelistData::new(file_hash, vec![line_range], total_lines),
                        );
                    }
                }
                Some(())
            } else {
                None
            }
        });

        Ok(format!("Successfully wrote file {}", file_path))
    }
}

// Helper trait to add lstrip_matches
trait StrExt {
    fn lstrip_matches<P>(&self, pat: P) -> &Self
    where
        P: FnMut(char) -> bool;
}

impl StrExt for str {
    fn lstrip_matches<P>(&self, mut pat: P) -> &Self
    where
        P: FnMut(char) -> bool,
    {
        let chars = self.char_indices();

        for (idx, c) in chars {
            if !pat(c) {
                return &self[idx..];
            }
        }

        ""
    }
}
