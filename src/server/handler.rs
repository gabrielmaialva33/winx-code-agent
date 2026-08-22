use std::fmt::Write as _;

use axum::http::request::Parts;
use rmcp::{
    model::{
        CacheScope, CallToolRequestParams, CallToolResponse, CancelTaskParams,
        GetPromptRequestParams, GetPromptResponse, GetPromptResult, GetTaskParams, GetTaskResult,
        Icon, Implementation, ListPromptsResult, ListResourcesResult, ListToolsResult,
        PaginatedRequestParams, PromptMessage, ProtocolVersion, ReadResourceRequestParams,
        ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, Role,
        ServerCapabilities, ServerInfo, TaskStatus, Tool, UpdateTaskParams,
    },
    service::{NotificationContext, RequestContext, RoleServer},
    ErrorData as McpError, ServerHandler,
};
use sha2::{Digest, Sha256};

use super::catalog::{server_icon_data_uri, winx_prompts, winx_tools};
use super::outcomes;
use super::principal::{
    conversation_identity_from_context, principal_from_context, scope_tool_request,
    session_affinity_from_context, task_belongs_to_principal, RequestScope,
};
use super::WinxService;

/// One structured `winx::usage` event per tool call. Only metadata is logged —
/// never tool arguments, command text, or file contents (secrets/PII).
struct UsageEvent {
    tool: String,
    action: String,
    ws: String,
    principal: String,
    thread_id: String,
    request_id: String,
    client_name: String,
    client_version: String,
    protocol: String,
    client_session: String,
    started: std::time::Instant,
}

