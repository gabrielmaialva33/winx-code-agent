//! Code analyzer tool for the Winx application.
//!
//! This module provides functionality for analyzing code, identifying
//! issues, and providing interactive debugging assistance.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::debug;

use crate::errors::WinxError;

/// Code analysis results including issues and suggestions
#[derive(Debug, Clone)]
pub struct CodeAnalysisResult {
    /// File path being analyzed
    pub file_path: PathBuf,
    /// Programming language detected
    pub language: String,
    /// List of issues found
    pub issues: Vec<CodeIssue>,
    /// List of improvement suggestions
    pub suggestions: Vec<CodeSuggestion>,
    /// Overall complexity metrics
    pub complexity: Option<ComplexityMetrics>,
    /// Time taken for analysis (in milliseconds)
    pub analysis_time_ms: u64,
}

/// A code issue identified during analysis
#[derive(Debug, Clone)]
pub struct CodeIssue {
    /// Type of issue (error, warning, etc.)
    pub issue_type: String,
    /// Description of the issue
    pub description: String,
    /// Line number where the issue occurs
    pub line: Option<usize>,
    /// Column where the issue starts
    pub column: Option<usize>,
    /// Severity level (high, medium, low)
    pub severity: String,
    /// Possible solutions to fix the issue
    pub solutions: Vec<String>,
}

/// A suggestion for code improvement
#[derive(Debug, Clone)]
pub struct CodeSuggestion {
    /// Type of suggestion (performance, style, etc.)
    pub suggestion_type: String,
    /// Description of the suggestion
    pub description: String,
    /// Line number where the suggestion applies
    pub line: Option<usize>,
    /// Confidence level (0.0-1.0)
    pub confidence: f64,
    /// The actual code suggestion
    pub code_sample: Option<String>,
}

/// Code complexity metrics
#[derive(Debug, Clone)]
pub struct ComplexityMetrics {
    /// Cyclomatic complexity
    pub cyclomatic_complexity: u32,
    /// Cognitive complexity
    pub cognitive_complexity: u32,
    /// Lines of code
    pub lines_of_code: u32,
    /// Number of functions/methods
    pub function_count: u32,
    /// Average function length
    pub avg_function_length: f64,
    /// Maximum nesting depth
    pub max_nesting_depth: u32,
}

/// Parameters for the code analysis request
#[derive(Debug, Clone)]
pub struct CodeAnalysisParams {
    /// Path to the file to analyze
    pub file_path: String,
    /// Language to use for analysis (auto-detect if not specified)
    pub language: Option<String>,
    /// Analysis depth (quick, normal, deep)
    pub analysis_depth: String,
    /// Whether to include complexity metrics
    pub include_complexity: bool,
    /// Whether to include suggestions
    pub include_suggestions: bool,
    /// Whether to show code snippets for issues
    pub show_code_snippets: bool,
    /// Whether to analyze imports and dependencies
    pub analyze_dependencies: bool,
    /// Chat ID for this session
    pub chat_id: String,
}

/// Handle the CodeAnalyzer tool call
///
/// This function processes a code analysis request, analyzing the specified
/// file for issues, suggestions, and complexity metrics.
///
/// # Arguments
///
/// * `bash_state` - The shared bash state
/// * `params` - The parameters for the code analysis
///
/// # Returns
///
/// Returns a Result containing a formatted string with the analysis results
///
/// # Errors
///
/// Returns an error if the analysis fails for any reason
pub async fn handle_tool_call(
    bash_state: &Arc<Mutex<Option<crate::state::bash_state::BashState>>>,
    params: CodeAnalysisParams,
) -> Result<String, WinxError> {
    debug!("CodeAnalyzer tool call with params: {:?}", params);

    // Get bash state guard
    let bash_state_guard = bash_state
        .lock()
        .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bash state: {}", e)))?;

    // Check if bash state is initialized
    let bash_state = bash_state_guard
        .as_ref()
        .ok_or(WinxError::BashStateNotInitialized)?;

    // Verify chat_id
    if !params.chat_id.is_empty() && params.chat_id != bash_state.current_chat_id {
        return Err(WinxError::ChatIdMismatch(format!(
            "Chat ID mismatch: expected {}, got {}",
            bash_state.current_chat_id, params.chat_id
        )));
    }

    // Resolve and validate file path
    let file_path = if Path::new(&params.file_path).is_absolute() {
        PathBuf::from(&params.file_path)
    } else {
        bash_state.cwd.join(&params.file_path)
    };

    if !file_path.exists() {
        return Err(WinxError::FileAccessError {
            path: file_path.clone(),
            message: "File does not exist".to_string(),
        });
    }

    if !file_path.is_file() {
        return Err(WinxError::FileAccessError {
            path: file_path.clone(),
            message: "Path exists but is not a file".to_string(),
        });
    }

    // Start timing the analysis
    let start_time = std::time::Instant::now();

    // Detect language if not provided
    let language = match &params.language {
        Some(lang) => lang.clone(),
        None => detect_language(&file_path)?,
    };

    // Read file content
    let file_content =
        std::fs::read_to_string(&file_path).map_err(|e| WinxError::FileAccessError {
            path: file_path.clone(),
            message: format!("Failed to read file: {}", e),
        })?;

    // Perform static analysis based on the language
    let (issues, suggestions) = analyze_code(&file_content, &language, &params)?;

    // Calculate complexity metrics if requested
    let complexity = if params.include_complexity {
        Some(calculate_complexity(&file_content, &language)?)
    } else {
        None
    };

    // Analyze dependencies if requested
    let dependency_info = if params.analyze_dependencies {
        analyze_dependencies(&file_path, &language)?
    } else {
        String::new()
    };

    // Calculate analysis time
    let analysis_time = start_time.elapsed();
    let analysis_time_ms = analysis_time.as_millis() as u64;

    // Create the analysis result
    let analysis_result = CodeAnalysisResult {
        file_path: file_path.clone(),
        language: language.clone(),
        issues,
        suggestions,
        complexity,
        analysis_time_ms,
    };

    // Format results
    let mut result = format_analysis_results(&analysis_result, &params)?;

    // Add dependency information if available
    if !dependency_info.is_empty() {
        result.push_str("\n\n## Dependencies\n\n");
        result.push_str(&dependency_info);
    }

    // Add footer
    result.push_str("\n\n---\n\n");
    result.push_str(&format!(
        "Analysis completed in {:.2}s",
        analysis_time.as_secs_f64()
    ));

    Ok(result)
}

