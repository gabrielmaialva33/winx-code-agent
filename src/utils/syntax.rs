use std::path::Path;
use std::process::Command;

use tree_sitter::Parser;

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
        "sh" | "bash" | "zsh" if ext != "zsh" => {
            let language = tree_sitter_bash::LANGUAGE.into();
            tree_sitter_warning(content, &language, "bash")
        }
        "py" => python_warning(path),
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
    tree.root_node()
        .has_error()
        .then(|| format!("Syntax warning: tree-sitter reported {language_name} syntax errors."))
}

fn python_warning(path: &Path) -> Option<String> {
    let python = if Command::new("python3").arg("--version").output().is_ok() {
        "python3"
    } else if Command::new("python").arg("--version").output().is_ok() {
        "python"
    } else {
        return None;
    };

    let output = Command::new(python).args(["-m", "py_compile"]).arg(path).output().ok()?;
    (!output.status.success()).then(|| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        format!("Syntax warning: Python parser reported:\n{}", stderr.trim())
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
}
