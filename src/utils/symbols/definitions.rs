//! Exact definition spans for `ReadFiles`, using `CodeMap`'s embedded tags queries.
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tree_sitter::{Node, ParseOptions, Parser, QueryCursor, QueryCursorOptions, StreamingIterator};
struct Configuration {
    language: tree_sitter::Language,
    query: tree_sitter::Query,
}

pub const MAX_SOURCE_BYTES: usize = 2_000_000;
pub const MAX_MATCHES: usize = 8;
const MAX_CANDIDATES: usize = 16;
const MAX_SELECTOR_BYTES: usize = 1_024;
const RESOLUTION_TIME: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionRange {
    pub name: String,
    pub qualified_name: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    #[error("symbol_not_found")]
    NotFound,
    #[error("symbol_language_unsupported")]
    UnsupportedLanguage,
    #[error("symbol_parse_failed")]
    ParseFailed,
    #[error("symbol_ambiguous")]
    TooManyMatches,
    #[error("symbol_invalid_selector")]
    InvalidSelector,
    #[error("symbol_source_too_large")]
    SourceTooLarge,
    #[error("symbol_resolution_budget")]
    BudgetExceeded,
}

#[derive(Clone, Debug, thiserror::Error, Serialize)]
#[error("{reason}")]
#[serde(rename_all = "camelCase")]
pub struct ResolutionFailure {
    pub reason: FailureReason,
    pub candidates: Vec<DefinitionRange>,
    pub candidates_truncated: bool,
}
impl From<FailureReason> for ResolutionFailure {
    fn from(reason: FailureReason) -> Self {
        Self { reason, candidates: Vec::new(), candidates_truncated: false }
    }
}

fn configuration(ext: &str) -> Result<Arc<Configuration>, ResolutionFailure> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Configuration>>>> = OnceLock::new();
    if !super::supports(ext) {
        return Err(FailureReason::UnsupportedLanguage.into());
    }
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(config) = cache.lock().map_err(|_| FailureReason::ParseFailed)?.get(ext) {
        return Ok(config.clone());
    }
    let (language, query) = super::lang_and_query(ext).ok_or(FailureReason::UnsupportedLanguage)?;
    let query = if matches!(ext, "ts" | "tsx") {
        // TypeScript's embedded query extends JavaScript's implemented items.
        format!("{}\n{query}", tree_sitter_javascript::TAGS_QUERY)
    } else {
        query.to_owned()
    };
    // ReadFiles consumes @name and @definition.* directly. Other grammar
    // captures (for example C#'s @module) remain harmless unused captures.
    let query =
        tree_sitter::Query::new(&language, &query).map_err(|_| FailureReason::ParseFailed)?;
    let config = Arc::new(Configuration { language, query });
    let mut cache = cache.lock().map_err(|_| FailureReason::ParseFailed)?;
    // Only extensions admitted by the static grammar catalog enter this cache.
    Ok(cache.entry(ext.to_owned()).or_insert(config).clone())
}

/// Resolve all matching definitions without reading or mutating the filesystem.
/// Qualified names accept `.` or `::`; short names include every matching owner.
pub fn resolve(
    ext: &str,
    source: &str,
    selector: &str,
) -> Result<Vec<DefinitionRange>, ResolutionFailure> {
    if selector.is_empty()
        || selector.len() > MAX_SELECTOR_BYTES
        || selector.chars().any(char::is_control)
    {
        return Err(FailureReason::InvalidSelector.into());
    }
    let selector = selector.replace("::", ".");
    if selector.split('.').any(str::is_empty) {
        return Err(FailureReason::InvalidSelector.into());
    }
    if source.len() > MAX_SOURCE_BYTES {
        return Err(FailureReason::SourceTooLarge.into());
    }
    let config = configuration(ext)?;
    let deadline = Instant::now() + RESOLUTION_TIME;
    let mut parser = Parser::new();
    parser.set_language(&config.language).map_err(|_| FailureReason::ParseFailed)?;
    let mut check = |_: &tree_sitter::ParseState| {
        if Instant::now() >= deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let tree = parser
        .parse_with_options(
            &mut |index, _| source.as_bytes().get(index..).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut check)),
        )
        .ok_or(FailureReason::BudgetExceeded)?;
    if tree.root_node().has_error() {
        return Err(FailureReason::ParseFailed.into());
    }
    collect(&config, tree.root_node(), source, &selector, ext, deadline)
}