/// Detect programming language from file extension and content
fn detect_language(file_path: &Path) -> Result<String, WinxError> {
    let extension = file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");

    // Map file extensions to languages
    let language = match extension {
        "rs" => "rust",
        "go" => "go",
        "js" => "javascript",
        "ts" => "typescript",
        "jsx" => "javascript",
        "tsx" => "typescript",
        "py" => "python",
        "rb" => "ruby",
        "php" => "php",
        "java" => "java",
        "scala" => "scala",
        "c" => "c",
        "cpp" | "cc" | "cxx" => "cpp",
        "h" | "hpp" => "cpp",
        "cs" => "csharp",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "hs" => "haskell",
        "ex" | "exs" => "elixir",
        "erl" => "erlang",
        "clj" | "cljs" => "clojure",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" | "sass" => "scss",
        "json" => "json",
        "yml" | "yaml" => "yaml",
        "md" => "markdown",
        "sh" | "bash" => "bash",
        "pl" => "perl",
        "r" => "r",
        "lua" => "lua",
        "dart" => "dart",
        "sql" => "sql",
        "xml" => "xml",
        "toml" => "toml",
        "ini" => "ini",
        "conf" => "config",
        _ => {
            // Try to detect from file content if extension not recognized
            let content = match std::fs::read_to_string(file_path) {
                Ok(content) => content,
                Err(_) => return Ok("unknown".to_string()),
            };

            // Simple heuristics for language detection from content
            if content.contains("fn ") && content.contains("pub ") && content.contains("->") {
                "rust"
            } else if content.contains("package main") || content.contains("import (") {
                "go"
            } else if content.contains("def ") && content.contains(":") {
                "python"
            } else if content.contains("function ") && content.contains("const ") {
                "javascript"
            } else if content.contains("public class ") || content.contains("private class ") {
                "java"
            } else if content.contains("#include") && content.contains("int main") {
                "cpp"
            } else if content.contains("<?php") {
                "php"
            } else if content.contains("#!/bin/bash") || content.contains("#!/bin/sh") {
                "bash"
            } else {
                "unknown"
            }
        }
    }
    .to_string();

    debug!("Detected language '{}' for file {:?}", language, file_path);
    Ok(language)
}

