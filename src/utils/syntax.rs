use std::fmt::Write as FmtWrite;
use std::path::Path;
use std::process::Command;

use tree_sitter::{Node, Parser};

pub fn syntax_warning(path: &Path, content: &str) -> Option<String> {
    let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default();
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();

    match ext {
        "json" => serde_json::from_str::<serde_json::Value>(content)
            .err()
            .map(|error| format!("Syntax warning: JSON parser reported: {error}")),
        "toml" => toml::from_str::<toml::Value>(content)
            .err()
            .map(|error| format!("Syntax warning: TOML parser reported: {error}")),
        "rs" => {
            let language = tree_sitter_rust::LANGUAGE.into();
            tree_sitter_warning(content, &language, "Rust")
        }
        "js" | "mjs" | "cjs" | "jsx" => {
            let language = tree_sitter_javascript::LANGUAGE.into();
            tree_sitter_warning(content, &language, "JavaScript")
        }
        "ts" => {
            let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
            tree_sitter_warning(content, &language, "TypeScript")
        }
        "tsx" => {
            let language = tree_sitter_typescript::LANGUAGE_TSX.into();
            tree_sitter_warning(content, &language, "TSX")
        }
        // zsh is checked with the bash grammar too. The previous `if ext != "zsh"`
        // guard made the whole arm a no-op for .zsh files.
        "sh" | "bash" | "zsh" => {
            let language = tree_sitter_bash::LANGUAGE.into();
            tree_sitter_warning(content, &language, "shell")
        }
        "go" => {
            let language = tree_sitter_go::LANGUAGE.into();
            tree_sitter_warning(content, &language, "Go")
        }
        "c" => {
            let language = tree_sitter_c::LANGUAGE.into();
            tree_sitter_warning(content, &language, "C")
        }
        // `.h` is parsed with the C++ grammar, which accepts C headers too.
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "h" => {
            let language = tree_sitter_cpp::LANGUAGE.into();
            tree_sitter_warning(content, &language, "C++")
        }
        "java" => {
            let language = tree_sitter_java::LANGUAGE.into();
            tree_sitter_warning(content, &language, "Java")
        }
        "rb" => {
            let language = tree_sitter_ruby::LANGUAGE.into();
            tree_sitter_warning(content, &language, "Ruby")
        }
        "css" => {
            let language = tree_sitter_css::LANGUAGE.into();
            tree_sitter_warning(content, &language, "CSS")
        }
        "html" | "htm" => {
            let language = tree_sitter_html::LANGUAGE.into();
            tree_sitter_warning(content, &language, "HTML")
        }
        "php" => {
            let language = tree_sitter_php::LANGUAGE_PHP.into();
            tree_sitter_warning(content, &language, "PHP")
        }
        "cs" => {
            let language = tree_sitter_c_sharp::LANGUAGE.into();
            tree_sitter_warning(content, &language, "C#")
        }
        "lua" => {
            let language = tree_sitter_lua::LANGUAGE.into();
            tree_sitter_warning(content, &language, "Lua")
        }
        // Python keeps the interpreter `compile()` check (not tree-sitter): the
        // tree-sitter-python grammar silently accepts IndentationError and py2
        // `print` statements, and indentation is *the* classic Python mistake —
        // verified empirically that tree-sitter misses those, compile() catches them.
        "py" | "pyi" => python_warning(path),
        _ if matches!(file_name, "Dockerfile" | "Makefile") => None,
        _ => None,
    }
}

fn tree_sitter_warning(
    content: &str,
    language: &tree_sitter::Language,
    language_name: &str,
) -> Option<String> {
    let mut parser = Parser::new();
    if let Err(error) = parser.set_language(language) {
        return Some(format!("Syntax warning: failed to load {language_name} parser: {error}"));
    }

    let tree = parser.parse(content, None)?;
    let root = tree.root_node();
    if !root.has_error() {
        return None;
    }

    let mut message =
        format!("Syntax warning: tree-sitter reported {language_name} syntax errors.");
    if let Some(row) = first_error_row(root) {
        let _ = write!(message, "\n{}", error_context(content, row));
    }
    Some(message)
}

