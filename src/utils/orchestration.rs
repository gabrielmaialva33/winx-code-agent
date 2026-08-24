use std::fmt::Write as _;

const DEFAULT_INSTRUCTIONS: &str = concat!(
    "Initialize Winx once for the intended workspace. Treat structuredContent.status as ",
    "authoritative. Never repeat a failed tool call unchanged: execute next_action and every ",
    "required_read first. Keep the exact thread_id/workspace_root pair for the user's current ",
    "project; never borrow one from another chat. workspace_root identifies the session, not a ",
    "path sandbox: allowed absolute targets outside it do not require rebinding. If Initialize ",
    "reports initialize_workspace_already_bound or workspace_change_requires_new_session, do not ",
    "call Initialize again in that conversation. Use CodeMap with a concise query to locate code, ",
    "then batch ReadFiles for exact text. Read targets before editing; use MultiFileEdit for atomic ",
    "cross-file changes. Combine related finite checks with && or edit verify_command. For ",
    "BashCommand, follow next_action only while status is running and never auto-approve a prompt ",
    "that changes permissions, data, or system state. Use ReadFiles for unsupported CodeMap ",
    "languages; never transform source solely to make CodeMap parse it. Put useful derived helpers ",
    "only in the temporary_artifact_dir returned by Initialize (exported as WINX_TEMP_DIR). Keep ",
    "a small stable set: overwrite or reuse the same descriptive helper per purpose, retain ",
    "source-path/line provenance, remove obsolete files, and treat helpers as non-canonical. Never turn ",
    "command, lint, test, or search output into a parseable carrier merely to call CodeMap. CodeMap ",
    "on helpers accepts only an existing single file and has a smaller aggregate session budget; ",
    "canonical source maps remain available. Never encode payload in names or create .winx-* or ",
    ".winx_tmp artifacts at the project root."
);

const DISALLOW_INSTRUCTION: &str = "As soon as you encounter \"The user has chosen to disallow the tool call.\", immediately stop doing everything and ask the user for the reason.";

/// Instructions advertised in the MCP handshake. The stable defaults come
/// first so clients that truncate this field still receive the orchestration
/// contract; operator-provided rules are appended without requiring a rebuild.
pub fn server_instructions() -> String {
    let mut instructions = format!("{DEFAULT_INSTRUCTIONS}\n\n{DISALLOW_INSTRUCTION}");
    if let Some(extra) = crate::config::env_text("WINX_SERVER_INSTRUCTIONS") {
        let _ = write!(instructions, "\n\nOperator instructions:\n{extra}");
    }
    instructions
}

/// Mirror the handshake contract in Initialize output for older clients that
/// do not surface ServerInfo.instructions to the model.
pub fn append_initialize_instructions(response: &mut String) {
    let _ = write!(response, "\n# Winx orchestration contract\n{}\n", server_instructions());
}

#[cfg(test)]
mod tests {
    use super::server_instructions;

    #[test]
    fn default_contract_names_structured_recovery_before_optional_operator_text() {
        let instructions = server_instructions();
        let prefix = instructions.chars().take(512).collect::<String>();
        assert!(prefix.contains("structuredContent.status"));
        assert!(prefix.contains("Never repeat a failed tool call unchanged"));
        assert!(prefix.contains("required_read"));
        assert!(prefix.contains("thread_id/workspace_root pair"));
        assert!(prefix.contains("user's current project"));
        assert!(prefix.contains("not a path sandbox"));
    }

    #[test]
    fn default_contract_keeps_agent_artifacts_coherent() {
        let instructions = server_instructions();
        assert!(instructions.contains("never transform source solely"));
        assert!(instructions.contains("Use ReadFiles"));
        assert!(instructions.contains("temporary_artifact_dir returned by Initialize"));
        assert!(instructions.contains("exported as WINX_TEMP_DIR"));
        assert!(instructions.contains("helpers as non-canonical"));
        assert!(instructions.contains("overwrite or reuse the same descriptive helper"));
        assert!(instructions.contains("merely to call CodeMap"));
        assert!(instructions.contains("existing single file"));
        assert!(instructions.contains("Never encode payload in names"));
        assert!(instructions.contains(".winx-* or .winx_tmp artifacts"));
    }

    #[test]
    fn default_contract_stops_terminal_initialize_retries() {
        let instructions = server_instructions();
        assert!(instructions.contains("initialize_workspace_already_bound"));
        assert!(instructions.contains("workspace_change_requires_new_session"));
        assert!(instructions.contains("do not call Initialize again"));
        assert!(instructions.contains("allowed absolute targets outside it"));
    }
}