/// Analyze code for issues and suggestions
fn analyze_code(
    content: &str,
    language: &str,
    params: &CodeAnalysisParams,
) -> Result<(Vec<CodeIssue>, Vec<CodeSuggestion>), WinxError> {
    debug!(
        "Analyzing {} code with depth: {}",
        language, params.analysis_depth
    );

    let mut issues = Vec::new();
    let mut suggestions = Vec::new();

    // Process code based on language
    match language {
        "rust" => analyze_rust_code(content, &mut issues, &mut suggestions, params)?,
        "python" => analyze_python_code(content, &mut issues, &mut suggestions, params)?,
        "javascript" | "typescript" => {
            analyze_js_code(content, &mut issues, &mut suggestions, params)?
        }
        "go" => analyze_go_code(content, &mut issues, &mut suggestions, params)?,
        _ => {
            // Generic analysis for other languages
            generic_code_analysis(content, &mut issues, &mut suggestions, params)?;
        }
    }

    // Sort issues by severity and line number
    issues.sort_by(|a, b| {
        // First by severity (high to low)
        let severity_order = |s: &str| match s {
            "high" => 0,
            "medium" => 1,
            "low" => 2,
            _ => 3,
        };

        let a_order = severity_order(&a.severity);
        let b_order = severity_order(&b.severity);

        if a_order != b_order {
            return a_order.cmp(&b_order);
        }

        // Then by line number
        match (a.line, b.line) {
            (Some(a_line), Some(b_line)) => a_line.cmp(&b_line),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });

    // Sort suggestions by confidence (high to low)
    suggestions.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Limit the number of suggestions based on depth
    let max_suggestions = match params.analysis_depth.as_str() {
        "quick" => 3,
        "normal" => 5,
        "deep" => 10,
        _ => 5,
    };

    if suggestions.len() > max_suggestions {
        suggestions.truncate(max_suggestions);
    }

    Ok((issues, suggestions))
}

/// Rust-specific code analysis
fn analyze_rust_code(
    content: &str,
    issues: &mut Vec<CodeIssue>,
    suggestions: &mut Vec<CodeSuggestion>,
    params: &CodeAnalysisParams,
) -> Result<(), WinxError> {
    // Check for common Rust issues

    // 1. Missing error handling
    for (i, line) in content.lines().enumerate() {
        if (line.contains(".unwrap()") || line.contains(".expect("))
            && !line.contains("test")
            && !line.contains("#[test]")
        {
            issues.push(CodeIssue {
                issue_type: "error_handling".to_string(),
                description: "Potential unhandled error with unwrap() or expect()".to_string(),
                line: Some(i + 1),
                column: None,
                severity: "medium".to_string(),
                solutions: vec![
                    "Consider using proper error handling with ? operator".to_string(),
                    "Use match or if let to handle the Result/Option explicitly".to_string(),
                ],
            });
        }
    }

    // 2. Check for mutable references that could be immutable
    for (i, line) in content.lines().enumerate() {
        if line.contains("&mut ") && !line.contains("=") && !line.contains(".") {
            suggestions.push(CodeSuggestion {
                suggestion_type: "style".to_string(),
                description: "Consider using immutable reference if data isn't modified"
                    .to_string(),
                line: Some(i + 1),
                confidence: 0.7,
                code_sample: Some(line.replace("&mut ", "& ").to_string()),
            });
        }
    }

    // 3. Check for large functions
    let mut current_fn_start = 0;
    let mut in_function = false;
    let mut line_count = 0;

    for (i, line) in content.lines().enumerate() {
        if line.contains("fn ") && line.contains("(") && !line.contains(";") {
            if in_function {
                // Found a new function without closing the previous one
                if line_count > 50 {
                    issues.push(CodeIssue {
                        issue_type: "complexity".to_string(),
                        description: format!("Function is too long ({} lines)", line_count),
                        line: Some(current_fn_start + 1),
                        column: None,
                        severity: "low".to_string(),
                        solutions: vec![
                            "Consider breaking the function into smaller, more focused functions"
                                .to_string(),
                            "Extract helper methods for complex operations".to_string(),
                        ],
                    });
                }
            }

            in_function = true;
            current_fn_start = i;
            line_count = 0;
        }

        if in_function {
            line_count += 1;

            if line.contains("}") && line.trim() == "}" {
                in_function = false;

                if line_count > 50 {
                    issues.push(CodeIssue {
                        issue_type: "complexity".to_string(),
                        description: format!("Function is too long ({} lines)", line_count),
                        line: Some(current_fn_start + 1),
                        column: None,
                        severity: "low".to_string(),
                        solutions: vec![
                            "Consider breaking the function into smaller, more focused functions"
                                .to_string(),
                            "Extract helper methods for complex operations".to_string(),
                        ],
                    });
                }
            }
        }
    }

    // 4. Check for unsafe blocks
    let mut in_unsafe = false;
    let mut unsafe_start = 0;

    for (i, line) in content.lines().enumerate() {
        if line.contains("unsafe") && line.contains("{") {
            in_unsafe = true;
            unsafe_start = i;
        }

        if in_unsafe && line.contains("}") && line.trim() == "}" {
            in_unsafe = false;

            issues.push(CodeIssue {
                issue_type: "safety".to_string(),
                description: "Code contains unsafe block".to_string(),
                line: Some(unsafe_start + 1),
                column: None,
                severity: "high".to_string(),
                solutions: vec![
                    "Review unsafe code carefully for memory safety".to_string(),
                    "Consider if there's a safe alternative".to_string(),
                    "Add detailed comments explaining why unsafe is needed and how safety is maintained".to_string(),
                ],
            });
        }
    }

    // 5. Suggest usage of more idiomatic Rust
    if content.contains("for i in 0..") && content.contains("[i]") {
        suggestions.push(CodeSuggestion {
            suggestion_type: "idiomatic".to_string(),
            description: "Consider using iterator methods instead of indexing in loops".to_string(),
            line: None,
            confidence: 0.8,
            code_sample: Some("for item in items.iter() { ... }".to_string()),
        });
    }

    // Add more Rust-specific checks for deep analysis
    if params.analysis_depth == "deep" {
        // Check for potential memory leaks with Rc/Arc cycles
        if content.contains("Rc<") && content.contains("RefCell<") {
            suggestions.push(CodeSuggestion {
                suggestion_type: "safety".to_string(),
                description: "Potential for reference cycles with Rc and RefCell".to_string(),
                line: None,
                confidence: 0.6,
                code_sample: Some("Consider using Weak references to break cycles".to_string()),
            });
        }

        // Check for synchronization issues
        if content.contains("Arc<Mutex<") && content.contains(".lock()") {
            for (i, line) in content.lines().enumerate() {
                if line.contains(".lock()") && !line.contains("?") && !line.contains("unwrap_or") {
                    issues.push(CodeIssue {
                        issue_type: "concurrency".to_string(),
                        description: "Unhandled potential deadlock with lock()".to_string(),
                        line: Some(i + 1),
                        column: None,
                        severity: "medium".to_string(),
                        solutions: vec![
                            "Handle potential lock poisoning with ? or match".to_string(),
                            "Consider using a timeout for the lock acquisition".to_string(),
                        ],
                    });
                }
            }
        }
    }

    Ok(())
}

/// Python-specific code analysis
fn analyze_python_code(
    content: &str,
    issues: &mut Vec<CodeIssue>,
    suggestions: &mut Vec<CodeSuggestion>,
    _params: &CodeAnalysisParams,
) -> Result<(), WinxError> {
    // Check for common Python issues

    // 1. Bare except clauses
    for (i, line) in content.lines().enumerate() {
        if line.trim() == "except:" {
            issues.push(CodeIssue {
                issue_type: "error_handling".to_string(),
                description: "Bare except clause".to_string(),
                line: Some(i + 1),
                column: None,
                severity: "medium".to_string(),
                solutions: vec![
                    "Specify the exception types to catch".to_string(),
                    "Use 'except Exception:' as a last resort".to_string(),
                ],
            });
        }
    }

    // 2. Check for unused imports
    let mut imports = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if line.starts_with("import ") || line.starts_with("from ") {
            let import_name = if line.starts_with("import ") {
                line.strip_prefix("import ")
                    .unwrap()
                    .split(" as ")
                    .next()
                    .unwrap()
            } else {
                line.split(" import ")
                    .next()
                    .unwrap()
                    .strip_prefix("from ")
                    .unwrap()
            };

            imports.push((import_name.to_string(), i));
        }
    }

    for (import, line_num) in &imports {
        // Simple check - doesn't account for all usage patterns
        if !content.contains(&format!("{}..", import))
            && !content.contains(&format!(" {}", import))
            && !content.contains(&format!("({}", import))
        {
            suggestions.push(CodeSuggestion {
                suggestion_type: "style".to_string(),
                description: format!("Potentially unused import: {}", import),
                line: Some(line_num + 1),
                confidence: 0.5, // Low confidence since this is a simple check
                code_sample: None,
            });
        }
    }

    // 3. Check for mutable default arguments
    for (i, line) in content.lines().enumerate() {
        if line.contains("def ") && line.contains("=[]") || line.contains("={}") {
            issues.push(CodeIssue {
                issue_type: "bug_risk".to_string(),
                description: "Mutable default argument".to_string(),
                line: Some(i + 1),
                column: None,
                severity: "high".to_string(),
                solutions: vec![
                    "Use None as default and initialize the mutable value in the function body"
                        .to_string(),
                ],
            });
        }
    }

    // 4. Check for globals
    for (i, line) in content.lines().enumerate() {
        if line.trim().starts_with("global ") {
            suggestions.push(CodeSuggestion {
                suggestion_type: "design".to_string(),
                description: "Use of global variables can lead to maintainability issues"
                    .to_string(),
                line: Some(i + 1),
                confidence: 0.7,
                code_sample: Some(
                    "Consider using class attributes or function parameters instead".to_string(),
                ),
            });
        }
    }

    Ok(())
}