fn collect(
    config: &Configuration,
    root: Node<'_>,
    source: &str,
    selector: &str,
    ext: &str,
    deadline: Instant,
) -> Result<Vec<DefinitionRange>, ResolutionFailure> {
    let names = config.query.capture_names();
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(16_384);
    let mut check = |_: &tree_sitter::QueryCursorState| {
        if Instant::now() >= deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut matches = cursor.matches_with_options(
        &config.query,
        root,
        source.as_bytes(),
        QueryCursorOptions::new().progress_callback(&mut check),
    );
    let mut candidates = Vec::new();
    let mut candidates_truncated = false;
    while let Some(found) = matches.next() {
        if Instant::now() >= deadline {
            return Err(FailureReason::BudgetExceeded.into());
        }
        let Some(name_node) = found
            .captures()
            .iter()
            .find(|capture| names[capture.index as usize] == "name")
            .map(|capture| capture.node)
        else {
            continue;
        };
        let Some(node) = found
            .captures()
            .iter()
            .find(|capture| names[capture.index as usize].starts_with("definition."))
            .map(|capture| capture.node)
        else {
            continue;
        };
        let name = text(name_node, source)?;
        let short_name = name.rsplit(['.', ':']).next().unwrap_or(name);
        if selector.rsplit('.').next() != Some(short_name) {
            continue;
        }
        let qualified_name = qualified_name(node, name, source)?;
        if selector.contains('.')
            && qualified_name != selector
            && !qualified_name.ends_with(&format!(".{selector}"))
        {
            continue;
        }
        let expanded = wrapper(complete_definition(node));
        let start_line = attached_start(expanded, source, ext, deadline)?;
        let end_line = last_line(expanded);
        if end_line < start_line {
            return Err(FailureReason::ParseFailed.into());
        }
        let candidate =
            DefinitionRange { name: short_name.to_owned(), qualified_name, start_line, end_line };
        if candidates.contains(&candidate) {
            continue;
        }
        if candidates.len() == MAX_CANDIDATES {
            candidates_truncated = true;
            break;
        }
        candidates.push(candidate);
    }
    drop(matches);
    if cursor.did_exceed_match_limit() || Instant::now() >= deadline {
        return Err(FailureReason::BudgetExceeded.into());
    }
    candidates.sort_by_key(|candidate| (candidate.start_line, candidate.end_line));
    if candidates.len() > MAX_MATCHES {
        return Err(ResolutionFailure {
            reason: FailureReason::TooManyMatches,
            candidates,
            candidates_truncated,
        });
    }
    if candidates.is_empty() {
        return Err(FailureReason::NotFound.into());
    }
    Ok(candidates)
}

fn text<'a>(node: Node<'_>, source: &'a str) -> Result<&'a str, ResolutionFailure> {
    source.get(node.byte_range()).ok_or_else(|| FailureReason::ParseFailed.into())
}

fn qualified_name(node: Node<'_>, name: &str, source: &str) -> Result<String, ResolutionFailure> {
    let mut owners = Vec::new();
    if node.kind() == "method_declaration" {
        if let Some(receiver) = node
            .child_by_field_name("receiver")
            .and_then(|receiver| receiver.named_child(0))
            .and_then(|parameter| parameter.child_by_field_name("type"))
        {
            owners.push(normalize_owner(text(receiver, source)?));
        }
    }
    let mut parent = node.parent();
    let mut depth = 0;
    while let Some(node) = parent {
        depth += 1;
        if depth > 128 {
            return Err(FailureReason::BudgetExceeded.into());
        }
        if let Some(owner) = owner(node, source)? {
            if owner.len() > MAX_SELECTOR_BYTES {
                return Err(FailureReason::BudgetExceeded.into());
            }
            owners.push(normalize_owner(owner));
        }
        parent = node.parent();
    }
    owners.reverse();
    owners.push(name.replace("::", "."));
    let qualified = owners.join(".");
    if qualified.len() > MAX_SELECTOR_BYTES {
        return Err(FailureReason::BudgetExceeded.into());
    }
    Ok(qualified)
}

fn normalize_owner(value: &str) -> String {
    value
        .split(['<', '['])
        .next()
        .unwrap_or(value)
        .trim()
        .trim_start_matches(['*', '&'])
        .replace("::", ".")
}

fn owner<'a>(node: Node<'_>, source: &'a str) -> Result<Option<&'a str>, ResolutionFailure> {
    let owner = match node.kind() {
        "impl_item" => node.child_by_field_name("type"),
        "class_definition"
        | "class_declaration"
        | "class_specifier"
        | "struct_specifier"
        | "struct_declaration"
        | "interface_declaration"
        | "trait_item"
        | "mod_item"
        | "namespace_definition"
        | "namespace_declaration"
        | "function_item"
        | "function_definition"
        | "function_declaration"
        | "method_definition"
        | "method_declaration"
        | "variable_declarator"
        | "class"
        | "module"
        | "method" => node.child_by_field_name("name"),
        "call"
            if node
                .child_by_field_name("target")
                .and_then(|target| source.get(target.byte_range()))
                == Some("defmodule") =>
        {
            node.child_by_field_name("arguments").and_then(|arguments| arguments.named_child(0))
        }
        _ => None,
    };
    owner.map(|owner| text(owner, source)).transpose()
}

fn complete_definition(mut node: Node<'_>) -> Node<'_> {
    if node.kind() != "function_declarator" {
        return node;
    }
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "function_definition" | "declaration" | "field_declaration" | "type_definition" => {
                return parent
            }
            "translation_unit" | "compound_statement" => return node,
            _ => node = parent,
        }
    }
    node
}

