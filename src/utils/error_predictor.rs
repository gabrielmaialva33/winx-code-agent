//! Error prediction and prevention system
//!
//! This module provides functionality for predicting and preventing errors
//! in commands and file operations based on pattern recognition and
//! machine learning techniques.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::debug;

use crate::errors::WinxError;

/// Maximum number of errors to keep in history
const MAX_ERROR_HISTORY: usize = 100;

/// Error frequency threshold for prediction
const ERROR_FREQUENCY_THRESHOLD: f64 = 0.5;

/// Confidence threshold for prediction
const PREDICTION_CONFIDENCE_THRESHOLD: f64 = 0.7;

/// Maximum age for error history entries
const MAX_ERROR_AGE_HOURS: u64 = 24;

/// Error pattern to track
#[derive(Debug, Clone)]
pub struct ErrorPattern {
    /// Error type
    pub error_type: String,
    /// Error message pattern
    pub message_pattern: String,
    /// Command pattern that triggered the error
    pub command_pattern: Option<String>,
    /// File path pattern associated with the error
    pub file_pattern: Option<String>,
    /// Directory pattern associated with the error
    pub directory_pattern: Option<String>,
    /// Frequency of this error
    pub frequency: u32,
    /// Last time this error was seen
    pub last_seen: Instant,
}

/// Error prediction with confidence
#[derive(Debug, Clone)]
pub struct ErrorPrediction {
    /// Error type
    pub error_type: String,
    /// Error message pattern
    pub message_pattern: String,
    /// Confidence level (0.0-1.0)
    pub confidence: f64,
    /// Suggested prevention
    pub prevention: String,
}

/// Error history entry
#[derive(Debug, Clone)]
struct ErrorHistoryEntry {
    /// Error type
    pub error_type: String,
    /// Full error message
    pub message: String,
    /// Command that triggered the error (if applicable)
    pub command: Option<String>,
    /// File path associated with the error (if applicable)
    pub file_path: Option<String>,
    /// Directory associated with the error (if applicable)
    pub directory: Option<String>,
    /// Timestamp when the error occurred
    pub timestamp: Instant,
}

/// Error predictor and prevention system
#[derive(Debug)]
pub struct ErrorPredictor {
    /// Error history
    error_history: VecDeque<ErrorHistoryEntry>,
    /// Error patterns
    error_patterns: Vec<ErrorPattern>,
    /// File-specific errors
    file_errors: HashMap<String, Vec<String>>,
    /// Command-specific errors
    command_errors: HashMap<String, Vec<String>>,
    /// Directory-specific errors
    directory_errors: HashMap<String, Vec<String>>,
    /// Last cleanup time
    last_cleanup: Instant,
}

impl Default for ErrorPredictor {
    fn default() -> Self {
        Self::new()
    }
}

impl ErrorPredictor {
    /// Create a new error predictor
    pub fn new() -> Self {
        Self {
            error_history: VecDeque::with_capacity(MAX_ERROR_HISTORY),
            error_patterns: Vec::new(),
            file_errors: HashMap::new(),
            command_errors: HashMap::new(),
            directory_errors: HashMap::new(),
            last_cleanup: Instant::now(),
        }
    }

    /// Record an error for future pattern recognition
    pub fn record_error(
        &mut self,
        error_type: &str,
        message: &str,
        command: Option<&str>,
        file_path: Option<&str>,
        directory: Option<&str>,
    ) {
        debug!(
            "Recording error for pattern recognition: {} - {}",
            error_type, message
        );

        // Perform cleanup if needed
        if self.last_cleanup.elapsed() > Duration::from_secs(3600) {
            self.cleanup_old_entries();
        }

        // Create error history entry
        let entry = ErrorHistoryEntry {
            error_type: error_type.to_string(),
            message: message.to_string(),
            command: command.map(|s| s.to_string()),
            file_path: file_path.map(|s| s.to_string()),
            directory: directory.map(|s| s.to_string()),
            timestamp: Instant::now(),
        };

        // Add to history
        self.error_history.push_back(entry);

        // Ensure we don't exceed max history size
        if self.error_history.len() > MAX_ERROR_HISTORY {
            self.error_history.pop_front();
        }

        // Update error patterns
        self.update_error_patterns(error_type, message, command, file_path, directory);

        // Update specific mappings
        if let Some(file) = file_path {
            self.file_errors
                .entry(file.to_string())
                .or_default()
                .push(message.to_string());
        }

        if let Some(cmd) = command {
            let base_cmd = cmd.split_whitespace().next().unwrap_or(cmd);
            self.command_errors
                .entry(base_cmd.to_string())
                .or_default()
                .push(message.to_string());
        }

        if let Some(dir) = directory {
            self.directory_errors
                .entry(dir.to_string())
                .or_default()
                .push(message.to_string());
        }

        // Log updated patterns
        debug!(
            "Updated error patterns, now have {} patterns",
            self.error_patterns.len()
        );
    }

