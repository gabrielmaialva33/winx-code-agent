//! File extension management and token limits
//!
//! This module provides file type detection and context length management
//! based on file extensions, directly ported from WCGW's extensions.py.
//! It supports intelligent token allocation for source code vs non-source files.

use std::collections::HashSet;
use std::path::Path;

/// Set of file extensions considered to be source code
/// Each extension is listed without the dot (e.g., 'rs' not '.rs')
const SOURCE_CODE_EXTENSIONS: &[&str] = &[
    // Rust
    "rs",
    "rlib",
    // Python
    "py",
    "pyx",
    "pyi",
    "pyw",
    // JavaScript and TypeScript
    "js",
    "jsx",
    "ts",
    "tsx",
    "mjs",
    "cjs",
    // Web
    "html",
    "htm",
    "xhtml",
    "css",
    "scss",
    "sass",
    "less",
    // C and C++
    "c",
    "h",
    "cpp",
    "cxx",
    "cc",
    "hpp",
    "hxx",
    "hh",
    "inl",
    // C#
    "cs",
    "csx",
    // Java
    "java",
    "scala",
    "kt",
    "kts",
    "groovy",
    // Go
    "go",
    "mod",
    // Swift
    "swift",
    // Ruby
    "rb",
    "rake",
    "gemspec",
    // PHP
    "php",
    "phtml",
    "phar",
    "phps",
    // Shell
    "sh",
    "bash",
    "zsh",
    "fish",
    // PowerShell
    "ps1",
    "psm1",
    "psd1",
    // SQL
    "sql",
    "ddl",
    "dml",
    // Markup and config
    "xml",
    "json",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    // Documentation
    "md",
    "markdown",
    "rst",
    "adoc",
    "tex",
    // Build and dependency files
    "Makefile",
    "Dockerfile",
    "Jenkinsfile",
    // Haskell
    "hs",
    "lhs",
    // Lisp family
    "lisp",
    "cl",
    "el",
    "clj",
    "cljs",
    "edn",
    "scm",
    // Erlang and Elixir
    "erl",
    "hrl",
    "ex",
    "exs",
    // Dart and Flutter
    "dart",
    // Objective-C
    "m",
    "mm",
];

/// Context length limits based on file type (in tokens)
#[derive(Debug, Clone)]
pub struct ContextLengthLimits {
    /// Token limit for known source code files
    pub source_code: usize,
    /// Token limit for all other files
    pub default: usize,
}

impl Default for ContextLengthLimits {
    fn default() -> Self {
        Self { source_code: 24000, default: 8000 }
    }
}

/// File extension analyzer for token management
#[derive(Debug, Clone)]
pub struct FileExtensionAnalyzer {
    source_extensions: HashSet<String>,
    context_limits: ContextLengthLimits,
}

impl Default for FileExtensionAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl FileExtensionAnalyzer {
    /// Create a new file extension analyzer with default settings
    pub fn new() -> Self {
        let source_extensions =
            SOURCE_CODE_EXTENSIONS.iter().map(|ext| ext.to_lowercase()).collect();

        Self { source_extensions, context_limits: ContextLengthLimits::default() }
    }

    /// Create a new analyzer with custom context limits
    pub fn with_limits(context_limits: ContextLengthLimits) -> Self {
        let mut analyzer = Self::new();
        analyzer.context_limits = context_limits;
        analyzer
    }

    /// Determine if a file is a source code file based on its extension
    ///
    /// # Arguments
    /// * `filename` - The name of the file to check
    ///
    /// # Returns
    /// True if the file has a recognized source code extension, False otherwise
    pub fn is_source_code_file(&self, filename: &str) -> bool {
        // Extract extension (without the dot)
        if let Some(ext) = Path::new(filename).extension().and_then(|ext| ext.to_str()) {
            return self.source_extensions.contains(&ext.to_lowercase());
        }

        // Files without extensions (like 'Makefile', 'Dockerfile')
        // Case-insensitive match for files without extensions
        self.source_extensions.contains(&filename.to_lowercase())
    }

    /// Get the appropriate context length limit for a file based on its extension
    ///
    /// # Arguments
    /// * `filename` - The name of the file to check
    ///
    /// # Returns
    /// The context length limit in tokens
    pub fn get_context_length_for_file(&self, filename: &str) -> usize {
        if self.is_source_code_file(filename) {
            self.context_limits.source_code
        } else {
            self.context_limits.default
        }
    }

    /// Select the appropriate `max_tokens` limit based on file type
    ///
    /// # Arguments
    /// * `filename` - The name of the file to check
    /// * `coding_max_tokens` - Maximum tokens for source code files
    /// * `noncoding_max_tokens` - Maximum tokens for non-source code files
    ///
    /// # Returns
    /// The appropriate `max_tokens` limit for the file
    pub fn select_max_tokens(
        &self,
        filename: &str,
        coding_max_tokens: Option<usize>,
        noncoding_max_tokens: Option<usize>,
    ) -> Option<usize> {
        if coding_max_tokens.is_none() && noncoding_max_tokens.is_none() {
            return None;
        }

        if self.is_source_code_file(filename) {
            coding_max_tokens
        } else {
            noncoding_max_tokens
        }
    }

