use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rand::RngExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, CreateTaskResult, Task, TaskStatus,
};
use rmcp::ErrorData as McpError;
use serde_json::Value;
use tokio::task::AbortHandle;
use tracing::warn;

use super::principal::{new_task_id, RequestScope};
use super::tool_dispatch::ToolCallExecution;
use super::WinxService;
use crate::runtime::{ShellActionOptions, ShellExecutionToken};
use crate::state::task_state::{TaskEntry, DEFAULT_TASK_TTL_MS};
use crate::types::{normalize_thread_id, BashCommand, BashCommandAction, BashWaitPolicy};

const MAX_TASK_RUNTIME: Duration = Duration::from_secs(60 * 60);
const MAX_TASK_OUTPUT_BYTES: usize = 1_000_000;
const TASK_GENERATION_CAPTURE_SECONDS: f32 = 0.0;

pub(super) struct BashTaskReservation {
    pub(super) task_id: String,
    task: Task,
    thread_id: String,
}

impl WinxService {
    /// Poll one foreground command through to completion. Every status check is
    /// bound to the exact generation returned by the initial execution.
    #[allow(clippy::too_many_lines)]
    async fn run_bash_task(
        &self,
        mut request: CallToolRequestParams,
        mut initial_result: Option<ToolCallExecution>,
        compact_bash_output: bool,
        task_id: &str,
        abort_handle: &OnceLock<AbortHandle>,
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
        bash.wait_for_seconds = Some(TASK_GENERATION_CAPTURE_SECONDS);
        request.arguments = Some(task_arguments(&bash)?);

        let mut output = String::new();
        let mut output_truncated = false;
        let mut expected_execution: Option<ShellExecutionToken> = None;
        let launch_cancelled = self
            .tasks
            .lock()
            .await
            .get(task_id)
            .map(TaskEntry::execution_control)
            .map(|control| control.cancellation_flag())
            .ok_or_else(|| {
                McpError::internal_error("BashCommand task reservation vanished", None)
            })?;
        loop {
            let execution = if let Some(execution) = initial_result.take() {
                execution
            } else {
                if expected_execution.is_none() && !self.begin_task_launch(task_id).await {
                    return Err(McpError::internal_error(
                        "BashCommand task was cancelled before launch",
                        None,
                    ));
                }
                let launched = self
                    .execute_tool_call(
                        request,
                        task_action_options(
                            compact_bash_output,
                            expected_execution.clone(),
                            Arc::clone(&launch_cancelled),
                            task_id,
                        ),
                    )
                    .await;
                match launched {
                    Ok(execution) => execution,
                    Err(error) => return Err(error),
                }
            };
            let result = execution.result;
            if result.is_error == Some(true) {
                return Ok(result);
            }
            if execution.command_generation
                != execution.execution_token.as_ref().map(|token| token.generation)
            {
                return Err(McpError::internal_error(
                    "runtime returned inconsistent generation and execution token metadata",
                    None,
                ));
            }
            output_truncated |= append_result_output(&mut output, &result);
            let running = super::outcomes::result_status(&result) == "running";

            if expected_execution.is_none() {
                expected_execution = execution.execution_token;
                if running && (!execution.generation_bound_actions || expected_execution.is_none())
                {
                    let _ = self.shell_runtime.terminate_session(&thread_id).await.map_err(|error| {
                        warn!(%thread_id, %error, "failed to terminate unsafe unbound Task launch");
                        error
                    });
                    return Err(McpError::internal_error(
                        "runtime did not bind a running BashCommand to its generation",
                        None,
                    ));
                }
                if let Some(token) = expected_execution.clone() {
                    let still_working = self
                        .bind_task_execution(task_id, token.clone(), abort_handle.get().cloned())
                        .await;
                    if !still_working {
                        if running {
                            self.interrupt_task_execution(&thread_id, Some(token)).await;
                        }
                        return Err(McpError::internal_error(
                            "BashCommand task was cancelled",
                            None,
                        ));
                    }
                }
            } else if execution.execution_token != expected_execution {
                return Err(McpError::internal_error(
                    "runtime returned a different BashCommand generation while polling a task",
                    None,
                ));
            }

            if !running {
                let mut final_result = result;
                final_result.content = vec![ContentBlock::text(output)];
                update_aggregate_metadata(&mut final_result, output_truncated);
                return Ok(final_result);
            }
            request = status_request(&thread_id)?;
        }
    }

