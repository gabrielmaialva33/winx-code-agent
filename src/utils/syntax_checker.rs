//! Syntax checking module for post-edit validation
//!
//! This module provides syntax checking capabilities for edited files,
//! matching the behavior of wcgw Python's `syntax_checker`.
//! Currently provides a basic implementation that can be expanded with
//! tree-sitter support in the future.

use std::path::Path;

/// Output from syntax checking
#[derive(Debug, Clone, Default)]
pub struct SyntaxCheckOutput {
    /// Description of syntax errors found (empty if no errors)
    pub description: String,
    /// List of error locations as (line, column) pairs
    pub errors: Vec<(usize, usize)>,
    /// Whether the syntax check was actually performed
    pub was_checked: bool,
}

impl SyntaxCheckOutput {
    /// Create a new empty output (no errors)
    pub fn ok() -> Self {
        Self {
            description: String::new(),
            errors: Vec::new(),
            was_checked: true,
        }
    }

    /// Create output indicating syntax checking is not available
    pub fn not_available() -> Self {
        Self {
            description: String::new(),
            errors: Vec::new(),
            was_checked: false,
        }
    }

    /// Create output with errors
    pub fn with_errors(description: String, errors: Vec<(usize, usize)>) -> Self {
        Self {
            description,
            errors,
            was_checked: true,
        }
    }

    /// Check if there are any syntax errors
    pub fn has_errors(&self) -> bool {
        !self.description.is_empty() || !self.errors.is_empty()
    }
}

/// Extensions that support syntax checking
/// These are the extensions for which we can provide at least basic validation
const CHECKABLE_EXTENSIONS: &[&str] = &[
    // JSON - can validate structure
    "json",
    // TOML - can validate structure
    "toml",
    // YAML - can validate structure
    "yaml", "yml",
];

/// Check if syntax checking is available for this file type
pub fn is_syntax_check_available(extension: &str) -> bool {
    CHECKABLE_EXTENSIONS.contains(&extension.to_lowercase().as_str())
}

/// Check syntax of content based on file extension
///
/// This is the main entry point matching wcgw Python's `check_syntax` function.
///
/// # Arguments
/// * `extension` - File extension (without dot)
/// * `content` - File content to check
///
/// # Returns
/// `SyntaxCheckOutput` with any errors found
pub fn check_syntax(extension: &str, content: &str) -> SyntaxCheckOutput {
    let ext_lower = extension.to_lowercase();

    match ext_lower.as_str() {
        "json" => check_json_syntax(content),
        "toml" => check_toml_syntax(content),
        "yaml" | "yml" => check_yaml_syntax(content),
        _ => SyntaxCheckOutput::not_available(),
    }
}

/// Check syntax of a file by path
pub fn check_file_syntax(path: &Path) -> SyntaxCheckOutput {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match std::fs::read_to_string(path) {
        Ok(content) => check_syntax(extension, &content),
        Err(e) => SyntaxCheckOutput::with_errors(
            format!("Failed to read file: {e}"),
            Vec::new(),
        ),
    }
}

/// Check JSON syntax
fn check_json_syntax(content: &str) -> SyntaxCheckOutput {
    match serde_json::from_str::<serde_json::Value>(content) {
        Ok(_) => SyntaxCheckOutput::ok(),
        Err(e) => {
            let line = e.line();
            let column = e.column();
            SyntaxCheckOutput::with_errors(
                format!("JSON syntax error at line {line}, column {column}: {e}"),
                vec![(line, column)],
            )
        }
    }
}

/// Check TOML syntax
fn check_toml_syntax(content: &str) -> SyntaxCheckOutput {
    // Basic TOML validation using pattern matching
    // Full validation would require a TOML parser dependency

    let mut errors = Vec::new();
    let mut error_messages = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Check for unclosed brackets in table headers
        if trimmed.starts_with('[') && !trimmed.ends_with(']') {
            errors.push((line_num + 1, 1));
            error_messages.push(format!(
                "Line {}: Unclosed bracket in table header",
                line_num + 1
            ));
        }

        // Check for key-value pairs without equals sign
        if !trimmed.starts_with('[') && !trimmed.contains('=') && !trimmed.is_empty() {
            // Could be a multi-line value, so just warn
            // errors.push((line_num + 1, 1));
        }
    }

    if errors.is_empty() {
        SyntaxCheckOutput::ok()
    } else {
        SyntaxCheckOutput::with_errors(error_messages.join("\n"), errors)
    }
}

/// Check YAML syntax
fn check_yaml_syntax(content: &str) -> SyntaxCheckOutput {
    // Basic YAML validation using pattern matching
    // Full validation would require a YAML parser dependency

    let mut errors = Vec::new();
    let mut error_messages = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        // Check for tabs (YAML doesn't allow tabs for indentation)
        if line.starts_with('\t') {
            errors.push((line_num + 1, 1));
            error_messages.push(format!(
                "Line {}: YAML does not allow tabs for indentation",
                line_num + 1
            ));
        }

        // Check for mixing spaces and tabs
        let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        if indent.contains('\t') && indent.contains(' ') {
            errors.push((line_num + 1, 1));
            error_messages.push(format!(
                "Line {}: Mixed tabs and spaces in indentation",
                line_num + 1
            ));
        }
    }

    if errors.is_empty() {
        SyntaxCheckOutput::ok()
    } else {
        SyntaxCheckOutput::with_errors(error_messages.join("\n"), errors)
    }
}

/// Format syntax error context for display (matches wcgw Python behavior)
///
/// # Arguments
/// * `errors` - List of (line, column) error locations
/// * `file_content` - Content of the file
/// * `filename` - Name of the file
///
/// # Returns
/// Formatted string with context around errors
pub fn format_syntax_error_context(
    errors: &[(usize, usize)],
    file_content: &str,
    filename: &str,
) -> String {
    if errors.is_empty() {
        return String::new();
    }

    let lines: Vec<&str> = file_content.lines().collect();
    let min_line = errors.iter().map(|(l, _)| *l).min().unwrap_or(1);
    let max_line = errors.iter().map(|(l, _)| *l).max().unwrap_or(1);

    // Get context: 10 lines before and 5 lines after
    let start_line = min_line.saturating_sub(10);
    let end_line = (max_line + 5).min(lines.len());

    let context_lines: Vec<&str> = lines
        .iter()
        .skip(start_line)
        .take(end_line - start_line)
        .copied()
        .collect();

    let context = context_lines.join("\n");

    format!(
        "Here's relevant snippet from {filename} where the syntax errors occurred:\n<snippet>\n{context}\n</snippet>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_valid() {
        let result = check_syntax("json", r#"{"key": "value"}"#);
        assert!(!result.has_errors());
        assert!(result.was_checked);
    }

    #[test]
    fn test_json_invalid() {
        let result = check_syntax("json", r#"{"key": value}"#);
        assert!(result.has_errors());
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn test_yaml_tabs() {
        let result = check_syntax("yaml", "\tkey: value");
        assert!(result.has_errors());
        assert!(result.description.contains("tabs"));
    }

    #[test]
    fn test_unsupported_extension() {
        let result = check_syntax("rs", "fn main() {}");
        assert!(!result.was_checked);
    }

    #[test]
    fn test_toml_valid() {
        let result = check_syntax("toml", "[package]\nname = \"test\"");
        assert!(!result.has_errors());
    }

    #[test]
    fn test_toml_unclosed_bracket() {
        let result = check_syntax("toml", "[package\nname = \"test\"");
        assert!(result.has_errors());
    }
}
