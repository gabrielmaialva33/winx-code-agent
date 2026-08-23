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
use crate::runtime::ShellActionOptions;
use crate::types::{BashCommand, BashWaitPolicy};

pub(crate) const COMPACT_BASH_OUTPUT_EXTENSION: &str = "io.winx/compact-bash-output";
const ADAPTIVE_TASK_INLINE_SECONDS: f32 = 2.0;
const RETURN_EARLY_DEFAULT_SECONDS: f32 = 0.25;
const RETURN_EARLY_MAX_SECONDS: f32 = 5.0;
const SYNCHRONOUS_MAX_SECONDS: f32 = 60.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BashTaskRoute {
    Synchronous,
    Adaptive,
    Immediate,
}

fn bash_task_route(
    client_tasks: bool,
    generation_bound_runtime: bool,
    eligible: bool,
    policy: BashWaitPolicy,
) -> BashTaskRoute {
    if !client_tasks || !generation_bound_runtime || !eligible {
        return BashTaskRoute::Synchronous;
    }
    match policy {
        BashWaitPolicy::Adaptive => BashTaskRoute::Adaptive,
        BashWaitPolicy::UntilComplete => BashTaskRoute::Immediate,
        BashWaitPolicy::ReturnEarly => BashTaskRoute::Synchronous,
    }
}

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

fn supports_compact_bash_output(context: &RequestContext<RoleServer>) -> bool {
    context.client_capabilities().is_some_and(|capabilities| {
        capabilities
            .extensions
            .as_ref()
            .is_some_and(|extensions| extensions.contains_key(COMPACT_BASH_OUTPUT_EXTENSION))
    })
}

