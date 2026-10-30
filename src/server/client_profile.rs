//! Client family detection for transport-level tuning.
//!
//! Remote clients differ in ways that matter for latency and payload size
//! (per-call timeouts, which result fields reach the model). Detection uses the
//! self-reported `clientInfo.name`, so it only ever selects tuning, never
//! authority: path, command, mode and principal checks never consult it.

use rmcp::service::{RequestContext, RoleServer};

/// Known MCP client families. Unknown clients get the conservative defaults.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ClientProfile {
    /// `ChatGPT` connectors and apps (`openai-mcp`).
    ChatGpt,
    /// `OpenAI` Codex (`openai-mcp (Codex)` or a `codex` client name).
    Codex,
    /// claude.ai connectors, Claude Code and Claude Desktop.
    Claude,
    #[default]
    Other,
}

impl ClientProfile {
    pub(crate) fn from_context(context: &RequestContext<RoleServer>) -> Self {
        context.client_info().map_or(Self::Other, |client| Self::from_client_name(&client.name))
    }

    pub(crate) fn from_client_name(name: &str) -> Self {
        let name = name.trim().to_ascii_lowercase();
        if name.contains("codex") {
            Self::Codex
        } else if name.starts_with("openai-mcp") || name.starts_with("chatgpt") {
            Self::ChatGpt
        } else if name.starts_with("claude") || name.contains("anthropic") {
            Self::Claude
        } else {
            Self::Other
        }
    }

    /// Stable lowercase label for telemetry.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ChatGpt => "chatgpt",
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Other => "other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ClientProfile;

    #[test]
    fn client_names_map_to_families() {
        assert_eq!(ClientProfile::from_client_name("openai-mcp"), ClientProfile::ChatGpt);
        assert_eq!(ClientProfile::from_client_name("openai-mcp (Codex)"), ClientProfile::Codex);
        assert_eq!(ClientProfile::from_client_name("codex-mcp-client"), ClientProfile::Codex);
        assert_eq!(ClientProfile::from_client_name("claude-ai"), ClientProfile::Claude);
        assert_eq!(ClientProfile::from_client_name("claude-code"), ClientProfile::Claude);
        assert_eq!(
            ClientProfile::from_client_name("long-verification-client"),
            ClientProfile::Other
        );
        assert_eq!(ClientProfile::from_client_name(""), ClientProfile::Other);
    }
}