    async fn bind_task_execution(
        &self,
        task_id: &str,
        token: ShellExecutionToken,
        abort_handle: Option<AbortHandle>,
    ) -> bool {
        let mut tasks = self.tasks.lock().await;
        let Some(entry) = tasks.get_mut(task_id) else { return false };
        // Publish first. `tasks/cancel` and this transition share the registry
        // lock, so one side always observes the exact generation and the other
        // side handles interruption if cancellation won the race.
        entry.set_execution_token(token);
        if entry.task.status != TaskStatus::Working || entry.is_cancel_requested() {
            return false;
        }
        if let Some(abort_handle) = abort_handle {
            entry.abort_handle = Some(abort_handle);
        }
        true
    }

    async fn begin_task_launch(&self, task_id: &str) -> bool {
        let (working, control) = {
            let mut tasks = self.tasks.lock().await;
            let Some(entry) = tasks.get(task_id) else { return false };
            (
                entry.task.status == TaskStatus::Working && !entry.is_cancel_requested(),
                entry.execution_control(),
            )
        };
        if !working {
            control.finish_launch();
        }
        working
    }

    async fn finish_task_launch(&self, task_id: &str) {
        let control = self.tasks.lock().await.get(task_id).map(TaskEntry::execution_control);
        if let Some(control) = control {
            control.finish_launch();
        }
    }

    pub(super) async fn interrupt_task_execution(
        &self,
        thread_id: &str,
        expected: Option<ShellExecutionToken>,
    ) -> bool {
        let Some(expected) = expected else { return false };
        let slot = {
            let registry = self.sessions.lock().await;
            registry.slots.get(thread_id).cloned()
        };
        let Some(slot) = slot else { return false };
        match self.shell_runtime.interrupt_execution(&slot, Some(expected.clone())).await {
            Ok(interrupted) => interrupted,
            Err(error) => {
                warn!(thread_id, ?expected, %error, "failed to interrupt MCP task execution");
                false
            }
        }
    }