    /// Update error patterns based on the latest error
    fn update_error_patterns(
        &mut self,
        error_type: &str,
        message: &str,
        command: Option<&str>,
        file_path: Option<&str>,
        directory: Option<&str>,
    ) {
        // Extract key pattern from the message
        let pattern = self.extract_error_pattern(message);

        // Process command and file patterns outside the loop to avoid borrowing issues
        let cmd_pattern = command.map(|cmd| self.extract_command_pattern(cmd));
        let file_pattern = file_path.map(|file| self.extract_file_pattern(file));

        // Create a utility function to avoid borrowing self when iterating
        let generalize_patterns = |a: &str, b: &str| -> String {
            if a == b {
                a.to_string()
            } else if a.starts_with("*.") && b.starts_with("*.") {
                "*.{ext}".to_string()
            } else if a.contains('/') && b.contains('/') {
                "*/{name}".to_string()
            } else if a.starts_with('/') && b.starts_with('/') {
                "/{path}".to_string()
            } else {
                "*".to_string()
            }
        };

        // Check if we have a matching pattern already
        for existing_pattern in &mut self.error_patterns {
            if existing_pattern.error_type == error_type
                && existing_pattern.message_pattern == pattern
            {
                // Update existing pattern
                existing_pattern.frequency += 1;
                existing_pattern.last_seen = Instant::now();

                // Update command pattern if it makes sense
                if let Some(cmd_pat) = &cmd_pattern {
                    match &existing_pattern.command_pattern {
                        Some(existing_cmd) if existing_cmd != cmd_pat => {
                            // Make pattern more generic if they differ
                            let generalized = generalize_patterns(existing_cmd, cmd_pat);
                            existing_pattern.command_pattern = Some(generalized);
                        }
                        None => {
                            existing_pattern.command_pattern = Some(cmd_pat.clone());
                        }
                        _ => {}
                    }
                }

                // Update file pattern if it makes sense
                if let Some(file_pat) = &file_pattern {
                    match &existing_pattern.file_pattern {
                        Some(existing_file) if existing_file != file_pat => {
                            // Make pattern more generic if they differ
                            let generalized = generalize_patterns(existing_file, file_pat);
                            existing_pattern.file_pattern = Some(generalized);
                        }
                        None => {
                            existing_pattern.file_pattern = Some(file_pat.clone());
                        }
                        _ => {}
                    }
                }

                return;
            }
        }

        // Create a new pattern if no match found
        let dir_pattern = directory.map(|dir| self.extract_directory_pattern(dir));

        let new_pattern = ErrorPattern {
            error_type: error_type.to_string(),
            message_pattern: pattern,
            command_pattern: cmd_pattern,
            file_pattern,
            directory_pattern: dir_pattern,
            frequency: 1,
            last_seen: Instant::now(),
        };

        self.error_patterns.push(new_pattern);
    }