fn normalized_wait_request(
    mut request: CallToolRequestParams,
    policy: BashWaitPolicy,
    task_inline: bool,
) -> Result<CallToolRequestParams, McpError> {
    let mut arguments = request
        .arguments
        .clone()
        .ok_or_else(|| McpError::invalid_request("Missing BashCommand arguments", None))?;
    let requested = arguments
        .get("wait_for_seconds")
        .cloned()
        .map(serde_json::from_value::<f32>)
        .transpose()
        .map_err(|error| {
            McpError::invalid_request(format!("Invalid wait_for_seconds: {error}"), None)
        })?;
    let (default, maximum) = match (policy, task_inline) {
        (BashWaitPolicy::Adaptive, true) => {
            (ADAPTIVE_TASK_INLINE_SECONDS, ADAPTIVE_TASK_INLINE_SECONDS)
        }
        (BashWaitPolicy::ReturnEarly, _) => {
            (RETURN_EARLY_DEFAULT_SECONDS, RETURN_EARLY_MAX_SECONDS)
        }
        (BashWaitPolicy::Adaptive, false) => (15.0, SYNCHRONOUS_MAX_SECONDS),
        (BashWaitPolicy::UntilComplete, false) => {
            (SYNCHRONOUS_MAX_SECONDS, SYNCHRONOUS_MAX_SECONDS)
        }
        (BashWaitPolicy::UntilComplete, true) => unreachable!("until_complete starts a Task"),
    };
    arguments.insert(
        "wait_for_seconds".to_string(),
        serde_json::Value::from(requested.unwrap_or(default).clamp(0.0, maximum)),
    );
    request.arguments = Some(arguments);
    Ok(request)
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
        let mut capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_prompts()
            .enable_tasks()
            .build();
        capabilities
            .extensions
            .get_or_insert_default()
            .insert(COMPACT_BASH_OUTPUT_EXTENSION.to_string(), rmcp::model::JsonObject::new());
        ServerInfo::new(capabilities)
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
        let (abort_handle, thread_id, mut execution_token, execution_control) = {
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
            entry.request_cancel();
            let thread_id = entry.thread_id.clone();
            let execution_token = entry.execution_token();
            let execution_control = entry.execution_control();
            // Before a generation exists the worker must stay alive long
            // enough to publish it and interrupt that exact process. Once it
            // exists, aborting the polling worker is safe.
            let abort_handle = execution_token.as_ref().and(entry.abort_handle.take());
            entry.finish(TaskStatus::Cancelled, Some("Cancelled by client".to_string()), None);
            (abort_handle, thread_id, execution_token, execution_control)
        };
        if let Some(abort_handle) = abort_handle {
            abort_handle.abort();
        }
        if execution_token.is_none() {
            execution_token = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                execution_control.wait_for_execution(),
            )
            .await
            .ok()
            .flatten();
        }
        self.interrupt_task_execution(&thread_id, execution_token).await;
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

    #[allow(clippy::too_many_lines)] // request scoping, negotiated routing, execution, and telemetry
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
        let compact_bash_output = supports_compact_bash_output(&context);
        let supports_tasks =
            context.client_capabilities().is_some_and(|capabilities| capabilities.supports_tasks());
        let wait_policy = if request.name == "BashCommand" {
            Some(Self::bash_wait_policy(&request)?)
        } else {
            None
        };
        if wait_policy == Some(BashWaitPolicy::UntilComplete) {
            let is_foreground_command = request
                .arguments
                .clone()
                .and_then(|arguments| {
                    serde_json::from_value::<BashCommand>(serde_json::Value::Object(arguments)).ok()
                })
                .is_some_and(|bash| {
                    matches!(
                        bash.action_json,
                        crate::types::BashCommandAction::Command { is_background: false, .. }
                    )
                });
            if !is_foreground_command {
                return Err(McpError::invalid_request(
                    "wait_policy=until_complete is valid only for a foreground Command action",
                    None,
                ));
            }
        }

        let eligible = Self::bash_task_is_eligible(&request);
        let generation_bound_runtime = if supports_tasks && eligible {
            let thread_id = request
                .arguments
                .as_ref()
                .and_then(|arguments| arguments.get("thread_id"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let (slot, session_guard) = self.session_for(thread_id).await;
            let supported = self
                .shell_runtime
                .supports_generation_bound_actions_for(&slot)
                .await
                .unwrap_or(false);
            drop(session_guard);
            supported
        } else {
            false
        };
        let route = wait_policy.map_or(BashTaskRoute::Synchronous, |policy| {
            bash_task_route(supports_tasks, generation_bound_runtime, eligible, policy)
        });

        match route {
            BashTaskRoute::Immediate => {
                let reservation = self.reserve_bash_task(&request, &scope).await?;
                return match self
                    .start_reserved_bash_task(
                        reservation,
                        request,
                        scope,
                        None,
                        compact_bash_output,
                    )
                    .await
                {
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
            BashTaskRoute::Adaptive => {
                let reservation = self.reserve_bash_task(&request, &scope).await?;
                let task_request = request.clone();
                let inline_request =
                    normalized_wait_request(request, BashWaitPolicy::Adaptive, true)?;
                match self
                    .execute_tool_call(
                        inline_request,
                        ShellActionOptions {
                            compact_output: compact_bash_output,
                            ..ShellActionOptions::default()
                        },
                    )
                    .await
                {
                    Ok(execution) if outcomes::result_status(&execution.result) == "running" => {
                        return match self
                            .start_reserved_bash_task(
                                reservation,
                                task_request,
                                scope,
                                Some(execution),
                                compact_bash_output,
                            )
                            .await
                        {
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
                    Ok(mut execution) => {
                        self.release_bash_task(&reservation).await;
                        scope.unscope_result(&mut execution.result);
                        let status = outcomes::result_status(&execution.result);
                        let outcome = if execution.result.is_error == Some(true) {
                            "tool_error"
                        } else {
                            "ok"
                        };
                        usage.emit(
                            outcome,
                            &status,
                            outcomes::result_size_bytes(&execution.result),
                        );
                        return Ok(execution.result.into());
                    }
                    Err(mut error) => {
                        self.release_bash_task(&reservation).await;
                        scope.unscope_error(&mut error);
                        usage.emit("protocol_error", "failed", 0);
                        return Err(error);
                    }
                }
            }
            BashTaskRoute::Synchronous => {}
        }

        let request = match wait_policy {
            Some(policy) => normalized_wait_request(request, policy, false)?,
            None => request,
        };
        match self
            .execute_tool_call(
                request,
                ShellActionOptions {
                    compact_output: compact_bash_output,
                    ..ShellActionOptions::default()
                },
            )
            .await
        {
            Ok(mut execution) => {
                let result = &mut execution.result;
                scope.unscope_result(result);
                let status = outcomes::result_status(result);
                let outcome = if result.is_error == Some(true) { "tool_error" } else { "ok" };
                usage.emit(outcome, &status, outcomes::result_size_bytes(result));
                Ok(execution.result.into())
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

    #[test]
    fn guardian_1_4_uses_synchronous_policy_fallback_even_for_task_clients() {
        assert_eq!(
            bash_task_route(true, false, true, BashWaitPolicy::Adaptive),
            BashTaskRoute::Synchronous
        );
        assert_eq!(
            bash_task_route(true, false, true, BashWaitPolicy::UntilComplete),
            BashTaskRoute::Synchronous
        );
    }

    #[test]
    fn task_routing_requires_client_runtime_and_foreground_eligibility() {
        assert_eq!(
            bash_task_route(true, true, true, BashWaitPolicy::Adaptive),
            BashTaskRoute::Adaptive
        );
        assert_eq!(
            bash_task_route(true, true, true, BashWaitPolicy::UntilComplete),
            BashTaskRoute::Immediate
        );
        assert_eq!(
            bash_task_route(true, true, true, BashWaitPolicy::ReturnEarly),
            BashTaskRoute::Synchronous
        );
        assert_eq!(
            bash_task_route(false, true, true, BashWaitPolicy::UntilComplete),
            BashTaskRoute::Synchronous
        );
        assert_eq!(
            bash_task_route(true, true, false, BashWaitPolicy::UntilComplete),
            BashTaskRoute::Synchronous
        );
    }

    #[test]
    #[allow(clippy::expect_used, clippy::float_cmp)]
    fn synchronous_wait_policies_have_explicit_upper_bounds() {
        let request = |wait_for_seconds: Option<f32>| {
            let mut arguments = serde_json::Map::new();
            arguments.insert("command".to_string(), serde_json::Value::String("pwd".to_string()));
            if let Some(wait) = wait_for_seconds {
                arguments.insert("wait_for_seconds".to_string(), serde_json::json!(wait));
            }
            CallToolRequestParams::new("BashCommand").with_arguments(arguments)
        };
        let normalized = |policy, wait| {
            normalized_wait_request(request(wait), policy, false)
                .expect("valid wait request")
                .arguments
                .and_then(|arguments| arguments.get("wait_for_seconds").cloned())
                .and_then(|value| value.as_f64())
                .expect("normalized wait")
        };

        assert_eq!(normalized(BashWaitPolicy::Adaptive, Some(500.0)), 60.0);
        assert_eq!(normalized(BashWaitPolicy::UntilComplete, None), 60.0);
        assert_eq!(normalized(BashWaitPolicy::ReturnEarly, Some(500.0)), 5.0);
        assert_eq!(normalized(BashWaitPolicy::ReturnEarly, None), 0.25);
    }
}