    /// Get file type category for statistics and reporting
    ///
    /// # Arguments
    /// * `filename` - The name of the file to analyze
    ///
    /// # Returns
    /// A string describing the file type category
    pub fn get_file_type_category(&self, filename: &str) -> String {
        if let Some(ext) = Path::new(filename).extension().and_then(|ext| ext.to_str()) {
            let ext_lower = ext.to_lowercase();

            // Categorize by major language families
            match ext_lower.as_str() {
                "rs" | "rlib" => "Rust".to_string(),
                "py" | "pyx" | "pyi" | "pyw" => "Python".to_string(),
                "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" => "JavaScript/TypeScript".to_string(),
                "html" | "htm" | "xhtml" | "css" | "scss" | "sass" | "less" => "Web".to_string(),
                "c" | "h" | "cpp" | "cxx" | "cc" | "hpp" | "hxx" | "hh" | "inl" => {
                    "C/C++".to_string()
                }
                "cs" | "csx" => "C#".to_string(),
                "java" | "scala" | "kt" | "kts" | "groovy" => "JVM Languages".to_string(),
                "go" | "mod" => "Go".to_string(),
                "swift" => "Swift".to_string(),
                "rb" | "rake" | "gemspec" => "Ruby".to_string(),
                "php" | "phtml" | "phar" | "phps" => "PHP".to_string(),
                "sh" | "bash" | "zsh" | "fish" => "Shell".to_string(),
                "ps1" | "psm1" | "psd1" => "PowerShell".to_string(),
                "sql" | "ddl" | "dml" => "SQL".to_string(),
                "xml" | "json" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" => {
                    "Configuration".to_string()
                }
                "md" | "markdown" | "rst" | "adoc" | "tex" => "Documentation".to_string(),
                "hs" | "lhs" => "Haskell".to_string(),
                "lisp" | "cl" | "el" | "clj" | "cljs" | "edn" | "scm" => "Lisp Family".to_string(),
                "erl" | "hrl" | "ex" | "exs" => "Erlang/Elixir".to_string(),
                "dart" => "Dart".to_string(),
                "m" | "mm" => "Objective-C".to_string(),
                _ => {
                    if self.is_source_code_file(filename) {
                        "Source Code".to_string()
                    } else {
                        ext.to_uppercase()
                    }
                }
            }
        } else {
            // Handle special files without extensions
            match filename.to_lowercase().as_str() {
                "makefile" => "Build".to_string(),
                "dockerfile" => "Docker".to_string(),
                "jenkinsfile" => "CI/CD".to_string(),
                _ => "Unknown".to_string(),
            }
        }
    }

    /// Check if a file should be prioritized for reading based on importance
    ///
    /// # Arguments
    /// * `filename` - The name of the file to check
    ///
    /// # Returns
    /// True if the file is considered high priority for analysis
    pub fn is_high_priority_file(&self, filename: &str) -> bool {
        let filename_lower = filename.to_lowercase();

        // Configuration and project files
        if matches!(
            filename_lower.as_str(),
            "cargo.toml"
                | "package.json"
                | "pyproject.toml"
                | "requirements.txt"
                | "readme.md"
                | "readme.txt"
                | "license"
                | "license.txt"
                | "license.md"
                | "makefile"
                | "dockerfile"
                | "jenkinsfile"
                | ".gitignore"
                | ".env"
        ) {
            return true;
        }

        // Main entry points
        if matches!(
            filename_lower.as_str(),
            "main.rs"
                | "lib.rs"
                | "main.py"
                | "__init__.py"
                | "index.js"
                | "index.ts"
                | "app.js"
                | "app.ts"
                | "main.go"
                | "main.java"
        ) {
            return true;
        }

        false
    }

    /// Get token budget allocation for multiple files
    ///
    /// # Arguments
    /// * `files` - List of filenames to analyze
    /// * `total_budget` - Total token budget available
    ///
    /// # Returns
    /// Vector of (filename, `allocated_tokens`) pairs
    pub fn allocate_token_budget(
        &self,
        files: &[String],
        total_budget: usize,
    ) -> Vec<(String, usize)> {
        if files.is_empty() {
            return Vec::new();
        }

        let mut allocations = Vec::new();
        let mut remaining_budget = total_budget;

        // First pass: allocate to high priority files
        let mut high_priority_files = Vec::new();
        let mut normal_files = Vec::new();

        for file in files {
            if self.is_high_priority_file(file) {
                high_priority_files.push(file.clone());
            } else {
                normal_files.push(file.clone());
            }
        }

        // Allocate 60% of budget to high priority files
        let high_priority_budget = (total_budget as f64 * 0.6) as usize;
        let normal_budget = total_budget - high_priority_budget;

        // Distribute high priority budget
        if !high_priority_files.is_empty() {
            let per_file_high = high_priority_budget / high_priority_files.len();
            for file in high_priority_files {
                let allocation = per_file_high.min(remaining_budget);
                allocations.push((file, allocation));
                remaining_budget = remaining_budget.saturating_sub(allocation);
            }
        }

        // Distribute remaining budget to normal files
        if !normal_files.is_empty() && remaining_budget > 0 {
            let per_file_normal = (normal_budget + remaining_budget) / normal_files.len();
            for file in normal_files {
                let allocation = per_file_normal.min(remaining_budget);
                allocations.push((file, allocation));
                remaining_budget = remaining_budget.saturating_sub(allocation);
            }
        }

        allocations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_code_detection() {
        let analyzer = FileExtensionAnalyzer::new();

        // Test various source code files
        assert!(analyzer.is_source_code_file("main.rs"));
        assert!(analyzer.is_source_code_file("script.py"));
        assert!(analyzer.is_source_code_file("component.tsx"));
        assert!(analyzer.is_source_code_file("style.css"));
        assert!(analyzer.is_source_code_file("config.json"));

        // Test non-source files
        assert!(!analyzer.is_source_code_file("image.png"));
        assert!(!analyzer.is_source_code_file("document.pdf"));
        assert!(!analyzer.is_source_code_file("data.csv"));

        // Test special files without extensions
        assert!(analyzer.is_source_code_file("Makefile"));
        assert!(analyzer.is_source_code_file("Dockerfile"));
        assert!(!analyzer.is_source_code_file("README"));
    }

    #[test]
    fn test_context_length_limits() {
        let analyzer = FileExtensionAnalyzer::new();

        // Source code files should get higher limits
        assert_eq!(analyzer.get_context_length_for_file("main.rs"), 24000);
        assert_eq!(analyzer.get_context_length_for_file("script.py"), 24000);

        // Non-source files should get default limits
        assert_eq!(analyzer.get_context_length_for_file("image.png"), 8000);
        assert_eq!(analyzer.get_context_length_for_file("data.csv"), 8000);
    }

    #[test]
    fn test_max_tokens_selection() {
        let analyzer = FileExtensionAnalyzer::new();

        // Test with both limits set
        assert_eq!(analyzer.select_max_tokens("main.rs", Some(1000), Some(500)), Some(1000));
        assert_eq!(analyzer.select_max_tokens("data.csv", Some(1000), Some(500)), Some(500));

        // Test with only one limit set
        assert_eq!(analyzer.select_max_tokens("main.rs", Some(1000), None), Some(1000));
        assert_eq!(analyzer.select_max_tokens("data.csv", Some(1000), None), None);

        // Test with no limits
        assert_eq!(analyzer.select_max_tokens("main.rs", None, None), None);
    }

    #[test]
    fn test_file_type_categorization() {
        let analyzer = FileExtensionAnalyzer::new();

        assert_eq!(analyzer.get_file_type_category("main.rs"), "Rust");
        assert_eq!(analyzer.get_file_type_category("script.py"), "Python");
        assert_eq!(analyzer.get_file_type_category("component.tsx"), "JavaScript/TypeScript");
        assert_eq!(analyzer.get_file_type_category("config.json"), "Configuration");
        assert_eq!(analyzer.get_file_type_category("Dockerfile"), "Docker");
    }

    #[test]
    fn test_high_priority_detection() {
        let analyzer = FileExtensionAnalyzer::new();

        // High priority files
        assert!(analyzer.is_high_priority_file("Cargo.toml"));
        assert!(analyzer.is_high_priority_file("package.json"));
        assert!(analyzer.is_high_priority_file("main.rs"));
        assert!(analyzer.is_high_priority_file("README.md"));

        // Normal priority files
        assert!(!analyzer.is_high_priority_file("utils.rs"));
        assert!(!analyzer.is_high_priority_file("test.py"));
        assert!(!analyzer.is_high_priority_file("style.css"));
    }

    #[test]
    fn test_token_budget_allocation() {
        let analyzer = FileExtensionAnalyzer::new();

        let files = vec![
            "Cargo.toml".to_string(), // High priority
            "main.rs".to_string(),    // High priority
            "utils.rs".to_string(),   // Normal
            "test.rs".to_string(),    // Normal
        ];

        let allocations = analyzer.allocate_token_budget(&files, 1000);

        // Should have allocations for all files
        assert_eq!(allocations.len(), 4);

        // Total should not exceed budget
        let total_allocated: usize = allocations.iter().map(|(_, tokens)| tokens).sum();
        assert!(total_allocated <= 1000);
    }
}
