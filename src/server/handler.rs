use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, GetPromptRequestParams,
        GetPromptResponse, GetPromptResult, GetTaskParams, GetTaskResult, Icon, Implementation,
        ListPromptsResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
        PromptMessage, ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse,
        ReadResourceResult, Resource, ResourceContents, Role, ServerCapabilities, ServerInfo,
        TaskStatus, Tool, UpdateTaskParams,
    },
    service::{NotificationContext, RequestContext, RoleServer},
    ErrorData as McpError, ServerHandler,
};

use super::catalog::{server_icon_data_uri, winx_prompts, winx_tools};
use super::principal::{principal_from_context, scope_tool_request, task_belongs_to_principal};
use super::WinxService;

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
        .with_instructions(
            "Winx is a high-performance Rust implementation of MCP tools for shell and file management.",
        )
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
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(winx_tools()))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![Resource::new("file://readme", "README")
            .with_description("Project README documentation")
            .with_mime_type("text/markdown")]))
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult::with_all_items(winx_prompts()))
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
        _context: RequestContext<RoleServer>,
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
        Ok(ReadResourceResult::new(content).into())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let principal = principal_from_context(&context);
        let (request, scope) = scope_tool_request(request, principal).map_err(|error| {
            McpError::invalid_request(format!("Cannot scope remote request: {error}"), None)
        })?;
        if context.client_capabilities().is_some_and(|capabilities| capabilities.supports_tasks())
            && Self::bash_task_is_eligible(&request)
        {
            return Ok(CallToolResponse::Task(self.enqueue_bash_task(request, scope).await?));
        }

        match self.execute_tool_call(request).await {
            Ok(mut result) => {
                scope.unscope_result(&mut result);
                Ok(result.into())
            }
            Err(mut error) => {
                scope.unscope_error(&mut error);
                Err(error)
            }
        }
    }
}