fn wrapper(mut node: Node<'_>) -> Node<'_> {
    while let Some(parent) = node.parent().filter(|parent| {
        matches!(
            parent.kind(),
            "decorated_definition"
                | "export_statement"
                | "template_declaration"
                | "ambient_declaration"
        )
    }) {
        node = parent;
    }
    node
}

fn last_line(node: Node<'_>) -> usize {
    let end = node.end_position();
    end.row + usize::from(end.column > 0)
}

fn attached_start(
    mut node: Node<'_>,
    source: &str,
    ext: &str,
    deadline: Instant,
) -> Result<usize, ResolutionFailure> {
    if node.kind() == "variable_declarator" {
        if let Some(parent) = node.parent().filter(|parent| {
            matches!(parent.kind(), "lexical_declaration" | "variable_declaration")
                && parent.named_child(0) == Some(node)
        }) {
            node = wrapper(parent);
        }
    }
    let mut start = node.start_position().row + 1;
    while let Some(previous) = previous_metadata_node(node) {
        if Instant::now() >= deadline {
            return Err(FailureReason::BudgetExceeded.into());
        }
        if start > last_line(previous) + 1 {
            break;
        }
        let Some(gap) = source.get(previous.end_byte()..node.start_byte()) else { break };
        if !gap.trim().is_empty() {
            break;
        }
        let Some(value) = source.get(previous.byte_range()) else { break };
        let line_start = source[..previous.start_byte()].rfind('\n').map_or(0, |index| index + 1);
        if !source[line_start..previous.start_byte()].trim().is_empty()
            || !attached_metadata(value, ext)
        {
            break;
        }
        start = previous.start_position().row + 1;
        node = previous;
    }
    Ok(start)
}

fn previous_metadata_node(mut node: Node<'_>) -> Option<Node<'_>> {
    loop {
        if let Some(previous) = node.prev_named_sibling() {
            return Some(previous);
        }
        // Python can place leading comments before the first body block rather
        // than inside it. Ascend only wrappers that share this exact start.
        let parent = node.parent()?;
        if parent.start_byte() != node.start_byte() {
            return None;
        }
        node = parent;
    }
}

fn attached_metadata(value: &str, ext: &str) -> bool {
    let value = value.trim_start();
    match ext {
        "rs" => value.starts_with("///") || value.starts_with("/**") || value.starts_with("#["),
        "py" | "pyi" | "rb" => value.starts_with('#') && !value.starts_with("#!"),
        "go" => value.starts_with("//") || value.starts_with("/*"),
        "lua" => value.starts_with("---"),
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" => {
            value.starts_with("/**") || value.starts_with("///") || value.starts_with('@')
        }
        "ex" | "exs" => {
            ["@doc", "@spec", "@impl", "@deprecated"].iter().any(|prefix| value.starts_with(prefix))
        }
        _ => value.starts_with("/**") || value.starts_with("///"),
    }
}