/// JavaScript/TypeScript-specific code analysis
fn analyze_js_code(
    content: &str,
    issues: &mut Vec<CodeIssue>,
    suggestions: &mut Vec<CodeSuggestion>,
    _params: &CodeAnalysisParams,
) -> Result<(), WinxError> {
    // Check for common JS/TS issues

    // 1. Check for == instead of ===
    for (i, line) in content.lines().enumerate() {
        if line.contains(" == ") && !line.contains("===") && !line.contains("!==") {
            issues.push(CodeIssue {
                issue_type: "bug_risk".to_string(),
                description: "Using == instead of === may lead to unexpected type coercion"
                    .to_string(),
                line: Some(i + 1),
                column: None,
                severity: "medium".to_string(),
                solutions: vec!["Use === for strict equality comparison".to_string()],
            });
        }
    }

    // 2. Check for potential variable hoisting issues
    for (i, line) in content.lines().enumerate() {
        if line.trim().starts_with("var ") {
            suggestions.push(CodeSuggestion {
                suggestion_type: "modernize".to_string(),
                description: "Use let or const instead of var to avoid hoisting issues".to_string(),
                line: Some(i + 1),
                confidence: 0.9,
                code_sample: Some(line.replace("var ", "const ").to_string()),
            });
        }
    }

    // 3. Check for console.log left in code
    for (i, line) in content.lines().enumerate() {
        if line.contains("console.log(") {
            issues.push(CodeIssue {
                issue_type: "debug_code".to_string(),
                description: "Debug console.log statement found".to_string(),
                line: Some(i + 1),
                column: None,
                severity: "low".to_string(),
                solutions: vec![
                    "Remove console.log before production deployment".to_string(),
                    "Replace with proper logging system".to_string(),
                ],
            });
        }
    }

    // 4. Check for potential memory leaks in React components
    if content.contains("React") && content.contains("Component") || content.contains("useState") {
        for (i, line) in content.lines().enumerate() {
            if line.contains("addEventListener") && !content.contains("removeEventListener") {
                issues.push(CodeIssue {
                    issue_type: "memory_leak".to_string(),
                    description: "Event listener added without cleanup".to_string(),
                    line: Some(i + 1),
                    column: None,
                    severity: "medium".to_string(),
                    solutions: vec![
                        "Remove event listeners in useEffect cleanup or componentWillUnmount"
                            .to_string(),
                    ],
                });
                break;
            }
        }
    }

    Ok(())
}