    /// Extract a command pattern from a command string
    fn extract_command_pattern(&self, command: &str) -> String {
        let parts: Vec<&str> = command.split_whitespace().collect();

        if parts.is_empty() {
            return String::new();
        }

        // The first word is usually the command name - keep it specific
        let mut pattern = parts[0].to_string();

        // For arguments, we may want to generalize
        if parts.len() > 1 {
            // Add generic pattern for options
            if parts.iter().any(|p| p.starts_with('-')) {
                pattern.push_str(" [options]");
            }

            // Add generic pattern for paths
            if parts.iter().any(|p| p.contains('/')) {
                pattern.push_str(" [path]");
            }

            // Add generic pattern for multiple arguments
            if parts.len() > 2 {
                pattern.push_str(" [args]");
            }
        }

        pattern
    }

    /// Extract a file pattern from a file path
    fn extract_file_pattern(&self, file_path: &str) -> String {
        let path = Path::new(file_path);

        if let Some(extension) = path.extension() {
            if let Some(ext_str) = extension.to_str() {
                // For files, we often care about the extension
                return format!("*.{}", ext_str);
            }
        }

        // If no extension or error, use a more generic pattern
        if let Some(file_name) = path.file_name() {
            if let Some(name) = file_name.to_str() {
                return name.to_string();
            }
        }

        // Fallback
        "*".to_string()
    }

    /// Extract a directory pattern from a directory path
    fn extract_directory_pattern(&self, directory: &str) -> String {
        let path = Path::new(directory);

        // We often care about the last component of the directory
        if let Some(last_component) = path.file_name() {
            if let Some(name) = last_component.to_str() {
                return format!("*/{}", name);
            }
        }

        // Fallback
        "*".to_string()
    }