/// Depth-first search for the first ERROR or MISSING node, returning its 0-based
/// start row. Mirrors wcgw's `get_context_for_errors`, which surfaces *where* the
/// parse broke instead of just stating that it did.
fn first_error_row(node: Node<'_>) -> Option<usize> {
    if node.is_error() || node.is_missing() {
        return Some(node.start_position().row);
    }
    if !node.has_error() {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(row) = first_error_row(child) {
            return Some(row);
        }
    }
    None
}

/// Render the ~10 lines around `error_row` (0-based) with a `>` marker on the
/// offending line, so the model can see and fix the error in place.
fn error_context(content: &str, error_row: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let start = error_row.saturating_sub(10);
    let end = (error_row + 11).min(lines.len());
    let mut out = String::from("<snippet>\n");
    for (offset, line) in lines[start..end].iter().enumerate() {
        let line_no = start + offset + 1;
        let marker = if start + offset == error_row { '>' } else { ' ' };
        let _ = writeln!(out, "{marker}{line_no} {line}");
    }
    out.push_str("</snippet>");
    out
}

fn python_warning(path: &Path) -> Option<String> {
    let python = python_interpreter()?;

    // `compile()` parses without executing and, unlike `python -m py_compile`,
    // does NOT write a `.pyc` next to the source — so syntax-checking a write
    // never litters the user's tree with `__pycache__`.
    let output = Command::new(python)
        .args([
            "-c",
            "import sys; compile(open(sys.argv[1], encoding='utf-8').read(), sys.argv[1], 'exec')",
        ])
        .arg(path)
        .output()
        .ok()?;
    (!output.status.success()).then(|| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        format!("Syntax warning: Python parser reported:\n{}", stderr.trim())
    })
}

/// The available Python interpreter, probed once. The probe used to re-spawn
/// `python --version` on every `.py` write.
fn python_interpreter() -> Option<&'static str> {
    static PYTHON: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *PYTHON.get_or_init(|| {
        if Command::new("python3").arg("--version").output().is_ok() {
            Some("python3")
        } else if Command::new("python").arg("--version").output().is_ok() {
            Some("python")
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::syntax_warning;
    use std::path::Path;

    #[test]
    fn reports_invalid_json() {
        assert!(syntax_warning(Path::new("bad.json"), "{").is_some());
    }

    #[test]
    fn accepts_valid_rust() {
        assert!(syntax_warning(Path::new("lib.rs"), "fn main() {}\n").is_none());
    }

    #[test]
    fn reports_invalid_bash() {
        assert!(syntax_warning(Path::new("script.sh"), "if true; then\n").is_some());
    }

    #[test]
    fn reports_invalid_zsh() {
        // Regression: the old `if ext != "zsh"` guard skipped .zsh entirely.
        assert!(syntax_warning(Path::new("script.zsh"), "if true; then\n").is_some());
    }

    #[test]
    fn error_warning_includes_snippet() {
        let warning = syntax_warning(Path::new("lib.rs"), "fn main() {\n").unwrap_or_default();
        assert!(warning.contains("<snippet>"), "expected snippet, got: {warning}");
    }

    #[test]
    fn checks_go() {
        assert!(syntax_warning(Path::new("main.go"), "package main\nfunc main() {}\n").is_none());
        assert!(syntax_warning(Path::new("main.go"), "package main\nfunc main( {\n").is_some());
    }

    #[test]
    fn checks_php() {
        assert!(syntax_warning(Path::new("a.php"), "<?php echo 1; ?>\n").is_none());
        assert!(syntax_warning(Path::new("a.php"), "<?php echo (1; ?>\n").is_some());
    }
}