    pub(super) async fn cancel_pending_task_action(
        &self,
        thread_id: &str,
        cancellation_key: &str,
    ) -> bool {
        let slot = {
            let registry = self.sessions.lock().await;
            registry.slots.get(thread_id).cloned()
        };
        let Some(slot) = slot else { return false };
        match self.shell_runtime.cancel_pending_action(&slot, cancellation_key).await {
            Ok(cancelled) => cancelled,
            Err(error) => {
                warn!(thread_id, cancellation_key, %error, "failed to cancel pending MCP task action");
                false
            }
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

    pub(super) fn bash_wait_policy(
        request: &CallToolRequestParams,
    ) -> Result<BashWaitPolicy, McpError> {
        request
            .arguments
            .as_ref()
            .and_then(|arguments| arguments.get("wait_policy"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                McpError::invalid_request(format!("Invalid BashCommand wait_policy: {error}"), None)
            })
            .map(Option::unwrap_or_default)
    }

    /// Reserve registry capacity before an adaptive command is started. A full
    /// registry therefore fails before any child process can be launched.
    pub(super) async fn reserve_bash_task(
        &self,
        request: &CallToolRequestParams,
        scope: &RequestScope,
    ) -> Result<BashTaskReservation, McpError> {
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
                "Do not combine wait_policy=until_complete with is_background=true",
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

        let task_id = new_task_id(scope.principal(), rand::rng().random::<u128>());
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

        Ok(BashTaskReservation { task_id, task, thread_id })
    }

    pub(super) async fn release_bash_task(&self, reservation: &BashTaskReservation) {
        self.tasks.lock().await.remove(&reservation.task_id);
    }

    async fn fail_bash_task_promotion(
        &self,
        reservation: &BashTaskReservation,
        started_execution: Option<ShellExecutionToken>,
        message: &str,
    ) -> McpError {
        let safely_stopped = if let Some(execution) = started_execution.clone() {
            self.interrupt_task_execution(&reservation.thread_id, Some(execution)).await
        } else {
            // A runtime that returned `running` without a generation breached
            // the promotion contract. Terminating this one durable session is
            // safer than an unbound interrupt that could hit a later command.
            match self.shell_runtime.terminate_session(&reservation.thread_id).await {
                Ok(()) => true,
                Err(error) => {
                    warn!(
                        thread_id = reservation.thread_id,
                        %error,
                        "failed to terminate session after unsafe BashCommand task promotion"
                    );
                    false
                }
            }
        };
        self.release_bash_task(reservation).await;
        warn!(
            task_id = reservation.task_id,
            thread_id = reservation.thread_id,
            ?started_execution,
            safely_stopped,
            %message,
            "rolled back BashCommand task reservation after promotion failure"
        );
        McpError::internal_error(message.to_string(), None)
    }

    #[allow(clippy::too_many_lines)] // reservation promotion plus terminal Task serialization
    pub(super) async fn start_reserved_bash_task(
        &self,
        reservation: BashTaskReservation,
        request: CallToolRequestParams,
        scope: RequestScope,
        initial_result: Option<ToolCallExecution>,
        compact_bash_output: bool,
    ) -> Result<CreateTaskResult, McpError> {
        if let Some(initial) = initial_result.as_ref() {
            if super::outcomes::result_status(&initial.result) == "running" {
                let Some(token) = initial.execution_token.clone() else {
                    return Err(self
                        .fail_bash_task_promotion(
                            &reservation,
                            None,
                            "runtime did not bind the promoted BashCommand to a generation",
                        )
                        .await);
                };
                if !initial.generation_bound_actions {
                    return Err(self
                        .fail_bash_task_promotion(
                            &reservation,
                            Some(token.clone()),
                            "runtime cannot safely promote this BashCommand",
                        )
                        .await);
                }
                if !self.bind_task_execution(&reservation.task_id, token.clone(), None).await {
                    return Err(self
                        .fail_bash_task_promotion(
                            &reservation,
                            Some(token),
                            "BashCommand task reservation disappeared before promotion",
                        )
                        .await);
                }
            }
        }

        let abort_cell = Arc::new(OnceLock::new());
        let worker_abort_cell = Arc::clone(&abort_cell);
        let service = self.clone();
        let worker_task_id = reservation.task_id.clone();
        let thread_id = reservation.thread_id.clone();
        let handle = tokio::spawn(async move {
            let outcome = tokio::time::timeout(
                MAX_TASK_RUNTIME,
                service.run_bash_task(
                    request,
                    initial_result,
                    compact_bash_output,
                    &worker_task_id,
                    &worker_abort_cell,
                ),
            )
            .await;
            service.finish_task_launch(&worker_task_id).await;

            let (status, message, result) = match outcome {
                Ok(Ok(mut result)) => {
                    let failed = result.is_error == Some(true);
                    scope.unscope_result(&mut result);
                    refresh_aggregate_metadata(&mut result);
                    match serde_json::to_value(result) {
                        Ok(result) => (
                            if failed { TaskStatus::Failed } else { TaskStatus::Completed },
                            Some(if failed {
                                "BashCommand returned a tool-level error".to_string()
                            } else {
                                "BashCommand completed".to_string()
                            }),
                            Some(result),
                        ),
                        Err(error) => (
                            TaskStatus::Failed,
                            Some(format!("Failed to serialize task result: {error}")),
                            None,
                        ),
                    }
                }
                Ok(Err(mut error)) => {
                    scope.unscope_error(&mut error);
                    (
                        TaskStatus::Failed,
                        Some(crate::utils::redact::redact(&error.message).into_owned()),
                        None,
                    )
                }
                Err(_) => {
                    let execution = service
                        .tasks
                        .lock()
                        .await
                        .get(&worker_task_id)
                        .and_then(TaskEntry::execution_token);
                    service.interrupt_task_execution(&thread_id, execution).await;
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
        let _ = abort_cell.set(abort_handle.clone());
        drop(handle);

        // Promoted tasks already know their generation and can be aborted now.
        let mut tasks = self.tasks.lock().await;
        if let Some(entry) = tasks.get_mut(&reservation.task_id) {
            if entry.task.status == TaskStatus::Working && entry.command_generation().is_some() {
                entry.abort_handle = Some(abort_handle);
            }
        }
        drop(tasks);

        Ok(CreateTaskResult::new(reservation.task))
    }
}

fn status_request(thread_id: &str) -> Result<CallToolRequestParams, McpError> {
    let status = BashCommand {
        action_json: BashCommandAction::StatusCheck {
            status_check: true,
            bg_command_id: None,
            scrollback_lines: None,
            verbose: false,
        },
        wait_for_seconds: Some(20.0),
        thread_id: thread_id.to_string(),
    };
    Ok(CallToolRequestParams::new("BashCommand").with_arguments(task_arguments(&status)?))
}

fn task_action_options(
    compact_output: bool,
    expected_execution: Option<ShellExecutionToken>,
    launch_cancelled: Arc<std::sync::atomic::AtomicBool>,
    cancellation_key: &str,
) -> ShellActionOptions {
    ShellActionOptions {
        compact_output,
        expected_generation: expected_execution.as_ref().map(|token| token.generation),
        expected_execution,
        expected_guardian_epoch: None,
        require_generation_binding: true,
        cancellation_key: Some(cancellation_key.to_string()),
        launch_cancelled: Some(launch_cancelled),
    }
}

fn append_result_output(output: &mut String, result: &CallToolResult) -> bool {
    let source_truncated = result
        .structured_content
        .as_ref()
        .and_then(|structured| structured.get("data"))
        .and_then(|data| data.get("output_truncated"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let chunk = result
        .content
        .iter()
        .filter_map(|content| content.as_text().map(|text| text.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    append_task_output(output, &chunk) || source_truncated
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

fn append_task_output(output: &mut String, chunk: &str) -> bool {
    if !output.is_empty() && !chunk.is_empty() {
        output.push('\n');
    }
    output.push_str(chunk);
    if output.len() <= MAX_TASK_OUTPUT_BYTES {
        return false;
    }
    let cut = crate::utils::floor_char_boundary(output, output.len() - MAX_TASK_OUTPUT_BYTES);
    output.replace_range(..cut, "(...earlier task output truncated...)\n");
    true
}

fn update_aggregate_metadata(result: &mut CallToolResult, output_truncated: bool) {
    let output_bytes = result
        .content
        .iter()
        .filter_map(|content| content.as_text())
        .map(|text| text.text.len())
        .sum::<usize>();
    let Some(serde_json::Value::Object(data)) =
        result.structured_content.as_mut().and_then(|structured| structured.get_mut("data"))
    else {
        return;
    };
    data.insert("output_bytes".to_string(), serde_json::json!(output_bytes));
    data.insert("output_truncated".to_string(), serde_json::json!(output_truncated));
}

fn refresh_aggregate_metadata(result: &mut CallToolResult) {
    let output_truncated = result
        .structured_content
        .as_ref()
        .and_then(|structured| structured.get("data"))
        .and_then(|data| data.get("output_truncated"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    update_aggregate_metadata(result, output_truncated);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn aggregate_metadata_describes_truncated_final_task_payload() {
        let mut output = String::new();
        assert!(append_task_output(&mut output, &"x".repeat(MAX_TASK_OUTPUT_BYTES + 200)));
        let mut result = CallToolResult::success(vec![ContentBlock::text(output.clone())]);
        result.structured_content = Some(serde_json::json!({
            "status": "completed",
            "data": { "output_bytes": 3, "output_truncated": false }
        }));

        update_aggregate_metadata(&mut result, true);
        let data = &result.structured_content.expect("structured content")["data"];
        assert_eq!(data["output_bytes"], output.len());
        assert_eq!(data["output_truncated"], true);
        assert!(output.starts_with("(...earlier task output truncated...)"));
    }

    #[test]
    fn aggregate_inherits_truncation_from_each_runtime_chunk() {
        let mut result = CallToolResult::success(vec![ContentBlock::text("chunk")]);
        result.structured_content = Some(serde_json::json!({
            "data": { "output_truncated": true }
        }));
        let mut output = String::new();
        assert!(append_result_output(&mut output, &result));
        assert_eq!(output, "chunk");
    }
}
