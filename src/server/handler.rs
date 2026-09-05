use rmcp::{
    model::{
        CacheScope, CallToolRequestParams, CallToolResponse, CancelTaskParams,
        GetPromptRequestParams, GetPromptResponse, GetPromptResult, GetTaskParams, GetTaskResult,
        Icon, Implementation, ListPromptsResult, ListResourcesResult, ListToolsResult,
        PaginatedRequestParams, PromptMessage, ProtocolVersion, ReadResourceRequestParams,
        ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, Role,
        ServerCapabilities, ServerInfo, Tool, UpdateTaskParams,
    },
    service::{NotificationContext, RequestContext, RoleServer},
    ErrorData as McpError, ServerHandler,
};

use super::catalog::{server_icon_data_uri, winx_prompts, winx_tools, winx_tools_for_policy};
use super::outcomes;
use super::principal::{
    conversation_identity_from_context, principal_from_context, scope_tool_request,
    session_affinity_from_context, task_belongs_to_principal,
};
use super::usage::UsageEvent;
use super::WinxService;
use crate::runtime::ShellActionOptions;
use crate::tool_policy::ToolPolicy;
use crate::tool_registry::ToolKind;
use crate::tools::edit_files::EditSurface;
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

fn adaptive_action_options(compact_output: bool) -> ShellActionOptions {
    ShellActionOptions {
        compact_output,
        // Adaptive promotion is already backed by a Task reservation. Require
        // the effective guardian epoch to be checked live before this first
        // action so a recreated guardian cannot launch an untracked process
        // under a stale adapter cache.
        require_generation_binding: true,
        ..ShellActionOptions::default()
    }
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
        self.cancel_bash_task(&request.task_id).await
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let principal = principal_from_context(&ctx);
        let (tools, scope) = principal.as_ref().map_or_else(
            || (winx_tools(), CacheScope::Public),
            |principal| {
                let policy = principal.tool_policy();
                let scope = if policy == crate::tool_policy::ToolPolicy::default() {
                    CacheScope::Public
                } else {
                    CacheScope::Private
                };
                (winx_tools_for_policy(policy), scope)
            },
        );
        let (ttl_ms, cache_scope) = cache_hints_for_protocol(ctx.protocol_version(), scope);
        let mut result = ListToolsResult::with_all_items(tools);
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
        let tool_kind = ToolKind::parse(request.name.as_ref());
        if tool_kind.is_none() {
            return Err(McpError::invalid_request(format!("Unknown tool: {}", request.name), None));
        }
        let principal = principal_from_context(&context);
        let effective_tool_policy = principal
            .as_ref()
            .map_or_else(ToolPolicy::default, super::super::config::HttpPrincipal::tool_policy);

        // Parse edit arguments once into the typed domain before any capability
        // decision. This avoids policy decisions based on loosely typed/raw
        // JSON fields and makes malformed edit calls recoverable tool results,
        // even when another gate (workspace or capability) would also reject
        // the request.
        let edit_surface = tool_kind.and_then(EditSurface::from_public_tool);
        let normalized_edit = if let Some(surface) = edit_surface {
            let arguments = request
                .arguments
                .clone()
                .map_or(serde_json::Value::Null, serde_json::Value::Object);
            match crate::tools::edit_files::normalize_edit_call(surface, arguments.clone()) {
                Ok(normalized) => Some(normalized),
                Err(error) => {
                    let mut result =
                        outcomes::tool_failure(request.name.as_ref(), &error, Some(&arguments))?;
                    outcomes::enforce_next_action_policy(&mut result, effective_tool_policy);
                outcomes::finalize_tool_result(&mut result);
                    return Ok(result.into());
                }
            }
        } else {
            None
        };
        let policy_allows_call =
            tool_kind.is_some_and(|kind| effective_tool_policy.allows_call_kind(kind));
        if !policy_allows_call {
            return Err(McpError::invalid_request(
                format!("Tool is not available for this principal: {}", request.name),
                None,
            ));
        }
        if normalized_edit.as_ref().is_some_and(|edit| edit.verification.is_some())
            && !effective_tool_policy.edit_permissions().allows_verification()
        {
            return Err(McpError::invalid_request(
                format!("{} verification requires BashCommand authority", request.name),
                None,
            ));
        }
        let affinity = session_affinity_from_context(&context);
        let conversation_identity = conversation_identity_from_context(&context);
        let (request, scope) =
            scope_tool_request(request, principal, affinity, conversation_identity.as_deref())
                .map_err(|error| {
                    McpError::invalid_request(format!("Cannot scope remote request: {error}"), None)
                })?;
        let mut usage = UsageEvent::start(&request, &scope, &context);
        match self
            .validate_workspace_coherence(
                &request,
                &scope,
                affinity,
                conversation_identity.as_deref(),
            )
            .await
        {
            Ok(coherence) => usage.set_workspace_coherence(coherence.as_str()),
            Err(error) => {
                usage.set_workspace_coherence("rejected");
                let arguments = request.arguments.clone().map(serde_json::Value::Object);
                let mut result =
                    outcomes::tool_failure(request.name.as_ref(), &error, arguments.as_ref())?;
                outcomes::enforce_next_action_policy(&mut result, effective_tool_policy);
                outcomes::finalize_tool_result(&mut result);
                scope.unscope_result(&mut result);
                let status = outcomes::result_status(&result);
                usage.emit(
                    "tool_error",
                    &status,
                    outcomes::result_size_bytes(&result),
                    Some(&result),
                );
                return Ok(result.into());
            }
        }
        let compact_bash_output = supports_compact_bash_output(&context);
        let supports_tasks =
            context.client_capabilities().is_some_and(|capabilities| capabilities.supports_tasks());
        let wait_policy = if tool_kind == Some(ToolKind::BashCommand) {
            Some(Self::bash_wait_policy(&request)?)
        } else {
            None
        };
        if wait_policy == Some(BashWaitPolicy::UntilComplete) {
            let parsed_bash = request.arguments.clone().and_then(|arguments| {
                serde_json::from_value::<BashCommand>(serde_json::Value::Object(arguments)).ok()
            });
            if let Some(bash) = parsed_bash.as_ref() {
                let is_foreground_command = matches!(
                    bash.action_json,
                    crate::types::BashCommandAction::Command { is_background: false, .. }
                );
                if !is_foreground_command {
                    let action = match &bash.action_json {
                        crate::types::BashCommandAction::Command {
                            is_background: true, ..
                        } => "background_command",
                        crate::types::BashCommandAction::Command { .. } => "foreground_command",
                        crate::types::BashCommandAction::StatusCheck { .. } => "status_check",
                        crate::types::BashCommandAction::SendText { .. } => "send_text",
                        crate::types::BashCommandAction::SendSpecials { .. } => "send_specials",
                        crate::types::BashCommandAction::SendAscii { .. } => "send_ascii",
                        crate::types::BashCommandAction::Screen { .. } => "screen",
                        crate::types::BashCommandAction::WaitForTurn { .. } => "wait_for_turn",
                    };
                    let error = crate::errors::WinxError::InvalidWaitPolicyForAction {
                        wait_policy: "until_complete".to_string(),
                        action: action.to_string(),
                    };
                    let arguments = request.arguments.clone().map(serde_json::Value::Object);
                    let mut result =
                        outcomes::tool_failure("BashCommand", &error, arguments.as_ref())?;
                    outcomes::enforce_next_action_policy(&mut result, effective_tool_policy);
                outcomes::finalize_tool_result(&mut result);
                    scope.unscope_result(&mut result);
                    let status = outcomes::result_status(&result);
                    usage.emit(
                        "tool_error",
                        &status,
                        outcomes::result_size_bytes(&result),
                        Some(&result),
                    );
                    return Ok(result.into());
                }
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
            let (slot, session_guard) = self.tool_session_for(thread_id).await;
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
                        usage.emit("task", "working", 0, None);
                        Ok(CallToolResponse::Task(task))
                    }
                    Err(error) => {
                        usage.emit("protocol_error", "failed", 0, None);
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
                        adaptive_action_options(compact_bash_output),
                        effective_tool_policy.edit_permissions(),
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
                                usage.emit("task", "working", 0, None);
                                Ok(CallToolResponse::Task(task))
                            }
                            Err(error) => {
                                usage.emit("protocol_error", "failed", 0, None);
                                Err(error)
                            }
                        };
                    }
                    Ok(mut execution) => {
                        self.release_bash_task(&reservation).await;
                        outcomes::enforce_next_action_policy(
                            &mut execution.result,
                            effective_tool_policy,
                        );
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
                            Some(&execution.result),
                        );
                        return Ok(execution.result.into());
                    }
                    Err(mut error) => {
                        self.release_bash_task(&reservation).await;
                        scope.unscope_error(&mut error);
                        usage.emit("protocol_error", "failed", 0, None);
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
                effective_tool_policy.edit_permissions(),
            )
            .await
        {
            Ok(mut execution) => {
                let result = &mut execution.result;
                outcomes::enforce_next_action_policy(result, effective_tool_policy);
                scope.unscope_result(result);
                let status = outcomes::result_status(result);
                let outcome = if result.is_error == Some(true) { "tool_error" } else { "ok" };
                usage.emit(outcome, &status, outcomes::result_size_bytes(result), Some(result));
                Ok(execution.result.into())
            }
            Err(mut error) => {
                scope.unscope_error(&mut error);
                usage.emit("protocol_error", "failed", 0, None);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::usage::usage_result_metadata;
    use super::*;
    use rmcp::model::CallToolResult;

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
    #[allow(clippy::expect_used)]
    fn receipt_bound_verify_is_not_task_promotable() {
        let arguments = serde_json::json!({
            "action_json": {
                "type": "verify",
                "verification_id": "verify_123",
                "command": "cargo test"
            },
            "thread_id": "thread"
        });
        let request = CallToolRequestParams::new("BashCommand")
            .with_arguments(arguments.as_object().expect("arguments object").clone());
        assert!(!WinxService::bash_task_is_eligible(&request));
    }

    #[test]
    fn adaptive_action_requires_live_generation_binding() {
        let options = adaptive_action_options(true);
        assert!(options.compact_output);
        assert!(options.require_generation_binding);
    }

    #[test]
    fn usage_metadata_reads_image_code_map_and_temp_envelopes() {
        let result_with = |structured| {
            let mut result = CallToolResult::success(Vec::new());
            result.structured_content = Some(structured);
            result
        };
        let image = result_with(serde_json::json!({
            "data": {"result": {
                "source_bytes": 17_000_000,
                "delivered_bytes": 2_000_000,
                "transcoded": true,
                "deduplicated": false
            }}
        }));
        let metadata = usage_result_metadata(Some(&image));
        assert_eq!(metadata.source_bytes, 17_000_000);
        assert_eq!(metadata.payload_bytes, 2_000_000);
        assert!(metadata.image_transcoded);
        assert!(!metadata.image_deduplicated);

        let code_map = result_with(serde_json::json!({
            "source_kind": "canonical",
            "payload_bytes": 12_345
        }));
        let metadata = usage_result_metadata(Some(&code_map));
        assert_eq!(metadata.source_kind, "canonical");
        assert_eq!(metadata.payload_bytes, 12_345);

        let temporary = result_with(serde_json::json!({
            "data": {
                "session_files": 129,
                "session_bytes": 4096,
                "stale_pruned_files": 32,
                "stale_pruned_bytes": 2048,
                "temporary_artifact_cleanup_required": true
            }
        }));
        let metadata = usage_result_metadata(Some(&temporary));
        assert_eq!(metadata.temporary_session_files, 129);
        assert_eq!(metadata.temporary_session_bytes, 4096);
        assert_eq!(metadata.temporary_stale_pruned_files, 32);
        assert_eq!(metadata.temporary_stale_pruned_bytes, 2048);
        assert!(metadata.temporary_over_budget);

        let edit_follow_up = result_with(serde_json::json!({
            "errorCode": "verification_failed",
            "retrySameCall": false,
            "nextAction": {"tool": "VerifyEdit"},
            "requiredReads": [],
            "data": {
                "edit_applied": true,
                "verification_status": "failed",
                "mutation_transition": "committed",
                "mutation_replayed": false,
                "mutation_receipt_persisted": true
            }
        }));
        let metadata = usage_result_metadata(Some(&edit_follow_up));
        assert_eq!(metadata.error_code, "verification_failed");
        assert_eq!(metadata.recovery.next_action_tool, "VerifyEdit");
        assert_eq!(metadata.recovery.required_read_count, 0);
        assert!(!metadata.recovery.retry_same_call);
        assert!(metadata.recovery.edit_applied);
        assert!(!metadata.recovery.fresh_read_required);
        assert_eq!(metadata.recovery.verification_status, "failed");
        assert_eq!(metadata.recovery.mutation_transition, "committed");
        assert_eq!(metadata.recovery.mutation_receipt_state, "persisted");

        let edit_conflict = result_with(serde_json::json!({
            "errorCode": "search_block_not_found",
            "retrySameCall": false,
            "nextAction": {"tool": "ReadFiles"},
            "requiredReads": [{"path": "/workspace/lib.rs", "ranges": []}],
            "data": {
                "edit_applied": false,
                "fresh_read_required": true,
                "recovery_attempt": 2,
                "recovery_escalated": true
            }
        }));
        let metadata = usage_result_metadata(Some(&edit_conflict));
        assert_eq!(metadata.error_code, "search_block_not_found");
        assert_eq!(metadata.recovery.next_action_tool, "ReadFiles");
        assert_eq!(metadata.recovery.required_read_count, 1);
        assert!(!metadata.recovery.retry_same_call);
        assert!(!metadata.recovery.edit_applied);
        assert!(metadata.recovery.fresh_read_required);
        assert_eq!(metadata.recovery.verification_status, "");
        assert_eq!(metadata.recovery.recovery_attempt, 2);
        assert_eq!(metadata.recovery.recovery_level, "escalated");
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
