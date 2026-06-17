//! Symbol/outline extraction via `tree-sitter-tags` and each grammar's embedded
//! `TAGS_QUERY` — the same engine GitHub uses for code navigation. We surface
//! only definitions (functions, types, methods, ...), not references.
//!
//! 11 languages have a tags query (rust, js/ts, go, c, c++, java, ruby, c#, php,
//! lua); the others (bash/css/html/json) have no code symbols and return empty.

use tree_sitter_tags::{TagsConfiguration, TagsContext};

/// A code symbol occurrence (definition or reference) from a source file.
#[derive(Debug, Clone)]
pub struct Symbol {
    /// Symbol name (e.g. `parse_config`).
    pub name: String,
    /// Symbol kind from the grammar's tags query (e.g. `function`, `struct`,
    /// `method`, `class`, `call`).
    pub kind: String,
    /// 1-based line where the occurrence starts.
    pub line: usize,
    /// True for a definition, false for a reference (call / use site).
    pub is_definition: bool,
}

/// Map a file extension to its tree-sitter language + embedded tags query.
/// Returns `None` for unsupported / non-code extensions.
fn lang_and_query(ext: &str) -> Option<(tree_sitter::Language, &'static str)> {
    let pair = match ext {
        "rs" => (tree_sitter_rust::LANGUAGE.into(), tree_sitter_rust::TAGS_QUERY),
        "js" | "mjs" | "cjs" | "jsx" => {
            (tree_sitter_javascript::LANGUAGE.into(), tree_sitter_javascript::TAGS_QUERY)
        }
        "ts" => {
            (tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), tree_sitter_typescript::TAGS_QUERY)
        }
        "tsx" => (tree_sitter_typescript::LANGUAGE_TSX.into(), tree_sitter_typescript::TAGS_QUERY),
        "go" => (tree_sitter_go::LANGUAGE.into(), tree_sitter_go::TAGS_QUERY),
        "c" | "h" => (tree_sitter_c::LANGUAGE.into(), tree_sitter_c::TAGS_QUERY),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => {
            (tree_sitter_cpp::LANGUAGE.into(), tree_sitter_cpp::TAGS_QUERY)
        }
        "java" => (tree_sitter_java::LANGUAGE.into(), tree_sitter_java::TAGS_QUERY),
        "rb" => (tree_sitter_ruby::LANGUAGE.into(), tree_sitter_ruby::TAGS_QUERY),
        "cs" => (tree_sitter_c_sharp::LANGUAGE.into(), tree_sitter_c_sharp::TAGS_QUERY),
        "php" => (tree_sitter_php::LANGUAGE_PHP.into(), tree_sitter_php::TAGS_QUERY),
        "lua" => (tree_sitter_lua::LANGUAGE.into(), tree_sitter_lua::TAGS_QUERY),
        _ => return None,
    };
    Some(pair)
}

/// Whether `ext` has a symbol extractor (used to filter repo-map candidates).
pub fn supports(ext: &str) -> bool {
    lang_and_query(ext).is_some()
}

/// Compile the tags configuration for `ext`, or `None` if unsupported / the
/// query fails to compile. Compiling is non-trivial, so callers cache per
/// language across a repo-map pass.
pub fn config_for(ext: &str) -> Option<TagsConfiguration> {
    let (language, query) = lang_and_query(ext)?;
    TagsConfiguration::new(language, query, "").ok()
}

/// Extract definition symbols from `text` using a pre-built `config`.
/// `context` is reusable across files.
pub fn extract(context: &mut TagsContext, config: &TagsConfiguration, text: &str) -> Vec<Symbol> {
    collect(context, config, text, true)
}

/// Extract all occurrences — definitions AND references (call/use sites).
pub fn extract_all(
    context: &mut TagsContext,
    config: &TagsConfiguration,
    text: &str,
) -> Vec<Symbol> {
    collect(context, config, text, false)
}

/// Shared collector. Best-effort: a parse/iteration error yields whatever was
/// gathered so far (possibly empty). `source[tag.name_range]` is safe because
/// `text` is valid UTF-8 and the range comes from tree-sitter over that buffer.
fn collect(
    context: &mut TagsContext,
    config: &TagsConfiguration,
    text: &str,
    definitions_only: bool,
) -> Vec<Symbol> {
    let source = text.as_bytes();
    let mut out = Vec::new();
    let Ok((tags, _)) = context.generate_tags(config, source, None) else {
        return out;
    };
    for tag in tags.flatten() {
        if definitions_only && !tag.is_definition {
            continue;
        }
        let name = String::from_utf8_lossy(&source[tag.name_range.clone()]).into_owned();
        if name.is_empty() {
            continue;
        }
        out.push(Symbol {
            name,
            kind: config.syntax_type_name(tag.syntax_type_id).to_string(),
            line: tag.span.start.row + 1,
            is_definition: tag.is_definition,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    #[test]
    fn extracts_rust_symbols() {
        let mut ctx = TagsContext::new();
        let cfg = config_for("rs").expect("rust config");
        let src = "pub fn alpha() {}\nstruct Beta;\nimpl Beta { fn gamma(&self) {} }\n";
        let syms = extract(&mut ctx, &cfg, src);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"alpha"), "got {names:?}");
        assert!(names.contains(&"Beta"), "got {names:?}");
        assert!(names.contains(&"gamma"), "got {names:?}");
        // alpha is defined on line 1.
        assert_eq!(syms.iter().find(|s| s.name == "alpha").map(|s| s.line), Some(1));
    }

    #[test]
    fn unsupported_extension_has_no_config() {
        assert!(config_for("json").is_none());
        assert!(!supports("css"));
        assert!(supports("rs"));
        assert!(supports("ts"));
    }
}