impl UsageEvent {
    fn start(
        request: &CallToolRequestParams,
        scope: &RequestScope,
        context: &RequestContext<RoleServer>,
    ) -> Self {
        let arguments = request.arguments.as_ref();
        let thread_id = arguments
            .and_then(|arguments| arguments.get("thread_id"))
            .and_then(serde_json::Value::as_str)
            .map(|thread_id| scope.unscope_text(thread_id))
            .unwrap_or_default();
        let action =
            if request.name == "BashCommand" { bash_action(arguments) } else { String::new() };
        let ws = if request.name == "Initialize" {
            arguments
                .and_then(|arguments| arguments.get("any_workspace_path"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        } else {
            String::new()
        };
        let client = context.client_info();
        let client_name =
            client.as_ref().map_or("unknown", |client| client.name.as_str()).to_string();
        let client_version =
            client.as_ref().map_or("unknown", |client| client.version.as_str()).to_string();
        let protocol = context
            .protocol_version()
            .map_or_else(|| "unknown".to_string(), |version| version.to_string());
        let request_id = context
            .extensions
            .get::<Parts>()
            .and_then(|parts| parts.extensions.get::<crate::http_server::RequestCorrelation>())
            .map_or_else(
                || {
                    serde_json::to_string(&context.id)
                        .map_or_else(|_| "unknown".to_string(), |id| short_fingerprint("r", &id))
                },
                |correlation| correlation.as_str().to_string(),
            );
        let client_session = context
            .extensions
            .get::<Parts>()
            .and_then(|parts| parts.headers.get("mcp-session-id"))
            .and_then(|value| value.to_str().ok())
            .map_or_else(|| "stateless".to_string(), |id| short_fingerprint("s", id));
        Self {
            tool: request.name.to_string(),
            action,
            ws,
            principal: scope
                .principal()
                .map_or_else(|| "local".to_string(), |principal| principal.name().to_string()),
            thread_id,
            request_id,
            client_name,
            client_version,
            protocol,
            client_session,
            started: std::time::Instant::now(),
        }
    }

    fn emit(&self, outcome: &str, result_status: &str, response_bytes: usize) {
        tracing::info!(
            target: "winx::usage",
            event = "tool_call",
            tool = %self.tool,
            action = %self.action,
            ws = %self.ws,
            principal = %self.principal,
            thread_id = %self.thread_id,
            request_id = %self.request_id,
            client_name = %self.client_name,
            client_version = %self.client_version,
            protocol = %self.protocol,
            client_session = %self.client_session,
            result_status,
            response_bytes,
            duration_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            outcome,
            "tool call"
        );
    }
}

fn short_fingerprint(prefix: &str, value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut output = format!("{prefix}_");
    for byte in &digest[..8] {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// Emit the cache fields required by MCP 2026-07-28 without changing the wire
/// shape for clients that negotiated an older protocol revision.
fn cache_hints_for_protocol(
    protocol_version: Option<ProtocolVersion>,
    cache_scope: CacheScope,
) -> (Option<u64>, Option<CacheScope>) {
    if protocol_version.is_some_and(|version| version >= ProtocolVersion::V_2026_07_28) {
        (Some(0), Some(cache_scope))
    } else {
        (None, None)
    }
}

/// Classify a `BashCommand` call by action kind (`command`, `status_check`,
/// `send_text`, ...) without touching the command text itself. Mirrors the
/// lenient forms accepted by the `BashCommand` deserializer: a typed
/// `action_json` object, a JSON-encoded string, a bare command string, or
/// legacy shorthand keys at the top level.
fn bash_action(arguments: Option<&rmcp::model::JsonObject>) -> String {
    use serde_json::Value;
    const KINDS: [&str; 7] = [
        "command",
        "status_check",
        "send_text",
        "send_specials",
        "send_ascii",
        "screen",
        "wait_for_turn",
    ];
    let Some(arguments) = arguments else {
        return String::new();
    };
    let parsed;
    let action = match arguments.get("action_json") {
        Some(Value::Object(object)) => object,
        Some(Value::String(text)) => {
            match serde_json::from_str::<Value>(&text.replace('\n', " ")) {
                Ok(Value::Object(object)) => {
                    parsed = object;
                    &parsed
                }
                // A non-object string is treated as a bare command downstream.
                _ => return "command".to_string(),
            }
        }
        // Legacy shorthand: the argument object itself is the action.
        _ => arguments,
    };
    if let Some(kind) = action.get("type").and_then(Value::as_str) {
        return kind.to_string();
    }
    KINDS.iter().find(|kind| action.contains_key(**kind)).map_or("?", |kind| kind).to_string()
}

#[allow(clippy::unused_async_trait_impl)] // rmcp's trait requires these async signatures
impl ServerHandler for WinxService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .enable_tasks()
                .build(),
        )
        .with_server_info(
            Implementation::new("winx-mcp-server", self.version.clone())
                .with_title("Winx High-Performance MCP")
                .with_icons(vec![Icon::new(server_icon_data_uri())
                    .with_mime_type("image/png")
                    .with_sizes(vec!["96x96".to_string()])]),
        )
        .with_protocol_version(ProtocolVersion::V_2026_07_28)
        .with_instructions(crate::utils::orchestration::server_instructions())
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        self.initialize_from_client_roots(context).await;
    }

    async fn on_roots_list_changed(&self, context: NotificationContext<RoleServer>) {
        // A changed Roots list is only used as a retry if Winx is still
        // uninitialized. Never switch an active shell's workspace implicitly.
        self.initialize_from_client_roots(context).await;
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        winx_tools().into_iter().find(|tool| tool.name == name)
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, McpError> {
        let principal = principal_from_context(&context);
        if !task_belongs_to_principal(&request.task_id, principal.as_ref()) {
            return Err(McpError::invalid_request(
                format!("Unknown or expired task: {}", request.task_id),
                None,
            ));
        }
        let mut tasks = self.tasks.lock().await;
        let entry = tasks.get(&request.task_id).ok_or_else(|| {
            McpError::invalid_request(format!("Unknown or expired task: {}", request.task_id), None)
        })?;
        Ok(GetTaskResult::new(entry.detailed()))
    }

    async fn update_task(
        &self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        let principal = principal_from_context(&context);
        if !task_belongs_to_principal(&request.task_id, principal.as_ref()) {
            return Err(McpError::invalid_request(
                format!("Unknown or expired task: {}", request.task_id),
                None,
            ));
        }
        let mut tasks = self.tasks.lock().await;
        tasks.get(&request.task_id).ok_or_else(|| {
            McpError::invalid_request(format!("Unknown or expired task: {}", request.task_id), None)
        })?;
        Ok(())
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        let principal = principal_from_context(&context);
        if !task_belongs_to_principal(&request.task_id, principal.as_ref()) {
            return Err(McpError::invalid_request(
                format!("Unknown or expired task: {}", request.task_id),
                None,
            ));
        }
        let (abort_handle, thread_id) = {
            let mut tasks = self.tasks.lock().await;
            let entry = tasks.get_mut(&request.task_id).ok_or_else(|| {
                McpError::invalid_request(
                    format!("Unknown or expired task: {}", request.task_id),
                    None,
                )
            })?;
            if entry.task.status != TaskStatus::Working
                && entry.task.status != TaskStatus::InputRequired
            {
                return Err(McpError::invalid_request(
                    format!("Task {} is already terminal", request.task_id),
                    None,
                ));
            }
            let abort_handle = entry.abort_handle.take();
            let thread_id = entry.thread_id.clone();
            entry.finish(TaskStatus::Cancelled, Some("Cancelled by client".to_string()), None);
            (abort_handle, thread_id)
        };
        if let Some(abort_handle) = abort_handle {
            abort_handle.abort();
        }
        self.interrupt_task_thread(&thread_id).await;
        Ok(())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let (ttl_ms, cache_scope) =
            cache_hints_for_protocol(ctx.protocol_version(), CacheScope::Public);
        let mut result = ListToolsResult::with_all_items(winx_tools());
        result.ttl_ms = ttl_ms;
        result.cache_scope = cache_scope;
        Ok(result)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let (ttl_ms, cache_scope) =
            cache_hints_for_protocol(ctx.protocol_version(), CacheScope::Public);
        let mut result =
            ListResourcesResult::with_all_items(vec![Resource::new("file://readme", "README")
                .with_description("Project README documentation")
                .with_mime_type("text/markdown")]);
        result.ttl_ms = ttl_ms;
        result.cache_scope = cache_scope;
        Ok(result)
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let (ttl_ms, cache_scope) =
            cache_hints_for_protocol(ctx.protocol_version(), CacheScope::Public);
        let mut result = ListPromptsResult::with_all_items(winx_prompts());
        result.ttl_ms = ttl_ms;
        result.cache_scope = cache_scope;
        Ok(result)
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        if request.name != "KnowledgeTransfer" {
            return Err(McpError::invalid_request(
                format!("Unknown prompt: {}", request.name),
                None,
            ));
        }

        let principal = principal_from_context(&context);
        let session_prefix = principal.as_ref().map(crate::config::HttpPrincipal::session_prefix);
        let text = crate::utils::redact::redact(
            &self.knowledge_transfer_prompt_text(session_prefix.as_deref()).await,
        )
        .into_owned();
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)])
            .with_description("Knowledge transfer handoff prompt")
            .into())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let content = match request.uri.as_ref() {
            "file://readme" => match tokio::fs::read_to_string("README.md").await {
                Ok(content) => vec![ResourceContents::text(
                    crate::utils::redact::redact(&content).into_owned(),
                    request.uri.clone(),
                )],
                Err(_) => vec![ResourceContents::text(
                    "README.md not found".to_string(),
                    request.uri.clone(),
                )],
            },
            _ => {
                return Err(McpError::invalid_request(
                    format!("Unknown resource URI: {}", request.uri),
                    None,
                ));
            }
        };
        let (ttl_ms, cache_scope) =
            cache_hints_for_protocol(ctx.protocol_version(), CacheScope::Private);
        let mut result = ReadResourceResult::new(content);
        result.ttl_ms = ttl_ms;
        result.cache_scope = cache_scope;
        Ok(result.into())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let principal = principal_from_context(&context);
        let affinity = session_affinity_from_context(&context);
        let conversation_identity = conversation_identity_from_context(&context);
        let (request, scope) =
            scope_tool_request(request, principal, affinity, conversation_identity.as_deref())
                .map_err(|error| {
                    McpError::invalid_request(format!("Cannot scope remote request: {error}"), None)
                })?;
        let usage = UsageEvent::start(&request, &scope, &context);
        if context.client_capabilities().is_some_and(|capabilities| capabilities.supports_tasks())
            && Self::bash_task_is_eligible(&request)
        {
            return match self.enqueue_bash_task(request, scope).await {
                Ok(task) => {
                    usage.emit("task", "working", 0);
                    Ok(CallToolResponse::Task(task))
                }
                Err(error) => {
                    usage.emit("protocol_error", "failed", 0);
                    Err(error)
                }
            };
        }

        match self.execute_tool_call(request).await {
            Ok(mut result) => {
                scope.unscope_result(&mut result);
                let status = outcomes::result_status(&result);
                let outcome = if result.is_error == Some(true) { "tool_error" } else { "ok" };
                usage.emit(outcome, &status, outcomes::result_size_bytes(&result));
                Ok(result.into())
            }
            Err(mut error) => {
                scope.unscope_error(&mut error);
                usage.emit("protocol_error", "failed", 0);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hints_follow_negotiated_protocol_version() {
        assert_eq!(
            cache_hints_for_protocol(Some(ProtocolVersion::V_2026_07_28), CacheScope::Private,),
            (Some(0), Some(CacheScope::Private))
        );
        assert_eq!(
            cache_hints_for_protocol(Some(ProtocolVersion::V_2026_07_28), CacheScope::Public),
            (Some(0), Some(CacheScope::Public))
        );
        assert_eq!(
            cache_hints_for_protocol(Some(ProtocolVersion::V_2025_11_25), CacheScope::Private,),
            (None, None)
        );
        assert_eq!(cache_hints_for_protocol(None, CacheScope::Private), (None, None));
    }
}
