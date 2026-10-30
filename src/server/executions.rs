//! Session-scoped observation tools; they never send terminal input.
use crate::server::usage::lifecycle::{phase, Phase};
pub(super) mod history;
mod observe;
mod wait;

use std::path::Path;
use std::time::Duration;

use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::ErrorData as McpError;
use serde_json::{json, Value};

use super::outcomes::{self, ToolResultEnvelope, ToolResultStatus};
use super::sessions::SessionGuard;
use super::workflow_types::{ExecutionAction, ExecutionRequest};
use super::{SharedBashState, WinxService};
use crate::errors::WinxError;
use crate::state::execution_history::{
    ExecutionProcessStatus, ExecutionRecord, ExecutionScope, HistoryError, HistoryResult,
    DEFAULT_PAGE_BYTES,
};
use crate::types::Modes;

use history::{history_call, now_ms, refresh_record};
pub(super) use wait::WaitTiming;

pub(super) struct WorkflowContext {
    pub slot: SharedBashState,
    pub scope: ExecutionScope,
    pub allow_prompt: bool,
    _session_guard: SessionGuard,
}

pub(super) struct WorkflowReply {
    pub status: ToolResultStatus,
    pub error_code: Option<&'static str>,
    pub message: String,
    pub output: Option<String>,
    pub data: Value,
}

pub(super) fn workflow_result(
    tool: &str,
    reply: WorkflowReply,
) -> Result<CallToolResult, McpError> {
    workflow_envelope(
        tool,
        reply.status,
        &reply.message,
        reply.error_code,
        reply.output,
        reply.data,
    )
}

pub(super) fn workflow_envelope(
    tool: &str,
    status: ToolResultStatus,
    message: &str,
    error_code: Option<&str>,
    output: Option<String>,
    data: Value,
) -> Result<CallToolResult, McpError> {
    let content =
        output.as_ref().map_or_else(Vec::new, |text| vec![ContentBlock::text(text.clone())]);
    let envelope = ToolResultEnvelope {
        effective_wait: None,
        status,
        tool: tool.to_string(),
        message: message.to_string(),
        output,
        error_code: error_code.map(str::to_string),
        retryable: false,
        retry_same_call: false,
        retry_after_ms: None,
        next_action: None,
        required_reads: Vec::new(),
        data: Some(data),
    };
    let mut result = CallToolResult::success(content);
    result.is_error = Some(matches!(
        status,
        ToolResultStatus::InvalidInput
            | ToolResultStatus::NotFound
            | ToolResultStatus::Denied
            | ToolResultStatus::Failed
    ));
    result.structured_content = Some(serde_json::to_value(envelope).map_err(|error| {
        McpError::internal_error(format!("could not serialize workflow result: {error}"), None)
    })?);
    Ok(result)
}

pub(super) fn history_error_details(error: &HistoryError) -> (ToolResultStatus, &'static str) {
    match error {
        HistoryError::NotFound => (ToolResultStatus::NotFound, "execution_not_found"),
        HistoryError::InvalidInput(_) => {
            (ToolResultStatus::InvalidInput, "invalid_workflow_request")
        }
        HistoryError::InvalidCursor => (ToolResultStatus::InvalidInput, "invalid_execution_cursor"),
        HistoryError::IdentityConflict => {
            (ToolResultStatus::Conflict, "execution_identity_conflict")
        }
        HistoryError::Capacity => {
            (ToolResultStatus::CompletedWithIssues, "execution_history_capacity")
        }
        HistoryError::Io(_) | HistoryError::Json(_) => {
            (ToolResultStatus::Failed, "execution_history_unavailable")
        }
    }
}

pub(super) fn history_failure(
    tool: &str,
    error: &HistoryError,
) -> Result<CallToolResult, McpError> {
    let (status, code) = history_error_details(error);
    workflow_envelope(tool, status, &error.to_string(), Some(code), None, json!({}))
}

pub(super) fn bounded_timeout(value: Option<f32>) -> HistoryResult<Duration> {
    bounded_timeout_up_to(value, super::wait_budget::DEFAULT_EXECUTION_WAIT_MAX_SECONDS)
}

/// Validate a workflow timeout against `ceiling`, which only a client-profile
/// wait budget raises above the public 30 s range.
pub(super) fn bounded_timeout_up_to(value: Option<f32>, ceiling: f32) -> HistoryResult<Duration> {
    let seconds = value.unwrap_or(5.0);
    if !seconds.is_finite() || !(0.0..=ceiling).contains(&seconds) {
        return Err(HistoryError::InvalidInput(format!(
            "timeout_seconds must be between 0 and {ceiling}"
        )));
    }
    Ok(Duration::from_secs_f32(seconds))
}