/// Go-specific code analysis
fn analyze_go_code(
    content: &str,
    issues: &mut Vec<CodeIssue>,
    suggestions: &mut Vec<CodeSuggestion>,
    _params: &CodeAnalysisParams,
) -> Result<(), WinxError> {
    // 1. Check for error handling issues
    for (i, line) in content.lines().enumerate() {
        if line.contains("if err != nil {")
            && (line.contains("return") || line.contains("os.Exit"))
            && !line.contains("return")
            && !line.contains("fmt.")
            && !line.contains("log.")
        {
            issues.push(CodeIssue {
                issue_type: "error_handling".to_string(),
                description: "Error returned without logging or wrapping".to_string(),
                line: Some(i + 1),
                column: None,
                severity: "medium".to_string(),
                solutions: vec![
                    "Log the error or wrap it with context before returning".to_string(),
                    "Consider using fmt.Errorf or errors.Wrap".to_string(),
                ],
            });
        }
    }

    // 2. Check for unused error values
    for (i, line) in content.lines().enumerate() {
        if line.contains("= ")
            && line.contains("(")
            && line.contains(")")
            && !line.contains("err")
            && line.contains("_")
            && line.contains("err")
        {
            issues.push(CodeIssue {
                issue_type: "error_handling".to_string(),
                description: "Error value ignored with _".to_string(),
                line: Some(i + 1),
                column: None,
                severity: "high".to_string(),
                solutions: vec![
                    "Handle the error value properly".to_string(),
                    "If ignoring is intentional, add a comment explaining why".to_string(),
                ],
            });
        }
    }

    // 3. Check for inefficient string concatenation
    let mut has_string_builder = false;
    let mut has_concatenation = false;

    for line in content.lines() {
        if line.contains("strings.Builder") {
            has_string_builder = true;
        }

        if line.contains(" += ") && !line.contains("int") && !line.contains("float") {
            has_concatenation = true;
        }
    }

    if has_concatenation && !has_string_builder {
        suggestions.push(CodeSuggestion {
            suggestion_type: "performance".to_string(),
            description: "Consider using strings.Builder for string concatenation".to_string(),
            line: None,
            confidence: 0.7,
            code_sample: Some("var sb strings.Builder\nsb.WriteString(...)".to_string()),
        });
    }

    // 4. Check for mutex usage
    let has_mutex = content.contains("sync.Mutex");
    let has_defer = content.contains("defer");

    if has_mutex && !has_defer {
        issues.push(CodeIssue {
            issue_type: "concurrency".to_string(),
            description: "Mutex used without defer for unlocking".to_string(),
            line: None,
            column: None,
            severity: "high".to_string(),
            solutions: vec![
                "Use defer mu.Unlock() immediately after Lock() to prevent deadlocks".to_string(),
            ],
        });
    }

    Ok(())
}

