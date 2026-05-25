use tree_sitter::{Node, Parser};

use crate::errors::{Result, WinxError};

pub fn assert_single_statement(command: &str) -> Result<()> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    if trimmed.contains('\0') {
        return Err(WinxError::CommandExecutionError(
            "Command contains a NUL byte. JSON escape \\u0000 becomes an actual NUL before bash sees it; write \\\\0 or \\\\x00 in the command string instead.".to_string(),
        ));
    }

    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
    parser.set_language(&language).map_err(|error| {
        WinxError::CommandExecutionError(format!("Failed to load bash parser: {error}"))
    })?;

    let tree = parser.parse(trimmed, None).ok_or_else(|| {
        WinxError::CommandExecutionError("Failed to parse bash command".to_string())
    })?;
    let root = tree.root_node();

    if root.has_error() {
        if bash_accepts_syntax(trimmed) {
            return Ok(());
        }

        return Err(WinxError::CommandExecutionError(
            "Command contains invalid bash syntax. If this is a complex script, pass it as multiline bash, avoid NUL bytes, or set allow_multi=true after verifying the quoting.".to_string(),
        ));
    }

    let statement_count = top_level_statement_count(trimmed, root);

    if statement_count > 1 && !trimmed.contains('\n') {
        return Err(WinxError::CommandExecutionError(
            "Command should contain a single top-level bash statement. For deliberate scripts, split statements across lines or set allow_multi=true.".to_string(),
        ));
    }

    Ok(())
}

fn bash_accepts_syntax(command: &str) -> bool {
    std::process::Command::new("bash")
        .arg("-n")
        .arg("-c")
        .arg(command)
        .status()
        .is_ok_and(|status| status.success())
}

#[derive(Debug, Clone)]
struct StatementNode {
    kind: String,
    start_byte: usize,
    end_byte: usize,
}

fn top_level_statement_count(source: &str, root: Node<'_>) -> usize {
    let mut statements = Vec::new();
    collect_statement_nodes(root, &mut statements);

    statements
        .iter()
        .filter(|stmt| stmt.kind != "comment")
        .filter(|stmt| !statements.iter().any(|other| is_contained_statement(source, stmt, other)))
        .count()
}

fn collect_statement_nodes(node: Node<'_>, statements: &mut Vec<StatementNode>) {
    if is_statement_node(node.kind()) {
        statements.push(StatementNode {
            kind: node.kind().to_string(),
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
        });
    }

    for index in 0..node.named_child_count() as u32 {
        if let Some(child) = node.named_child(index) {
            collect_statement_nodes(child, statements);
        }
    }
}

fn is_statement_node(kind: &str) -> bool {
    matches!(
        kind,
        "command"
            | "variable_assignment"
            | "declaration_command"
            | "unset_command"
            | "comment"
            | "for_statement"
            | "c_style_for_statement"
            | "while_statement"
            | "if_statement"
            | "case_statement"
            | "function_definition"
            | "pipeline"
            | "list"
            | "compound_statement"
            | "subshell"
            | "redirected_statement"
    )
}

fn is_contained_statement(source: &str, stmt: &StatementNode, other: &StatementNode) -> bool {
    if stmt.start_byte == other.start_byte
        && stmt.end_byte == other.end_byte
        && stmt.kind == other.kind
    {
        return false;
    }

    let other_text = &source[other.start_byte..other.end_byte];
    if other.kind == "list" && other_text.contains(';') {
        return false;
    }

    other.start_byte <= stmt.start_byte
        && other.end_byte >= stmt.end_byte
        && other.end_byte - other.start_byte > stmt.end_byte - stmt.start_byte
        && other_text.contains(&source[stmt.start_byte..stmt.end_byte])
}

#[cfg(test)]
mod tests {
    use super::assert_single_statement;

    #[test]
    fn accepts_shell_chains_as_single_statement() {
        assert!(assert_single_statement("cargo test && cargo clippy").is_ok());
    }

    #[test]
    fn accepts_heredocs_as_single_statement() {
        let command = "cat <<'EOF'\nhello\nEOF";
        assert!(assert_single_statement(command).is_ok());
    }

    #[test]
    fn accepts_for_loop_as_single_compound_statement() {
        assert!(assert_single_statement("for i in 1 2 3; do echo tick; sleep 1; done").is_ok());
    }

    #[test]
    fn rejects_semicolon_separated_top_level_statements() {
        assert!(assert_single_statement("pwd; ls").is_err());
    }

    #[test]
    fn accepts_multiline_scripts() {
        assert!(assert_single_statement("pwd\nls").is_ok());
    }

    #[test]
    fn accepts_bash_lc_script_when_tree_sitter_reports_error() {
        let command = "bash -lc 'printf \"%s\\n\" \"-- drm connectors --\"; for s in /sys/class/drm/card*-*/status; do [ -e \"$s\" ] || continue; c=${s%/status}; printf \"%s: %s\" \"${c##*/}\" \"$(cat \"$s\")\"; done'";
        assert!(assert_single_statement(command).is_ok());
    }

    #[test]
    fn rejects_nul_with_actionable_message() {
        let error = match assert_single_statement("printf '\0'") {
            Ok(()) => String::new(),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("NUL byte"));
        assert!(error.contains("\\\\x00"));
    }
}
