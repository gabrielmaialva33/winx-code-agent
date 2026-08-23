use std::fmt::Write as _;

const DEFAULT_INSTRUCTIONS: &str = "Initialize Winx once for the intended workspace and preserve the returned thread_id. Treat structuredContent.status as the authoritative tool state. Never repeat a failed tool call unchanged: execute next_action and every required_read first. Use CodeMap to locate code, then batch ReadFiles calls. Read every target file and required range before editing. Use MultiFileEdit for atomic cross-file changes. Compose related finite fail-fast checks in one BashCommand with &&. When an edit tool exposes verify_command, use it for a quick finite post-edit check in the same round trip; the edit remains applied if verification fails. BashCommand defaults to wait_policy=adaptive; use until_complete for finite long commands and return_early when prompt control matters. Call status_check only when status is running. Do not automatically approve interactive prompts that change permissions, data, or system state.";

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
    }
}