/// Generic code analysis for languages without specific analyzers
fn generic_code_analysis(
    content: &str,
    issues: &mut Vec<CodeIssue>,
    suggestions: &mut Vec<CodeSuggestion>,
    _params: &CodeAnalysisParams,
) -> Result<(), WinxError> {
    // 1. Check for TODOs and FIXMEs
    for (i, line) in content.lines().enumerate() {
        if line.contains("TODO") || line.contains("FIXME") {
            issues.push(CodeIssue {
                issue_type: "maintenance".to_string(),
                description: "Contains TODO or FIXME comment".to_string(),
                line: Some(i + 1),
                column: None,
                severity: "low".to_string(),
                solutions: vec![
                    "Address the TODO/FIXME item".to_string(),
                    "Create a ticket for tracking if it can't be fixed immediately".to_string(),
                ],
            });
        }
    }

    // 2. Check for very long lines
    for (i, line) in content.lines().enumerate() {
        if line.len() > 100 {
            suggestions.push(CodeSuggestion {
                suggestion_type: "style".to_string(),
                description: format!("Line too long ({} characters)", line.len()),
                line: Some(i + 1),
                confidence: 0.8,
                code_sample: None,
            });
        }
    }

    // 3. Check for very large functions
    let lines = content.lines().collect::<Vec<&str>>();
    let mut in_function = false;
    let mut function_start = 0;
    let mut function_lines = 0;
    let mut brace_count = 0;

    for (i, line) in lines.iter().enumerate() {
        // Very basic function detection
        if !in_function
            && (line.contains("function ")
                || line.contains("def ")
                || line.contains("sub ")
                || line.contains("func ")
                || (line.contains("(")
                    && line.contains(")")
                    && line.contains("{")
                    && !line.contains(";")))
        {
            in_function = true;
            function_start = i;
            function_lines = 1;

            // Count opening braces
            brace_count = line.chars().filter(|c| *c == '{').count() as i32
                - line.chars().filter(|c| *c == '}').count() as i32;

            continue;
        }

        if in_function {
            function_lines += 1;

            // Update brace count for languages with braces
            if line.contains("{") || line.contains("}") {
                brace_count += line.chars().filter(|c| *c == '{').count() as i32
                    - line.chars().filter(|c| *c == '}').count() as i32;
            }

            // Check if function has ended
            let function_ended =
                // Python-style function end (dedent)
                (line.trim().is_empty() && i + 1 < lines.len() && !lines[i + 1].starts_with("    ")) ||
                // Brace-style function end
                (brace_count == 0 && line.contains("}") && line.trim() == "}") ||
                // End keyword
                line.trim() == "end";

            if function_ended {
                in_function = false;

                if function_lines > 50 {
                    issues.push(CodeIssue {
                        issue_type: "complexity".to_string(),
                        description: format!("Function is too long ({} lines)", function_lines),
                        line: Some(function_start + 1),
                        column: None,
                        severity: "medium".to_string(),
                        solutions: vec![
                            "Break down into smaller functions".to_string(),
                            "Extract helper methods for complex operations".to_string(),
                        ],
                    });
                }
            }
        }
    }

    // 4. Check for potential hardcoded credentials
    for (i, line) in content.lines().enumerate() {
        let lower_line = line.to_lowercase();
        if (lower_line.contains("password")
            || lower_line.contains("secret")
            || lower_line.contains("api_key")
            || lower_line.contains("apikey")
            || lower_line.contains("token"))
            && (line.contains("\"") || line.contains("'"))
        {
            issues.push(CodeIssue {
                issue_type: "security".to_string(),
                description: "Possible hardcoded credentials".to_string(),
                line: Some(i + 1),
                column: None,
                severity: "high".to_string(),
                solutions: vec![
                    "Move sensitive data to environment variables".to_string(),
                    "Use a secure credential store or vault".to_string(),
                    "Replace with configuration that's loaded at runtime".to_string(),
                ],
            });
        }
    }

    // 5. Check for commented-out code
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if (trimmed.starts_with("//") || trimmed.starts_with("#") || trimmed.starts_with("--"))
            && (trimmed.contains("if ")
                || trimmed.contains("for ")
                || trimmed.contains("while ")
                || trimmed.contains("function")
                || trimmed.contains(" = "))
        {
            suggestions.push(CodeSuggestion {
                suggestion_type: "maintenance".to_string(),
                description: "Commented-out code".to_string(),
                line: Some(i + 1),
                confidence: 0.6,
                code_sample: None,
            });
        }
    }

    Ok(())
}

/// Calculate code complexity metrics
fn calculate_complexity(content: &str, language: &str) -> Result<ComplexityMetrics, WinxError> {
    // This is a basic implementation of complexity calculation

    // Count total lines
    let total_lines = content.lines().count() as u32;

    // Count functions
    let function_count = match language {
        "rust" => content.matches("fn ").count() as u32,
        "python" => content.matches("def ").count() as u32,
        "javascript" | "typescript" => {
            content.matches("function ").count() as u32 + 
            // Count arrow functions
            content.matches(" => {").count() as u32
        }
        "go" => content.matches("func ").count() as u32,
        "java" | "kotlin" => {
            // Match method definitions
            let method_regex =
                regex::Regex::new(r"(public|private|protected)?\s+\w+\s+\w+\s*\(").unwrap();
            method_regex.find_iter(content).count() as u32
        }
        _ => {
            // Generic function detection
            let mut count = 0;
            count += content.matches("function ").count();
            count += content.matches("def ").count();
            count += content.matches("func ").count();
            count += content.matches("sub ").count();
            count as u32
        }
    };

    // Average function length
    let avg_function_length = if function_count > 0 {
        total_lines as f64 / function_count as f64
    } else {
        total_lines as f64
    };

    // Calculate cyclomatic complexity based on control flow statements
    let mut cyclomatic_complexity = 1; // Base complexity

    // Count control flow branching
    cyclomatic_complexity += content.matches("if ").count();
    cyclomatic_complexity += content.matches("else ").count();
    cyclomatic_complexity += content.matches("else if ").count();
    cyclomatic_complexity += content.matches("elif ").count();
    cyclomatic_complexity += content.matches("case ").count();
    cyclomatic_complexity += content.matches("for ").count();
    cyclomatic_complexity += content.matches("while ").count();
    cyclomatic_complexity += content.matches("do ").count();
    cyclomatic_complexity += content.matches("foreach").count();
    cyclomatic_complexity += content.matches("catch ").count();
    cyclomatic_complexity += content.matches("&&").count();
    cyclomatic_complexity += content.matches("||").count();

    // Cognitive complexity (more focused on nesting and structural complexity)
    // This is a simple approximation of cognitive complexity
    let mut cognitive_complexity = 0;
    let mut current_nesting = 0;
    let mut max_nesting = 0;

    for line in content.lines() {
        // Increase nesting level for control structures with braces or colons
        if line.contains("{")
            && (line.contains("if ")
                || line.contains("for ")
                || line.contains("while ")
                || line.contains("switch ")
                || line.contains("case "))
        {
            current_nesting += 1;
            cognitive_complexity += current_nesting; // Higher weight for deeper nesting
        }
        // Python-style blocks
        else if line.contains(":")
            && (line.contains("if ")
                || line.contains("for ")
                || line.contains("while ")
                || line.contains("def "))
        {
            current_nesting += 1;
            cognitive_complexity += current_nesting;
        }
        // Count closing braces to track nesting level
        else if line.trim() == "}" && current_nesting > 0 {
            current_nesting -= 1;
        }

        // Update max nesting depth
        if current_nesting > max_nesting {
            max_nesting = current_nesting;
        }

        // Additional cognitive complexity for boolean logic
        if line.contains("&&") || line.contains("||") {
            cognitive_complexity += 1;
        }
    }

    // Create and return the complexity metrics
    Ok(ComplexityMetrics {
        cyclomatic_complexity: cyclomatic_complexity as u32,
        cognitive_complexity: cognitive_complexity as u32,
        lines_of_code: total_lines,
        function_count,
        avg_function_length,
        max_nesting_depth: max_nesting as u32,
    })
}

