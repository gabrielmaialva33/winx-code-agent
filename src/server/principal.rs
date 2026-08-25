use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use axum::http::request::Parts;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ErrorData as McpError;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::{HttpPrincipal, HttpSessionAffinity};
use crate::errors::{Result, WinxError};
use crate::state::bash_state::generate_thread_id;
use crate::tool_registry::ToolKind;
use crate::types::{normalize_thread_id, MAX_NORMALIZED_THREAD_ID_BYTES};

const SCOPED_HASH_SUFFIX_BYTES: usize = 9;
const AFFINITY_ID_VERSION: &str = "workspace-binding-v2";

/// Per-request principal and the internal identifiers that must be translated
/// back before a response is returned to the remote client.
#[derive(Clone, Default)]
pub(super) struct RequestScope {
    principal: Option<HttpPrincipal>,
    replacements: Vec<(String, String)>,
}

impl RequestScope {
    pub(super) fn principal(&self) -> Option<&HttpPrincipal> {
        self.principal.as_ref()
    }

    pub(super) fn unscope_result(&self, result: &mut CallToolResult) {
        if self.replacements.is_empty() {
            return;
        }
        for content in &mut result.content {
            if let ContentBlock::Text(text) = content {
                text.text = self.unscope_text(&text.text);
            }
        }
        if let Some(structured) = result.structured_content.as_mut() {
            unscope_json(structured, &self.replacements);
        }
    }

    pub(super) fn unscope_error(&self, error: &mut McpError) {
        if !self.replacements.is_empty() {
            error.message = self.unscope_text(&error.message).into();
        }
    }

    pub(super) fn unscope_text(&self, text: &str) -> String {
        self.replacements.iter().fold(text.to_string(), |output, (internal, external)| {
            output.replace(internal, external)
        })
    }
}

pub(super) fn principal_from_context(
    context: &RequestContext<RoleServer>,
) -> Option<HttpPrincipal> {
    context
        .extensions
        .get::<Parts>()
        .and_then(|parts| parts.extensions.get::<HttpPrincipal>())
        .cloned()
}

pub(super) fn session_affinity_from_context(
    context: &RequestContext<RoleServer>,
) -> HttpSessionAffinity {
    context
        .extensions
        .get::<Parts>()
        .and_then(|parts| parts.extensions.get::<HttpSessionAffinity>())
        .copied()
        .unwrap_or(HttpSessionAffinity::Thread)
}

/// Stable conversation identity supplied by the HTTP transport. Legacy MCP
/// sessions use `Mcp-Session-Id`; reviewed gateways may provide the explicit
/// fallback header for modern stateless clients. The value is only hashed into
/// a durable session key and is never emitted in results or logs.
pub(super) fn conversation_identity_from_context(
    context: &RequestContext<RoleServer>,
) -> Option<String> {
    let parts = context.extensions.get::<Parts>()?;
    ["mcp-session-id", "x-winx-conversation-id"]
        .into_iter()
        .find_map(|name| parts.headers.get(name))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub(super) fn scope_tool_request(
    mut request: CallToolRequestParams,
    principal: Option<HttpPrincipal>,
    session_affinity: HttpSessionAffinity,
    conversation_identity: Option<&str>,
) -> Result<(CallToolRequestParams, RequestScope)> {
    let Some(principal) = principal else {
        return Ok((request, RequestScope::default()));
    };

    let mut scope = RequestScope { principal: Some(principal.clone()), replacements: Vec::new() };
    if !tool_uses_thread_id(request.name.as_ref()) {
        return Ok((request, scope));
    }

    let arguments = request.arguments.get_or_insert_default();
    let supplied = arguments.get("thread_id").and_then(Value::as_str).unwrap_or_default();
    let normalized = normalize_thread_id(supplied);
    let first_call = ToolKind::parse(request.name.as_ref()) == Some(ToolKind::Initialize)
        && arguments.get("type").and_then(Value::as_str).is_none_or(|kind| kind == "first_call");
    let external = if first_call {
        match session_affinity {
            HttpSessionAffinity::Workspace => affinity_thread_id(arguments, None),
            HttpSessionAffinity::Conversation => {
                let identity = conversation_identity
                    .filter(|identity| !identity.trim().is_empty())
                    .or_else(|| (!normalized.is_empty()).then_some(normalized.as_str()));
                affinity_thread_id(arguments, identity)
            }
            HttpSessionAffinity::Thread if normalized.is_empty() => generate_thread_id(),
            HttpSessionAffinity::Thread => normalized,
        }
    } else {
        normalized
    };
    let internal_source = if external.is_empty() { "anonymous" } else { external.as_str() };
    let internal = scoped_thread_id(&principal, internal_source)?;
    arguments.insert("thread_id".to_string(), Value::String(internal.clone()));
    scope.replacements.push((internal, external));
    Ok((request, scope))
}

pub(super) fn task_belongs_to_principal(task_id: &str, principal: Option<&HttpPrincipal>) -> bool {
    principal.is_none_or(|principal| task_id.starts_with(&principal.task_prefix()))
}

pub(super) fn new_task_id(principal: Option<&HttpPrincipal>, random: u128) -> String {
    match principal {
        Some(principal) => format!("{}{:032x}", principal.task_prefix(), random),
        None => format!("task_{random:032x}"),
    }
}

fn affinity_thread_id(
    arguments: &serde_json::Map<String, Value>,
    conversation_identity: Option<&str>,
) -> String {
    let workspace =
        arguments.get("any_workspace_path").and_then(Value::as_str).unwrap_or_default().trim();
    let resume =
        arguments.get("task_id_to_resume").and_then(Value::as_str).unwrap_or_default().trim();

    let (label, identity) = if workspace.is_empty() {
        if resume.is_empty() {
            ("scratch".to_string(), "scratch".to_string())
        } else {
            ("resume".to_string(), format!("resume:{resume}"))
        }
    } else {
        let path = canonical_workspace_identity(workspace);
        let label = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(workspace_label)
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| "workspace".to_string());
        (label, path.to_string_lossy().into_owned())
    };

    let (prefix, identity) = match conversation_identity {
        Some(conversation) => {
            ("cv", format!("{AFFINITY_ID_VERSION}:conversation:{conversation}:{identity}"))
        }
        None => ("ws", format!("{AFFINITY_ID_VERSION}:{identity}")),
    };
    let digest = Sha256::digest(identity.as_bytes());
    let mut suffix = String::with_capacity(16);
    for byte in &digest[..8] {
        let _ = write!(suffix, "{byte:02x}");
    }
    normalize_thread_id(&format!("{prefix}_{label}_{suffix}"))
}