impl WinxService {
    pub(super) async fn workflow_context(
        &self,
        thread_id: &str,
        workspace_root: &Path,
    ) -> crate::errors::Result<WorkflowContext> {
        if crate::types::normalize_thread_id(thread_id).is_empty() {
            return Err(WinxError::WorkspaceBindingRequired { thread_id: String::new() });
        }
        let (slot, session_guard) = self.tool_session_for(thread_id).await;
        let (scope, allow_prompt) = {
            let guard = slot.lock().await;
            let state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
            let requested = workspace_root
                .canonicalize()
                .map_err(|error| WinxError::WorkspacePathError(error.to_string()))?;
            if requested != state.workspace_root {
                return Err(WinxError::WorkspaceBindingMismatch {
                    thread_id: thread_id.to_string(),
                    requested_workspace: requested,
                    bound_workspace: state.workspace_root.clone(),
                });
            }
            (
                ExecutionScope {
                    thread_id: state.current_thread_id.clone(),
                    workspace_root: state.workspace_root.clone(),
                },
                matches!(state.mode, Modes::Wcgw),
            )
        };
        Ok(WorkflowContext { slot, scope, allow_prompt, _session_guard: session_guard })
    }

    /// Keep session admission inside the same deadline as the lifecycle wait.
    pub(super) async fn workflow_context_for_wait(
        &self,
        thread_id: &str,
        workspace_root: &Path,
        timing: Option<WaitTiming>,
    ) -> HistoryResult<crate::errors::Result<WorkflowContext>> {
        let guard = phase(Phase::SessionAdmission);
        let operation = self.workflow_context(thread_id, workspace_root);
        let result = if let Some(timing) = timing {
            tokio::time::timeout_at(timing.deadline(), operation).await.map_err(|_| {
                HistoryError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "execution workflow deadline elapsed before the session context could be acquired",
                ))
            })
        } else {
            Ok(operation.await)
        };
        guard.finish(matches!(&result, Ok(Ok(_))));
        result
    }

    pub(super) async fn handle_execution(
        &self,
        args: Option<Value>,
    ) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let request: ExecutionRequest = serde_json::from_value(args.clone()).map_err(|error| {
            McpError::invalid_request(format!("Invalid Execution parameters: {error}"), None)
        })?;
        if let ExecutionAction::Observe { items, .. } = &request.action {
            if let Err(error) =
                crate::state::execution_history::validate_observation_requests(items)
            {
                return history_failure("Execution", &error);
            }
        }
        let wait_timing = match &request.action {
            // Only a lifecycle wait may use a client-profile budget above 30 s;
            // an observation is a single bounded pass.
            ExecutionAction::Wait { timeout_seconds, .. } => {
                match WaitTiming::up_to(
                    *timeout_seconds,
                    super::wait_budget::execution_wait_ceiling(),
                ) {
                    Ok(timing) => Some(timing),
                    Err(error) => return history_failure("Execution", &error),
                }
            }
            ExecutionAction::Observe { timeout_seconds, .. } => {
                match WaitTiming::new(*timeout_seconds) {
                    Ok(timing) => Some(timing),
                    Err(error) => return history_failure("Execution", &error),
                }
            }
            _ => None,
        };
        let context = match self
            .workflow_context_for_wait(&request.thread_id, &request.workspace_root, wait_timing)
            .await
        {
            Ok(Ok(context)) => context,
            Ok(Err(error)) => return outcomes::tool_failure("Execution", &error, Some(&args)),
            Err(error) => return history_failure("Execution", &error),
        };
        match self.execution_action(&context, request.action, wait_timing).await {
            Ok(reply) => workflow_result("Execution", reply),
            Err(error) => history_failure("Execution", &error),
        }
    }

    pub(super) async fn execution_action(
        &self,
        context: &WorkflowContext,
        action: ExecutionAction,
        wait_timing: Option<WaitTiming>,
    ) -> HistoryResult<WorkflowReply> {
        match action {
            ExecutionAction::List { limit } => self.execution_list(context, limit).await,
            ExecutionAction::Get { execution_id } => {
                let record = self.execution_record(context, &execution_id, None).await?;
                Ok(WorkflowReply {
                    status: ToolResultStatus::Completed,
                    error_code: None,
                    message: "Retained execution metadata.".into(),
                    output: None,
                    data: json!({ "execution": record }),
                })
            }
            ExecutionAction::Explain { execution_id, recognizer } => {
                let record = self.execution_record(context, &execution_id, recognizer).await?;
                Ok(WorkflowReply {
                    status: ToolResultStatus::Completed,
                    error_code: None,
                    message: "Runtime state and the recognition evidence captured with it.".into(),
                    output: None,
                    data: json!({
                        "execution": record,
                        "historical": record.process_status != ExecutionProcessStatus::Running,
                        "explanation": record.explanation,
                    }),
                })
            }
            ExecutionAction::Read { execution_id, source, cursor, page_bytes } => {
                let record = self.execution_record(context, &execution_id, None).await?;
                let scope = context.scope.clone();
                let catalog = self.execution_history.catalog()?;
                let page = history_call(catalog, move |catalog| {
                    catalog.read(
                        &scope,
                        &execution_id,
                        source,
                        cursor.as_deref(),
                        page_bytes.unwrap_or(DEFAULT_PAGE_BYTES),
                        now_ms(),
                    )
                })
                .await?;
                Ok(WorkflowReply {
                    status: ToolResultStatus::Completed,
                    error_code: None,
                    message: if page.resync_required {
                        "Output retention changed; continue with next_cursor to resynchronize."
                    } else {
                        "One bounded page of retained execution output."
                    }
                    .into(),
                    output: Some(page.text.clone()),
                    data: json!({
                        "page": page,
                        "output_pending": record.output_pending,
                        "output_activity_seq": record.output_activity_seq,
                    }),
                })
            }
            ExecutionAction::Observe { items, timeout_seconds } => {
                self.observe_executions(context, items, timeout_seconds, wait_timing).await
            }
            ExecutionAction::Wait { execution_ids, after, events, timeout_seconds } => {
                self.wait_for_executions(
                    context,
                    execution_ids,
                    after,
                    events,
                    timeout_seconds,
                    wait_timing,
                )
                .await
            }
        }
    }

    async fn execution_list(
        &self,
        context: &WorkflowContext,
        limit: Option<usize>,
    ) -> HistoryResult<WorkflowReply> {
        let limit = limit.unwrap_or(128);
        if !(1..=128).contains(&limit) {
            return Err(HistoryError::InvalidInput("list limit must be between 1 and 128".into()));
        }
        self.refresh_workflow_history(context, None).await?;
        let catalog = self.execution_history.catalog()?;
        let scope = context.scope.clone();
        let mut snapshot =
            history_call(catalog, move |catalog| catalog.list(&scope, now_ms())).await?;
        let omitted = snapshot.executions.len().saturating_sub(limit);
        snapshot.executions.truncate(limit);
        Ok(WorkflowReply {
            status: ToolResultStatus::Completed,
            error_code: None,
            message: "Executions retained for this principal, thread and workspace.".into(),
            output: None,
            data: json!({ "catalog": snapshot, "omitted": omitted }),
        })
    }

    pub(super) async fn execution_record(
        &self,
        context: &WorkflowContext,
        execution_id: &str,
        recognizer: Option<String>,
    ) -> HistoryResult<ExecutionRecord> {
        let catalog = self.execution_history.catalog()?;
        let scope = context.scope.clone();
        let id = execution_id.to_string();
        let record = history_call(catalog.clone(), move |catalog| {
            catalog.get(&scope, &id, now_ms())?.ok_or(HistoryError::NotFound)
        })
        .await?;
        if (record.process_status == ExecutionProcessStatus::Exited && !record.output_pending)
            || !self
                .shell_runtime
                .supports_agent_workflows_for(&context.slot)
                .await
                .unwrap_or(false)
        {
            return Ok(record);
        }
        refresh_record(
            self.shell_runtime.as_ref(),
            &context.slot,
            &catalog,
            &context.scope,
            &record,
            recognizer,
        )
        .await
    }

    pub(super) async fn refresh_workflow_history(
        &self,
        context: &WorkflowContext,
        execution_ids: Option<&[String]>,
    ) -> HistoryResult<bool> {
        let supported =
            self.shell_runtime.supports_agent_workflows_for(&context.slot).await.unwrap_or(false);
        if !supported {
            return Ok(false);
        }
        let catalog = self.execution_history.catalog()?;
        let scope = context.scope.clone();
        let snapshot =
            history_call(catalog.clone(), move |catalog| catalog.list(&scope, now_ms())).await?;
        for record in snapshot.executions.iter().filter(|record| {
            (record.process_status != ExecutionProcessStatus::Exited || record.output_pending)
                && execution_ids.is_none_or(|ids| ids.contains(&record.execution_id))
        }) {
            refresh_record(
                self.shell_runtime.as_ref(),
                &context.slot,
                &catalog,
                &context.scope,
                record,
                None,
            )
            .await?;
        }
        Ok(true)
    }
}