/// Analyze dependencies in the code
fn analyze_dependencies(file_path: &Path, language: &str) -> Result<String, WinxError> {
    let mut dependencies = Vec::new();

    // Get parent directory for project files
    let parent_dir = file_path.parent().unwrap_or(Path::new("."));

    match language {
        "rust" => {
            // Check for Cargo.toml
            let cargo_path = find_file_in_ancestors(parent_dir, "Cargo.toml")?;
            if let Some(cargo_path) = cargo_path {
                let content = std::fs::read_to_string(cargo_path)?;
                dependencies.push("**Rust dependencies (Cargo.toml):**".to_string());

                // Extract dependencies section
                if let Some(deps_section) = content.split("[dependencies]").nth(1) {
                    if let Some(end_section) = deps_section.find('[') {
                        let deps = &deps_section[..end_section];

                        for line in deps.lines() {
                            let line = line.trim();
                            if !line.is_empty() && line.contains('=') {
                                dependencies.push(format!("- {}", line));
                            }
                        }
                    } else {
                        // No end section found, just use the whole section
                        for line in deps_section.lines() {
                            let line = line.trim();
                            if !line.is_empty() && line.contains('=') {
                                dependencies.push(format!("- {}", line));
                            }
                        }
                    }
                }
            }
        }
        "javascript" | "typescript" => {
            // Check for package.json
            let pkg_path = find_file_in_ancestors(parent_dir, "package.json")?;
            if let Some(pkg_path) = pkg_path {
                let content = std::fs::read_to_string(pkg_path)?;
                dependencies
                    .push("**JavaScript/TypeScript dependencies (package.json):**".to_string());

                // Very basic JSON parsing
                if let Some(deps_start) = content.find("\"dependencies\"") {
                    if let Some(deps_content) = content[deps_start..].find('{') {
                        let start_idx = deps_start + deps_content;
                        let mut brace_count = 1;
                        let mut end_idx = start_idx + 1;

                        // Find matching closing brace
                        for (i, c) in content[start_idx + 1..].char_indices() {
                            if c == '{' {
                                brace_count += 1;
                            } else if c == '}' {
                                brace_count -= 1;
                                if brace_count == 0 {
                                    end_idx = start_idx + 1 + i;
                                    break;
                                }
                            }
                        }

                        let deps_section = &content[start_idx..=end_idx];
                        for line in deps_section.lines() {
                            let line = line.trim();
                            if line.contains('"') && line.contains(':') {
                                dependencies.push(format!("- {}", line));
                            }
                        }
                    }
                }
            }
        }
        "python" => {
            // Check for requirements.txt or setup.py
            let req_path = find_file_in_ancestors(parent_dir, "requirements.txt")?;
            if let Some(req_path) = req_path {
                let content = std::fs::read_to_string(req_path)?;
                dependencies.push("**Python dependencies (requirements.txt):**".to_string());

                for line in content.lines() {
                    let line = line.trim();
                    if !line.is_empty() && !line.starts_with('#') {
                        dependencies.push(format!("- {}", line));
                    }
                }
            }

            // Also check for imports in the file itself
            let content = std::fs::read_to_string(file_path)?;

            dependencies.push("\n**Direct imports in this file:**".to_string());
            for line in content.lines() {
                if line.starts_with("import ") || line.starts_with("from ") {
                    dependencies.push(format!("- {}", line.trim()));
                }
            }
        }
        _ => {
            // Generic dependency analysis based on file contents
            let content = std::fs::read_to_string(file_path)?;

            dependencies.push("**Referenced libraries/modules:**".to_string());

            // Look for common import patterns
            let mut imports = Vec::new();

            for line in content.lines() {
                let line = line.trim();

                // Various import syntax forms
                if line.starts_with("import ")
                    || line.starts_with("from ")
                    || line.starts_with("require ")
                    || line.starts_with("#include ")
                    || line.starts_with("using ")
                {
                    imports.push(line);
                }
            }

            if imports.is_empty() {
                dependencies.push("- No imports detected".to_string());
            } else {
                for import in imports {
                    dependencies.push(format!("- {}", import));
                }
            }
        }
    }

    // Format the dependencies as a string
    let result = if dependencies.is_empty() {
        "No dependency information available.".to_string()
    } else {
        dependencies.join("\n")
    };

    Ok(result)
}

