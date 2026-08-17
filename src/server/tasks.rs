use std::time::Duration;

use rand::RngExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, CreateTaskResult, Task, TaskStatus,
};
use rmcp::ErrorData as McpError;
use serde_json::Value;
use tracing::warn;

use super::WinxService;
use crate::state::task_state::{TaskEntry, DEFAULT_TASK_TTL_MS};
use crate::types::{normalize_thread_id, BashCommand, BashCommandAction};

const MAX_TASK_RUNTIME: Duration = Duration::from_secs(60 * 60);
const MAX_TASK_OUTPUT_BYTES: usize = 1_000_000;

impl WinxService {
    /// Run one task-augmented foreground shell command through to completion.
    /// Normal `BashCommand` calls intentionally yield after a short wait; an MCP
    /// Task keeps polling that same PTY and exposes one final `CallToolResult`.
    pub(super) async fn run_bash_task(
        &self,
        mut request: CallToolRequestParams,
    ) -> Result<CallToolResult, McpError> {
        let args = request
            .arguments
            .clone()
            .ok_or_else(|| McpError::invalid_request("Missing BashCommand arguments", None))?;
        let mut bash: BashCommand =
            serde_json::from_value(Value::Object(args)).map_err(|error| {
                McpError::invalid_request(
                    format!("Invalid BashCommand task parameters: {error}"),
                    None,
                )
            })?;
        let BashCommandAction::Command { is_background, .. } = &bash.action_json else {
            return Err(McpError::invalid_request(
                "MCP Tasks support BashCommand's foreground command action only",
                None,
            ));
        };
        if *is_background {
            return Err(McpError::invalid_request(
                "Do not combine an MCP Task with BashCommand is_background=true",
                None,
            ));
        }

        let thread_id = normalize_thread_id(&bash.thread_id);
        if thread_id.is_empty() {
            return Err(McpError::invalid_request(
                "Task-augmented BashCommand requires an explicit thread_id",
                None,
            ));
        }
        bash.thread_id.clone_from(&thread_id);
        bash.wait_for_seconds = Some(20.0);
        request.arguments = Some(task_arguments(&bash)?);

        let mut output = String::new();
        loop {
            let result = self.execute_tool_call(request).await?;
            let chunk = result
                .content
                .iter()
                .filter_map(|content| content.as_text().map(|text| text.text.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            append_task_output(&mut output, &chunk);
            if !chunk.contains("status = still running") {
                return Ok(CallToolResult::success(vec![ContentBlock::text(output)]));
            }

            let status = BashCommand {
                action_json: BashCommandAction::StatusCheck {
                    status_check: true,
                    bg_command_id: None,
                    scrollback_lines: None,
                    verbose: false,
                },
                wait_for_seconds: Some(20.0),
                thread_id: thread_id.clone(),
            };
            request =
                CallToolRequestParams::new("BashCommand").with_arguments(task_arguments(&status)?);
        }
    }

    pub(super) async fn interrupt_task_thread(&self, thread_id: &str) {
        let slot = {
            let registry = self.sessions.lock().await;
            registry.slots.get(thread_id).cloned()
        };
        let Some(slot) = slot else { return };
        if let Err(error) = self.shell_runtime.interrupt(&slot).await {
            warn!(thread_id, %error, "failed to interrupt cancelled MCP task");
        }
    }

    pub(super) fn bash_task_is_eligible(request: &CallToolRequestParams) -> bool {
        if request.name != "BashCommand" {
            return false;
        }
        let Some(arguments) = request.arguments.clone() else {
            return false;
        };
        let Ok(bash) = serde_json::from_value::<BashCommand>(Value::Object(arguments)) else {
            return false;
        };
        matches!(bash.action_json, BashCommandAction::Command { is_background: false, .. })
            && !normalize_thread_id(&bash.thread_id).is_empty()
    }

    pub(super) async fn enqueue_bash_task(
        &self,
        request: CallToolRequestParams,
    ) -> Result<CreateTaskResult, McpError> {
        let args = request
            .arguments
            .clone()
            .ok_or_else(|| McpError::invalid_request("Missing BashCommand arguments", None))?;
        let bash: BashCommand = serde_json::from_value(Value::Object(args)).map_err(|error| {
            McpError::invalid_request(format!("Invalid BashCommand task parameters: {error}"), None)
        })?;
        let BashCommandAction::Command { is_background, .. } = bash.action_json else {
            return Err(McpError::invalid_request(
                "MCP Tasks support BashCommand's foreground command action only",
                None,
            ));
        };
        if is_background {
            return Err(McpError::invalid_request(
                "Do not combine an MCP Task with BashCommand is_background=true",
                None,
            ));
        }
        let thread_id = normalize_thread_id(&bash.thread_id);
        if thread_id.is_empty() {
            return Err(McpError::invalid_request(
                "Task-augmented BashCommand requires an explicit thread_id",
                None,
            ));
        }

        let task_id = format!("task_{:032x}", rand::rng().random::<u128>());
        let now = rmcp::task_manager::current_timestamp();
        let task = Task::new(task_id.clone(), TaskStatus::Working, now.clone(), now)
            .with_status_message("Running BashCommand")
            .with_ttl_ms(DEFAULT_TASK_TTL_MS)
            .with_poll_interval_ms(1_000);
        self.tasks
            .lock()
            .await
            .insert(task_id.clone(), TaskEntry::working(task.clone(), thread_id.clone()))
            .map_err(|message| McpError::internal_error(message, None))?;

        let service = self.clone();
        let worker_task_id = task_id.clone();
        let handle = tokio::spawn(async move {
            let outcome =
                tokio::time::timeout(MAX_TASK_RUNTIME, service.run_bash_task(request)).await;

            let (status, message, result) = match outcome {
                Ok(Ok(result)) => match serde_json::to_value(result) {
                    Ok(result) => (
                        TaskStatus::Completed,
                        Some("BashCommand completed".to_string()),
                        Some(result),
                    ),
                    Err(error) => (
                        TaskStatus::Failed,
                        Some(format!("Failed to serialize task result: {error}")),
                        None,
                    ),
                },
                Ok(Err(error)) => (
                    TaskStatus::Failed,
                    Some(crate::utils::redact::redact(&error.message).into_owned()),
                    None,
                ),
                Err(_) => {
                    service.interrupt_task_thread(&thread_id).await;
                    (
                        TaskStatus::Failed,
                        Some("BashCommand task exceeded the one-hour runtime limit".to_string()),
                        None,
                    )
                }
            };

            let mut tasks = service.tasks.lock().await;
            if let Some(entry) = tasks.get_mut(&worker_task_id) {
                if entry.task.status == TaskStatus::Working {
                    entry.finish(status, message, result);
                }
            }
        });
        let abort_handle = handle.abort_handle();
        drop(handle);
        if let Some(entry) = self.tasks.lock().await.get_mut(&task_id) {
            if entry.task.status == TaskStatus::Working {
                entry.abort_handle = Some(abort_handle);
            }
        }

        Ok(CreateTaskResult::new(task))
    }
}

fn task_arguments<T: serde::Serialize>(
    value: &T,
) -> Result<serde_json::Map<String, Value>, McpError> {
    match serde_json::to_value(value) {
        Ok(Value::Object(arguments)) => Ok(arguments),
        Ok(_) => Err(McpError::internal_error("Task arguments were not an object", None)),
        Err(error) => Err(McpError::internal_error(
            format!("Failed to serialize task arguments: {error}"),
            None,
        )),
    }
}

fn append_task_output(output: &mut String, chunk: &str) {
    if !output.is_empty() && !chunk.is_empty() {
        output.push('\n');
    }
    output.push_str(chunk);
    if output.len() <= MAX_TASK_OUTPUT_BYTES {
        return;
    }
    let cut = crate::utils::floor_char_boundary(output, output.len() - MAX_TASK_OUTPUT_BYTES);
    output.replace_range(..cut, "(...earlier task output truncated...)\n");
}