/// Derive the external thread ID that a workspace-bound request must carry.
/// Caller-owned thread affinity intentionally has no derivable ID.
pub(super) fn expected_affinity_thread_id(
    workspace: &Path,
    affinity: HttpSessionAffinity,
    conversation_identity: Option<&str>,
) -> Option<String> {
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "any_workspace_path".to_string(),
        Value::String(workspace.to_string_lossy().into_owned()),
    );
    match affinity {
        HttpSessionAffinity::Workspace => Some(affinity_thread_id(&arguments, None)),
        HttpSessionAffinity::Conversation => {
            conversation_identity.map(|identity| affinity_thread_id(&arguments, Some(identity)))
        }
        HttpSessionAffinity::Thread => None,
    }
}

pub(super) fn canonical_workspace_identity(workspace: &str) -> PathBuf {
    let expanded = crate::utils::path::expand_user(workspace);
    let mut path = PathBuf::from(expanded);
    if !path.is_absolute() {
        path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(path);
    }
    if path.is_file() {
        path = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    }
    path.canonicalize().unwrap_or(path)
}

fn workspace_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .take(24)
        .collect()
}

fn tool_uses_thread_id(name: &str) -> bool {
    // Every registered Winx tool participates in the principal-scoped logical
    // session. Keep this derived from the registry so unknown extension tools
    // are never rewritten accidentally.
    ToolKind::parse(name).is_some()
}

fn scoped_thread_id(principal: &HttpPrincipal, external: &str) -> Result<String> {
    let prefix = principal.session_prefix();
    if prefix.len() >= MAX_NORMALIZED_THREAD_ID_BYTES {
        return Err(WinxError::ConfigurationError(
            "HTTP principal session prefix exceeds the thread-id limit".to_string(),
        ));
    }

    let available = MAX_NORMALIZED_THREAD_ID_BYTES - prefix.len();
    let compact = if external.len() <= available {
        external.to_string()
    } else {
        let prefix_budget = available.saturating_sub(SCOPED_HASH_SUFFIX_BYTES);
        let mut shortened = String::with_capacity(available);
        for character in external.chars() {
            if shortened.len() + character.len_utf8() > prefix_budget {
                break;
            }
            shortened.push(character);
        }
        let digest = Sha256::digest(external.as_bytes());
        shortened.push('_');
        for byte in &digest[..4] {
            let _ = write!(shortened, "{byte:02x}");
        }
        shortened
    };
    Ok(format!("{prefix}{compact}"))
}