/// Find a file in ancestor directories
fn find_file_in_ancestors(start_dir: &Path, filename: &str) -> Result<Option<PathBuf>, WinxError> {
    let mut current_dir = Some(start_dir.to_path_buf());

    while let Some(dir) = current_dir {
        let file_path = dir.join(filename);
        if file_path.exists() {
            return Ok(Some(file_path));
        }

        // Move up to parent directory
        current_dir = dir.parent().map(|p| p.to_path_buf());
    }

    Ok(None)
}

/// Format analysis results as a human-readable string
fn format_analysis_results(
    result: &CodeAnalysisResult,
    params: &CodeAnalysisParams,
) -> Result<String, WinxError> {
    let mut output = String::new();

    // Header
    output.push_str(&format!(
        "# Code Analysis: {}\n\n",
        result.file_path.display()
    ));
    output.push_str(&format!("Language detected: **{}**\n\n", result.language));

    // Issues section
    if !result.issues.is_empty() {
        output.push_str("## Issues Found\n\n");

        let mut by_severity: HashMap<&str, Vec<&CodeIssue>> = HashMap::new();

        for issue in &result.issues {
            by_severity.entry(&issue.severity).or_default().push(issue);
        }

        // Sort by severity
        let severities = ["high", "medium", "low"];

        for &severity in &severities {
            if let Some(issues) = by_severity.get(severity) {
                output.push_str(&format!("### {} Priority\n\n", severity.to_uppercase()));

                for issue in issues {
                    output.push_str(&format!("- **{}**", issue.description));

                    if let Some(line) = issue.line {
                        output.push_str(&format!(" (line {})", line));
                    }

                    output.push('\n');

                    // Include solutions
                    if !issue.solutions.is_empty() {
                        output.push_str("  - Solutions:\n");
                        for solution in &issue.solutions {
                            output.push_str(&format!("    - {}\n", solution));
                        }
                    }
                }

                output.push('\n');
            }
        }
    } else {
        output.push_str("## Issues Found\n\nNo issues detected.\n\n");
    }

    // Suggestions section
    if params.include_suggestions && !result.suggestions.is_empty() {
        output.push_str("## Improvement Suggestions\n\n");

        for suggestion in &result.suggestions {
            output.push_str(&format!("- **{}**", suggestion.description));

            if let Some(line) = suggestion.line {
                output.push_str(&format!(" (line {})", line));
            }

            output.push_str(&format!(
                " [confidence: {:.0}%]",
                suggestion.confidence * 100.0
            ));
            output.push('\n');

            if let Some(code) = &suggestion.code_sample {
                output.push_str(&format!("  ```\n  {}\n  ```\n", code));
            }
        }

        output.push('\n');
    }

    // Complexity metrics section
    if params.include_complexity {
        if let Some(complexity) = &result.complexity {
            output.push_str("## Complexity Metrics\n\n");
            output.push_str(&format!(
                "- **Cyclomatic Complexity**: {}\n",
                complexity.cyclomatic_complexity
            ));
            output.push_str(&format!(
                "- **Cognitive Complexity**: {}\n",
                complexity.cognitive_complexity
            ));
            output.push_str(&format!(
                "- **Lines of Code**: {}\n",
                complexity.lines_of_code
            ));
            output.push_str(&format!(
                "- **Function Count**: {}\n",
                complexity.function_count
            ));
            output.push_str(&format!(
                "- **Average Function Length**: {:.1} lines\n",
                complexity.avg_function_length
            ));
            output.push_str(&format!(
                "- **Maximum Nesting Depth**: {}\n",
                complexity.max_nesting_depth
            ));

            // Add complexity interpretation
            output.push_str("\n### Complexity Interpretation\n\n");

            let cyclomatic_interpretation = match complexity.cyclomatic_complexity {
                0..=10 => "Simple code, easy to maintain",
                11..=20 => "Moderately complex, consider refactoring longer functions",
                21..=50 => "Complex code, high risk of bugs, should be refactored",
                _ => "Extremely complex, high risk, needs immediate refactoring",
            };

            let cognitive_interpretation = match complexity.cognitive_complexity {
                0..=15 => "Easy to understand",
                16..=30 => "Moderately difficult to understand",
                31..=60 => "Difficult to understand, consider refactoring",
                _ => "Very difficult to understand, refactoring strongly recommended",
            };

            output.push_str(&format!(
                "- **Cyclomatic Complexity**: {}\n",
                cyclomatic_interpretation
            ));
            output.push_str(&format!(
                "- **Cognitive Complexity**: {}\n",
                cognitive_interpretation
            ));

            output.push('\n');
        }
    }

    Ok(output)
}
