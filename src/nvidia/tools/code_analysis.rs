//! NVIDIA-powered code analysis tool

use crate::errors::{Result, WinxError};
use crate::nvidia::NvidiaClient;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::{debug, info};

/// Parameters for AI-powered code analysis
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AnalyzeCodeParams {
    /// Path to the file to analyze (optional if code is provided)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    /// Code content to analyze (optional if file_path is provided)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Programming language (auto-detected if not provided)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Include suggestions for improvements
    #[serde(default = "default_true")]
    pub include_suggestions: bool,
    /// Include complexity analysis
    #[serde(default = "default_true")]
    pub include_complexity: bool,
    /// Minimum severity level to include (Info, Warning, Error, Critical)
    #[serde(default = "default_warning")]
    pub min_severity: String,
}

fn default_true() -> bool {
    true
}

fn default_warning() -> String {
    "Warning".to_string()
}

/// Result of AI-powered code analysis
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AnalyzeCodeResult {
    /// Summary of the analysis
    pub summary: String,
    /// File that was analyzed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    /// Detected programming language
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Issues found in the code
    pub issues: Vec<CodeIssueReport>,
    /// General suggestions for improvement
    pub suggestions: Vec<String>,
    /// Complexity score (0-100, where higher is more complex)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub complexity_score: Option<u32>,
    /// AI model used for analysis
    pub model_used: String,
}

/// Detailed code issue report
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CodeIssueReport {
    /// Severity level of the issue
    pub severity: String,
    /// Category of the issue
    pub category: String,
    /// Human-readable description
    pub message: String,
    /// Line number where the issue occurs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Suggested fix for the issue
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

/// AI-powered code analysis tool
pub async fn analyze_code_with_ai(
    nvidia_client: &NvidiaClient,
    params: AnalyzeCodeParams,
) -> Result<AnalyzeCodeResult> {
    info!("Starting AI-powered code analysis");

    // Get code content
    let (code_content, file_path, language) = get_code_and_metadata(&params).await?;

    debug!(
        "Analyzing {} characters of {} code",
        code_content.len(),
        language.as_deref().unwrap_or("unknown")
    );

    // Perform AI analysis
    let analysis_result = nvidia_client
        .analyze_code(&code_content, language.as_deref())
        .await?;

    // Convert to our result format
    let issues = analysis_result
        .issues
        .into_iter()
        .filter(|issue| should_include_issue(&issue.severity.to_string(), &params.min_severity))
        .map(|issue| CodeIssueReport {
            severity: format!("{:?}", issue.severity),
            category: format!("{:?}", issue.category),
            message: issue.message,
            line: issue.line,
            suggestion: issue.suggestion,
        })
        .collect();

    let result = AnalyzeCodeResult {
        summary: analysis_result.summary,
        file_path,
        language,
        issues,
        suggestions: if params.include_suggestions {
            analysis_result.suggestions
        } else {
            vec![]
        },
        complexity_score: if params.include_complexity {
            analysis_result.complexity_score
        } else {
            None
        },
        model_used: nvidia_client
            .recommend_model(crate::nvidia::models::TaskType::CodeAnalysis)
            .as_str()
            .to_string(),
    };

    info!(
        "Code analysis completed. Found {} issues",
        result.issues.len()
    );
    Ok(result)
}

/// Get code content and metadata from parameters
async fn get_code_and_metadata(
    params: &AnalyzeCodeParams,
) -> Result<(String, Option<String>, Option<String>)> {
    match (&params.file_path, &params.code) {
        (Some(file_path), _) => {
            // Read from file
            let code = tokio::fs::read_to_string(file_path).await.map_err(|e| {
                WinxError::FileError(format!("Failed to read file {}: {}", file_path, e))
            })?;

            let language = params
                .language
                .clone()
                .or_else(|| detect_language_from_path(file_path));

            Ok((code, Some(file_path.clone()), language))
        }
        (None, Some(code)) => {
            // Use provided code
            Ok((code.clone(), None, params.language.clone()))
        }
        (None, None) => Err(WinxError::InvalidInput(
            "Either file_path or code must be provided".to_string(),
        )),
    }
}

/// Detect programming language from file extension
fn detect_language_from_path(file_path: &str) -> Option<String> {
    let path = Path::new(file_path);
    let extension = path.extension()?.to_str()?.to_lowercase();

    match extension.as_str() {
        "rs" => Some("Rust".to_string()),
        "py" => Some("Python".to_string()),
        "js" | "mjs" => Some("JavaScript".to_string()),
        "ts" => Some("TypeScript".to_string()),
        "go" => Some("Go".to_string()),
        "java" => Some("Java".to_string()),
        "cpp" | "cc" | "cxx" => Some("C++".to_string()),
        "c" => Some("C".to_string()),
        "cs" => Some("C#".to_string()),
        "php" => Some("PHP".to_string()),
        "rb" => Some("Ruby".to_string()),
        "swift" => Some("Swift".to_string()),
        "kt" => Some("Kotlin".to_string()),
        "scala" => Some("Scala".to_string()),
        "clj" => Some("Clojure".to_string()),
        "hs" => Some("Haskell".to_string()),
        "ml" => Some("OCaml".to_string()),
        "ex" | "exs" => Some("Elixir".to_string()),
        "erl" => Some("Erlang".to_string()),
        "dart" => Some("Dart".to_string()),
        "lua" => Some("Lua".to_string()),
        "r" => Some("R".to_string()),
        "m" => Some("MATLAB".to_string()),
        "sql" => Some("SQL".to_string()),
        "sh" | "bash" => Some("Shell".to_string()),
        "ps1" => Some("PowerShell".to_string()),
        "dockerfile" => Some("Dockerfile".to_string()),
        "yaml" | "yml" => Some("YAML".to_string()),
        "json" => Some("JSON".to_string()),
        "xml" => Some("XML".to_string()),
        "html" => Some("HTML".to_string()),
        "css" => Some("CSS".to_string()),
        "scss" | "sass" => Some("SCSS".to_string()),
        _ => None,
    }
}

/// Check if an issue should be included based on severity level
fn should_include_issue(issue_severity: &str, min_severity: &str) -> bool {
    let severity_order = ["Info", "Warning", "Error", "Critical"];

    let issue_level = severity_order
        .iter()
        .position(|&s| s == issue_severity)
        .unwrap_or(0);
    let min_level = severity_order
        .iter()
        .position(|&s| s == min_severity)
        .unwrap_or(1);

    issue_level >= min_level
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language_from_path() {
        assert_eq!(
            detect_language_from_path("main.rs"),
            Some("Rust".to_string())
        );
        assert_eq!(
            detect_language_from_path("script.py"),
            Some("Python".to_string())
        );
        assert_eq!(
            detect_language_from_path("app.js"),
            Some("JavaScript".to_string())
        );
        assert_eq!(detect_language_from_path("component.tsx"), None); // TypeScript JSX not in our list
        assert_eq!(detect_language_from_path("noext"), None);
    }

    #[test]
    fn test_should_include_issue() {
        assert_eq!(should_include_issue("Critical", "Warning"), true);
        assert_eq!(should_include_issue("Error", "Warning"), true);
        assert_eq!(should_include_issue("Warning", "Warning"), true);
        assert_eq!(should_include_issue("Info", "Warning"), false);
        assert_eq!(should_include_issue("Critical", "Critical"), true);
        assert_eq!(should_include_issue("Error", "Critical"), false);
    }
}
