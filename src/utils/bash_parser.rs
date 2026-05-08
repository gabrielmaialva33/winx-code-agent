use tree_sitter::Parser;

use crate::errors::{Result, WinxError};

pub fn assert_single_statement(command: &str) -> Result<()> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Ok(());
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
        return Err(WinxError::CommandExecutionError(
            "Command contains invalid bash syntax.".to_string(),
        ));
    }

    let statement_count = (0..root.named_child_count())
        .filter_map(|index| root.named_child(index))
        .filter(|node| node.kind() != "comment")
        .count();

    if statement_count > 1 {
        return Err(WinxError::CommandExecutionError(
            "Command should contain a single top-level bash statement.".to_string(),
        ));
    }

    Ok(())
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
    fn rejects_multiple_top_level_statements() {
        assert!(assert_single_statement("pwd\nls").is_err());
    }
}