fn unscope_json(value: &mut Value, replacements: &[(String, String)]) {
    match value {
        Value::String(text) => {
            for (internal, external) in replacements {
                *text = text.replace(internal, external);
            }
        }
        Value::Array(items) => {
            for item in items {
                unscope_json(item, replacements);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                unscope_json(item, replacements);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]
    use super::{new_task_id, scope_tool_request, task_belongs_to_principal};
    use crate::config::{HttpPrincipal, HttpSessionAffinity};
    use rmcp::model::CallToolRequestParams;
    use serde_json::{json, Value};

    fn principal(name: &str) -> HttpPrincipal {
        HttpPrincipal::new(name, "0123456789abcdef0123456789abcdef", false).expect("test principal")
    }

    #[test]
    fn same_external_thread_is_scoped_differently_per_principal() {
        let request = CallToolRequestParams::new("BashCommand").with_arguments(
            json!({"thread_id": "shared", "command": "pwd"}).as_object().cloned().unwrap(),
        );
        let (left, _) = scope_tool_request(
            request.clone(),
            Some(principal("left")),
            HttpSessionAffinity::Thread,
            None,
        )
        .expect("left");
        let (right, _) = scope_tool_request(
            request,
            Some(principal("right")),
            HttpSessionAffinity::Thread,
            None,
        )
        .expect("right");
        assert_ne!(left.arguments, right.arguments);
    }

    #[test]
    fn long_scoped_thread_stays_within_filename_limit() {
        let request = CallToolRequestParams::new("Initialize")
            .with_arguments(json!({"thread_id": "x".repeat(500)}).as_object().cloned().unwrap());
        let (request, _) = scope_tool_request(
            request,
            Some(principal("client")),
            HttpSessionAffinity::Thread,
            None,
        )
        .expect("scope");
        let id = request
            .arguments
            .and_then(|arguments| {
                arguments.get("thread_id").and_then(|value| value.as_str()).map(str::to_string)
            })
            .expect("thread id");
        assert!(id.len() <= crate::types::MAX_NORMALIZED_THREAD_ID_BYTES);
    }

    #[test]
    fn workspace_affinity_ignores_unstable_first_call_thread_ids() {
        let workspace = tempfile::tempdir().expect("workspace");
        let make = |thread_id: &str| {
            CallToolRequestParams::new("Initialize").with_arguments(
                json!({
                    "type": "first_call",
                    "any_workspace_path": workspace.path(),
                    "thread_id": thread_id
                })
                .as_object()
                .cloned()
                .unwrap(),
            )
        };
        let (first, _) = scope_tool_request(
            make("release_02333"),
            Some(principal("client")),
            HttpSessionAffinity::Workspace,
            None,
        )
        .expect("first");
        let (second, _) = scope_tool_request(
            make("release_0_2_333"),
            Some(principal("client")),
            HttpSessionAffinity::Workspace,
            None,
        )
        .expect("second");
        assert_eq!(first.arguments, second.arguments);
    }

    #[test]
    fn conversation_affinity_is_stable_within_and_distinct_across_conversations() {
        let workspace = tempfile::tempdir().expect("workspace");
        let make = |thread_id: &str| {
            CallToolRequestParams::new("Initialize").with_arguments(
                json!({
                    "type": "first_call",
                    "any_workspace_path": workspace.path(),
                    "thread_id": thread_id
                })
                .as_object()
                .cloned()
                .unwrap(),
            )
        };
        let scope = |request, conversation| {
            scope_tool_request(
                request,
                Some(principal("client")),
                HttpSessionAffinity::Conversation,
                Some(conversation),
            )
            .expect("conversation scope")
            .0
            .arguments
            .and_then(|arguments| {
                arguments.get("thread_id").and_then(Value::as_str).map(ToString::to_string)
            })
            .expect("thread id")
        };

        let first = scope(make("unstable_first"), "conversation-a");
        let resumed = scope(make("unstable_second"), "conversation-a");
        let parallel = scope(make("unstable_first"), "conversation-b");
        assert_eq!(first, resumed);
        assert_ne!(first, parallel);
        assert!(first.contains("__cv_"), "{first}");
    }

    #[test]
    fn conversation_affinity_falls_back_to_supplied_first_call_id() {
        let workspace = tempfile::tempdir().expect("workspace");
        let make = |thread_id: &str| {
            CallToolRequestParams::new("Initialize").with_arguments(
                json!({
                    "type": "first_call",
                    "any_workspace_path": workspace.path(),
                    "thread_id": thread_id
                })
                .as_object()
                .cloned()
                .unwrap(),
            )
        };
        let (left, _) = scope_tool_request(
            make("conversation_left"),
            Some(principal("client")),
            HttpSessionAffinity::Conversation,
            None,
        )
        .expect("left");
        let (right, _) = scope_tool_request(
            make("conversation_right"),
            Some(principal("client")),
            HttpSessionAffinity::Conversation,
            None,
        )
        .expect("right");
        assert_ne!(left.arguments, right.arguments);
    }

    #[test]
    fn task_ids_are_owned_by_the_authenticating_principal() {
        let left = principal("left");
        let right = principal("right");
        let id = new_task_id(Some(&left), 7);
        assert!(task_belongs_to_principal(&id, Some(&left)));
        assert!(!task_belongs_to_principal(&id, Some(&right)));
    }
}
