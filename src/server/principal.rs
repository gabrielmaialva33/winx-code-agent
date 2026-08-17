use std::fmt::Write as _;

use axum::http::request::Parts;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ErrorData as McpError;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::HttpPrincipal;
use crate::errors::{Result, WinxError};
use crate::state::bash_state::generate_thread_id;
use crate::types::{normalize_thread_id, MAX_NORMALIZED_THREAD_ID_BYTES};

const SCOPED_HASH_SUFFIX_BYTES: usize = 9;

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

pub(super) fn scope_tool_request(
    mut request: CallToolRequestParams,
    principal: Option<HttpPrincipal>,
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
    let external = if request.name == "Initialize" && normalized.is_empty() {
        generate_thread_id()
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

fn tool_uses_thread_id(name: &str) -> bool {
    matches!(
        name,
        "Initialize"
            | "BashCommand"
            | "ReadFiles"
            | "FileWriteOrEdit"
            | "MultiFileEdit"
            | "UndoEdit"
            | "ContextSave"
            | "ReadImage"
            | "CodeMap"
    )
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
    use crate::config::HttpPrincipal;
    use rmcp::model::CallToolRequestParams;
    use serde_json::json;

    fn principal(name: &str) -> HttpPrincipal {
        HttpPrincipal::new(name, "0123456789abcdef0123456789abcdef", false).expect("test principal")
    }

    #[test]
    fn same_external_thread_is_scoped_differently_per_principal() {
        let request = CallToolRequestParams::new("BashCommand").with_arguments(
            json!({"thread_id": "shared", "command": "pwd"}).as_object().cloned().unwrap(),
        );
        let (left, _) = scope_tool_request(request.clone(), Some(principal("left"))).expect("left");
        let (right, _) = scope_tool_request(request, Some(principal("right"))).expect("right");
        assert_ne!(left.arguments, right.arguments);
    }

    #[test]
    fn long_scoped_thread_stays_within_filename_limit() {
        let request = CallToolRequestParams::new("Initialize")
            .with_arguments(json!({"thread_id": "x".repeat(500)}).as_object().cloned().unwrap());
        let (request, _) = scope_tool_request(request, Some(principal("client"))).expect("scope");
        let id = request
            .arguments
            .and_then(|arguments| {
                arguments.get("thread_id").and_then(|value| value.as_str()).map(str::to_string)
            })
            .expect("thread id");
        assert!(id.len() <= crate::types::MAX_NORMALIZED_THREAD_ID_BYTES);
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