    /// Extract a error pattern from an error message
    fn extract_error_pattern(&self, message: &str) -> String {
        // Remove numbers and specific values
        let mut pattern = message.to_string();

        // Replace specific file paths with placeholders
        pattern = regex::Regex::new(r"[/\\][^\s:;,]+")
            .unwrap_or_else(|_| regex::Regex::new(r"xxx").unwrap())
            .replace_all(&pattern, "[PATH]")
            .to_string();

        // Replace numbers with placeholders
        pattern = regex::Regex::new(r"\b\d+\b")
            .unwrap_or_else(|_| regex::Regex::new(r"xxx").unwrap())
            .replace_all(&pattern, "[NUM]")
            .to_string();

        // Replace specific function or error names
        pattern = regex::Regex::new(r"'[^']+'")
            .unwrap_or_else(|_| regex::Regex::new(r"xxx").unwrap())
            .replace_all(&pattern, "'[NAME]'")
            .to_string();

        // Replace quoted strings with placeholders
        pattern = regex::Regex::new(r#""[^"]+""#)
            .unwrap_or_else(|_| regex::Regex::new(r"xxx").unwrap())
            .replace_all(&pattern, "\"[STRING]\"")
            .to_string();

        pattern
    }

    /// Generalize two patterns into a more general one
    fn generalize_pattern(&self, pattern1: &str, pattern2: &str) -> String {
        if pattern1 == pattern2 {
            return pattern1.to_string();
        }

        // Simple generalizations
        if pattern1.starts_with("*.") && pattern2.starts_with("*.") {
            return "*.{ext}".to_string();
        }

        if pattern1.contains('/') && pattern2.contains('/') {
            return "*/{name}".to_string();
        }

        // If both are paths
        if pattern1.starts_with('/') && pattern2.starts_with('/') {
            return "/{path}".to_string();
        }

        // Default: very generic
        "*".to_string()
    }

    /// Predict potential errors for a command
    pub fn predict_command_errors(&self, command: &str) -> Vec<ErrorPrediction> {
        let mut predictions = Vec::new();

        // Extract base command
        let base_command = command.split_whitespace().next().unwrap_or(command);

        // Check command-specific error history
        if let Some(errors) = self.command_errors.get(base_command) {
            // If we've seen many errors with this command, predict them
            if errors.len() > 2 {
                // Group and count similar errors
                let mut error_counts: HashMap<String, u32> = HashMap::new();
                for error in errors {
                    let pattern = self.extract_error_pattern(error);
                    *error_counts.entry(pattern).or_insert(0) += 1;
                }

                // Find the most common error
                if let Some((pattern, count)) = error_counts.iter().max_by_key(|(_, &count)| count)
                {
                    let frequency = *count as f64 / errors.len() as f64;
                    if frequency >= ERROR_FREQUENCY_THRESHOLD {
                        let suggestion = self.get_suggestion_for_error(base_command, pattern);
                        predictions.push(ErrorPrediction {
                            error_type: "command_error".to_string(),
                            message_pattern: pattern.clone(),
                            confidence: frequency * 0.9,
                            prevention: suggestion,
                        });
                    }
                }
            }
        }

        // Check patterns
        for pattern in &self.error_patterns {
            if let Some(cmd_pattern) = &pattern.command_pattern {
                if self.pattern_matches(cmd_pattern, command) {
                    // This pattern might apply to this command
                    let suggestion =
                        self.get_suggestion_for_error(command, &pattern.message_pattern);

                    let base_confidence = pattern.frequency as f64 / 10.0;
                    let decay_factor = 1.0
                        - (pattern.last_seen.elapsed().as_secs() as f64
                            / (MAX_ERROR_AGE_HOURS * 3600) as f64)
                            .min(1.0);

                    let confidence = (base_confidence * decay_factor).min(1.0);

                    if confidence >= PREDICTION_CONFIDENCE_THRESHOLD {
                        predictions.push(ErrorPrediction {
                            error_type: pattern.error_type.clone(),
                            message_pattern: pattern.message_pattern.clone(),
                            confidence,
                            prevention: suggestion,
                        });
                    }
                }
            }
        }

        predictions
    }

    /// Predict potential errors for a file operation
    pub fn predict_file_errors(&self, file_path: &str, operation: &str) -> Vec<ErrorPrediction> {
        let mut predictions = Vec::new();

        // Check file-specific error history
        if let Some(errors) = self.file_errors.get(file_path) {
            // If we've seen many errors with this file, predict them
            if errors.len() > 2 {
                // Group and count similar errors
                let mut error_counts: HashMap<String, u32> = HashMap::new();
                for error in errors {
                    let pattern = self.extract_error_pattern(error);
                    *error_counts.entry(pattern).or_insert(0) += 1;
                }

                // Find the most common error
                if let Some((pattern, count)) = error_counts.iter().max_by_key(|(_, &count)| count)
                {
                    let frequency = *count as f64 / errors.len() as f64;
                    if frequency >= ERROR_FREQUENCY_THRESHOLD {
                        let suggestion =
                            self.get_suggestion_for_file_error(file_path, pattern, operation);
                        predictions.push(ErrorPrediction {
                            error_type: "file_error".to_string(),
                            message_pattern: pattern.clone(),
                            confidence: frequency * 0.9,
                            prevention: suggestion,
                        });
                    }
                }
            }
        }

        // Check patterns
        let file_pattern = self.extract_file_pattern(file_path);
        for pattern in &self.error_patterns {
            if let Some(pat) = &pattern.file_pattern {
                if self.pattern_matches(pat, &file_pattern) {
                    // This pattern might apply to this file
                    let suggestion = self.get_suggestion_for_file_error(
                        file_path,
                        &pattern.message_pattern,
                        operation,
                    );

                    let base_confidence = pattern.frequency as f64 / 10.0;
                    let decay_factor = 1.0
                        - (pattern.last_seen.elapsed().as_secs() as f64
                            / (MAX_ERROR_AGE_HOURS * 3600) as f64)
                            .min(1.0);

                    let confidence = (base_confidence * decay_factor).min(1.0);

                    if confidence >= PREDICTION_CONFIDENCE_THRESHOLD {
                        predictions.push(ErrorPrediction {
                            error_type: pattern.error_type.clone(),
                            message_pattern: pattern.message_pattern.clone(),
                            confidence,
                            prevention: suggestion,
                        });
                    }
                }
            }
        }

        // Check for common file operation errors
        self.add_common_file_operation_predictions(file_path, operation, &mut predictions);

        predictions
    }

    /// Check if a pattern matches a string
    fn pattern_matches(&self, pattern: &str, s: &str) -> bool {
        if pattern == "*" || pattern.is_empty() {
            return true;
        }

        if pattern.contains('*') {
            let pattern_parts: Vec<&str> = pattern.split('*').filter(|s| !s.is_empty()).collect();
            if pattern_parts.is_empty() {
                return true;
            }

            // Simple wildcard matching
            let mut remaining = s;
            for (i, part) in pattern_parts.iter().enumerate() {
                if i == 0 && pattern.starts_with(part) && !s.starts_with(part) {
                    return false;
                }

                if i == pattern_parts.len() - 1 && pattern.ends_with(part) && !s.ends_with(part) {
                    return false;
                }

                if let Some(pos) = remaining.find(part) {
                    remaining = &remaining[pos + part.len()..];
                } else {
                    return false;
                }
            }

            true
        } else {
            // Exact match
            pattern == s
        }
    }

    /// Add predictions for common file operations
    fn add_common_file_operation_predictions(
        &self,
        file_path: &str,
        operation: &str,
        predictions: &mut Vec<ErrorPrediction>,
    ) {
        let path = Path::new(file_path);

        // Check if the file exists
        let file_exists = path.exists();

        match operation {
            "read" => {
                if !file_exists {
                    predictions.push(ErrorPrediction {
                        error_type: "file_not_found".to_string(),
                        message_pattern: "File not found".to_string(),
                        confidence: 0.95,
                        prevention: format!("The file '{}' does not exist. Check the path or create the file first.", file_path),
                    });
                } else if path.is_dir() {
                    predictions.push(ErrorPrediction {
                        error_type: "is_a_directory".to_string(),
                        message_pattern: "Is a directory".to_string(),
                        confidence: 0.95,
                        prevention: format!(
                            "'{}' is a directory, not a file. Use a file path instead.",
                            file_path
                        ),
                    });
                }
            }
            "write" | "edit" => {
                if file_exists && path.is_dir() {
                    predictions.push(ErrorPrediction {
                        error_type: "is_a_directory".to_string(),
                        message_pattern: "Is a directory".to_string(),
                        confidence: 0.95,
                        prevention: format!(
                            "'{}' is a directory, not a file. Use a file path instead.",
                            file_path
                        ),
                    });
                }

                // Check if parent directory exists for new files
                if !file_exists {
                    if let Some(parent) = path.parent() {
                        if !parent.exists() {
                            predictions.push(ErrorPrediction {
                                error_type: "directory_not_found".to_string(),
                                message_pattern: "Directory not found".to_string(),
                                confidence: 0.9,
                                prevention: format!("The parent directory for '{}' does not exist. Create it first with mkdir -p.", parent.display()),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Get a suggestion for fixing a command error
    fn get_suggestion_for_error(&self, command: &str, error_pattern: &str) -> String {
        // Extract base command
        let base_command = command.split_whitespace().next().unwrap_or(command);

        // Command-specific suggestions
        match base_command {
            "git" => {
                if error_pattern.contains("permission denied") {
                    return "Check repository permissions or try with sudo".to_string();
                }
                if error_pattern.contains("not a git repository") {
                    return "Initialize a git repository with 'git init' or navigate to a valid git repository".to_string();
                }
                if error_pattern.contains("has no upstream branch") {
                    return "Set upstream branch with 'git push --set-upstream origin <branch>'"
                        .to_string();
                }
            }
            "npm" | "yarn" => {
                if error_pattern.contains("not found") {
                    return "Run 'npm install' or 'yarn install' to install dependencies"
                        .to_string();
                }
                if error_pattern.contains("permission") {
                    return "Try running with sudo or fix npm permissions".to_string();
                }
            }
            "cargo" => {
                if error_pattern.contains("not found") {
                    return "Make sure you are in a Rust project directory with a Cargo.toml file"
                        .to_string();
                }
                if error_pattern.contains("compile") {
                    return "Fix compilation errors in your Rust code".to_string();
                }
            }
            "mkdir" => {
                if error_pattern.contains("permission denied") {
                    return "Check directory permissions or try with sudo".to_string();
                }
                if error_pattern.contains("No such file") {
                    return "Use mkdir -p to create parent directories".to_string();
                }
            }
            "cd" => {
                if error_pattern.contains("No such file") {
                    return "The directory does not exist, check the path".to_string();
                }
                if error_pattern.contains("Not a directory") {
                    return "The path is a file, not a directory. Specify a directory path"
                        .to_string();
                }
            }
            _ => {}
        }

        // Generic suggestions based on error patterns
        if error_pattern.contains("permission denied") {
            return "Check file permissions or try with sudo".to_string();
        }
        if error_pattern.contains("not found") || error_pattern.contains("No such file") {
            return "Check that the file or command exists and the path is correct".to_string();
        }
        if error_pattern.contains("syntax") {
            return "Check command syntax and arguments".to_string();
        }

        // Default suggestion
        "Double-check command arguments and options".to_string()
    }

    /// Get a suggestion for fixing a file error
    fn get_suggestion_for_file_error(
        &self,
        file_path: &str,
        error_pattern: &str,
        operation: &str,
    ) -> String {
        let path = Path::new(file_path);

        // Operation-specific suggestions
        match operation {
            "read" => {
                if error_pattern.contains("not found") || error_pattern.contains("No such file") {
                    return format!(
                        "The file '{}' does not exist. Check the path or create it first",
                        file_path
                    );
                }
                if error_pattern.contains("permission denied") {
                    return format!("Check read permissions for '{}'", file_path);
                }
                if error_pattern.contains("directory") {
                    return format!("'{}' is a directory, not a file", file_path);
                }
            }
            "write" | "edit" => {
                if error_pattern.contains("permission denied") {
                    return format!("Check write permissions for '{}'", file_path);
                }
                if error_pattern.contains("directory") {
                    return format!("'{}' is a directory, not a file", file_path);
                }
                if error_pattern.contains("No such file or directory") {
                    if let Some(parent) = path.parent() {
                        return format!("Create the parent directory '{}' first", parent.display());
                    }
                }
            }
            _ => {}
        }

        // Generic suggestions
        if error_pattern.contains("permission denied") {
            return "Check file permissions or try with sudo".to_string();
        }
        if error_pattern.contains("not found") || error_pattern.contains("No such file") {
            return "Check that the file exists and the path is correct".to_string();
        }

        // Default suggestion
        "Verify the file path and permissions".to_string()
    }

    /// Clean up old entries from the error history
    fn cleanup_old_entries(&mut self) {
        debug!("Cleaning up old error history entries");

        let now = Instant::now();
        let max_age = Duration::from_secs(MAX_ERROR_AGE_HOURS * 3600);

        // Remove old entries from error history
        while let Some(entry) = self.error_history.front() {
            if now.duration_since(entry.timestamp) > max_age {
                self.error_history.pop_front();
            } else {
                break;
            }
        }

        // Remove old patterns
        self.error_patterns
            .retain(|pattern| now.duration_since(pattern.last_seen) <= max_age);

        self.last_cleanup = now;

        debug!(
            "Cleanup complete, have {} history entries and {} patterns",
            self.error_history.len(),
            self.error_patterns.len()
        );
    }
}

/// Shared error predictor that can be used from multiple tools
#[derive(Debug, Clone)]
pub struct SharedErrorPredictor {
    inner: Arc<Mutex<ErrorPredictor>>,
}

impl Default for SharedErrorPredictor {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedErrorPredictor {
    /// Create a new shared error predictor
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ErrorPredictor::new())),
        }
    }

    /// Record an error for future pattern recognition
    pub fn record_error(
        &self,
        error_type: &str,
        message: &str,
        command: Option<&str>,
        file_path: Option<&str>,
        directory: Option<&str>,
    ) -> Result<(), WinxError> {
        let mut predictor = self.inner.lock().map_err(|e| {
            WinxError::BashStateLockError(format!("Failed to lock error predictor: {}", e))
        })?;

        predictor.record_error(error_type, message, command, file_path, directory);
        Ok(())
    }

    /// Record a WinxError for future pattern recognition
    pub fn record_winx_error(
        &self,
        error: &WinxError,
        command: Option<&str>,
        directory: Option<&str>,
    ) -> Result<(), WinxError> {
        let error_type = match error {
            WinxError::ShellInitializationError(_) => "shell_init",
            WinxError::WorkspacePathError(_) => "workspace_path",
            WinxError::BashStateLockError(_) => "bash_state_lock",
            WinxError::BashStateNotInitialized => "bash_state_not_init",
            WinxError::CommandExecutionError(_) => "command_execution",
            WinxError::ArgumentParseError(_) => "argument_parse",
            WinxError::FileAccessError { .. } => "file_access",
            WinxError::CommandNotAllowed(_) => "command_not_allowed",
            WinxError::ThreadIdMismatch(_) => "thread_id_mismatch",
            WinxError::DeserializationError(_) => "deserialization",
            WinxError::SerializationError(_) => "serialization",
            WinxError::SearchReplaceSyntaxError(_) => "search_replace_syntax",
            WinxError::SearchBlockNotFound(_) => "search_block_not_found",
            WinxError::SearchBlockAmbiguous { .. } => "search_block_ambiguous",
            WinxError::SearchBlockConflict { .. } => "search_block_conflict",
            WinxError::SearchReplaceSyntaxErrorDetailed { .. } => "search_replace_syntax_detailed",
            WinxError::JsonParseError(_) => "json_parse",
            WinxError::FileTooLarge { .. } => "file_too_large",
            WinxError::FileWriteError { .. } => "file_write",
            WinxError::DataLoadingError(_) => "data_loading",
            WinxError::ParameterValidationError { .. } => "parameter_validation",
            WinxError::MissingParameterError { .. } => "missing_parameter",
            WinxError::NullValueError { .. } => "null_value",
            WinxError::RecoverableSuggestionError { .. } => "recoverable_suggestion",
            WinxError::ContextSaveError(_) => "context_save_error",
            WinxError::CommandTimeout { .. } => "command_timeout",
            WinxError::InteractiveCommandDetected { .. } => "interactive_command",
            WinxError::CommandAlreadyRunning { .. } => "command_already_running",
            WinxError::ProcessCleanupError { .. } => "process_cleanup",
            WinxError::BufferOverflow { .. } => "buffer_overflow",
            WinxError::SessionRecoveryError { .. } => "session_recovery",
            WinxError::ResourceAllocationError { .. } => "resource_allocation",
            WinxError::IoError(_) => "io_error",
            WinxError::ApiError(_) => "api_error",
            WinxError::NetworkError(_) => "network_error",
            WinxError::ConfigurationError(_) => "configuration_error",
            WinxError::ParseError(_) => "parse_error",
            WinxError::InvalidInput(_) => "invalid_input",
            WinxError::FileError(_) => "file_error",
            WinxError::AIError(_) => "ai_error",
        };

        let message = format!("{}", error);
        let file_path = match error {
            WinxError::FileAccessError { path, .. } => Some(path.to_string_lossy().to_string()),
            WinxError::FileWriteError { path, .. } => Some(path.to_string_lossy().to_string()),
            WinxError::FileTooLarge { path, .. } => Some(path.to_string_lossy().to_string()),
            _ => None,
        };

        self.record_error(
            error_type,
            &message,
            command,
            file_path.as_deref(),
            directory,
        )
    }

    /// Predict potential errors for a command
    pub fn predict_command_errors(&self, command: &str) -> Result<Vec<ErrorPrediction>, WinxError> {
        let predictor = self.inner.lock().map_err(|e| {
            WinxError::BashStateLockError(format!("Failed to lock error predictor: {}", e))
        })?;

        Ok(predictor.predict_command_errors(command))
    }

    /// Predict potential errors for a file operation
    pub fn predict_file_errors(
        &self,
        file_path: &str,
        operation: &str,
    ) -> Result<Vec<ErrorPrediction>, WinxError> {
        let predictor = self.inner.lock().map_err(|e| {
            WinxError::BashStateLockError(format!("Failed to lock error predictor: {}", e))
        })?;

        Ok(predictor.predict_file_errors(file_path, operation))
    }
}
