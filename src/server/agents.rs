//! Native agent workflows and an explicitly configured Herdr backend.
mod herdr;
mod presentation;

use presentation::attach_agent_result;
pub(super) use presentation::{attach_history_result, attach_workflow_result};

use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;
use serde_json::{json, Value};

use super::executions::history::bound_options;
use super::executions::{
    bounded_timeout, history_failure, workflow_envelope, workflow_result, WaitTiming,
    WorkflowContext,
};
use super::outcomes::{self, ToolResultStatus};
use super::workflow_types::{
    AgentBackend, AgentOperation, AgentRequest, ExecutionAction, ExecutionWatchEvent,
    WorkflowAuthority,
};
use super::WinxService;
use crate::state::execution_history::{ExecutionProcessStatus, HistoryError};
use crate::types::{BashCommand, BashCommandAction};

impl WinxService {
    pub(super) async fn handle_agent(
        &self,
        args: Option<Value>,
        authority: &WorkflowAuthority,
    ) -> Result<CallToolResult, McpError> {
        let args = args.ok_or_else(|| McpError::invalid_request("Missing arguments", None))?;
        let request: AgentRequest = serde_json::from_value(args.clone()).map_err(|error| {
            McpError::invalid_request(format!("Invalid Agent parameters: {error}"), None)
        })?;
        let wait_timing = if request.backend == AgentBackend::Native
            && request.action == AgentOperation::Wait
            && request.prompt_id.is_none()
        {
            match WaitTiming::new(request.timeout_seconds) {
                Ok(timing) => Some(timing),
                Err(error) => return history_failure("Agent", &error),
            }
        } else {
            None
        };
        let context = match self
            .workflow_context_for_wait(&request.thread_id, &request.workspace_root, wait_timing)
            .await
        {
            Ok(Ok(context)) => context,
            Ok(Err(error)) => return outcomes::tool_failure("Agent", &error, Some(&args)),
            Err(error) => return history_failure("Agent", &error),
        };
        if request.action == AgentOperation::Prompt
            && (!authority.bash_allowed || !context.allow_prompt)
        {
            return workflow_envelope(
                "Agent",
                ToolResultStatus::Denied,
                "Agent prompts require BashCommand authority and wcgw mode.",
                Some("agent_prompt_denied"),
                None,
                json!({ "input_sent": false }),
            );
        }
        match request.backend {
            AgentBackend::Herdr => self.handle_herdr_agent(&context, request, authority).await,
            AgentBackend::Native => self.handle_native_agent(&context, request, wait_timing).await,
        }
    }

    async fn handle_native_agent(
        &self,
        context: &WorkflowContext,
        request: AgentRequest,
        wait_timing: Option<WaitTiming>,
    ) -> Result<CallToolResult, McpError> {
        if request.lines.is_some() || !request.until.is_empty() {
            return history_failure("Agent", &HistoryError::InvalidInput(
                "lines and until are Herdr-only parameters; native waits use execution lifecycle events or prompt_id".into(),
            ));
        }
        if request.action == AgentOperation::Prompt
            || (request.action == AgentOperation::Wait && request.prompt_id.is_some())
        {
            return self.native_prompt(context, request).await;
        }
        if request.text.is_some() || request.prompt_id.is_some() {
            return history_failure(
                "Agent",
                &HistoryError::InvalidInput(
                    "text is only accepted by prompt; prompt_id is accepted by prompt or wait"
                        .into(),
                ),
            );
        }
        let action = match native_observation_action(request) {
            Ok(action) => action,
            Err(error) => return history_failure("Agent", &error),
        };
        match self.execution_action(context, action, wait_timing).await {
            Ok(mut reply) => {
                reply.data["backend"] = json!("native");
                reply.data["prompt_correlated"] = json!(false);
                workflow_result("Agent", reply)
            }
            Err(error) => history_failure("Agent", &error),
        }
    }

    async fn native_prompt(
        &self,
        context: &WorkflowContext,
        request: AgentRequest,
    ) -> Result<CallToolResult, McpError> {
        let (target, prompt_id) = match validate_native_prompt(&request) {
            Ok(values) => values,
            Err(error) => return history_failure("Agent", &error),
        };
        if !self.shell_runtime.supports_agent_workflows_for(&context.slot).await.unwrap_or(false) {
            return workflow_envelope(
                "Agent", ToolResultStatus::Failed,
                "The effective guardian does not advertise agent_workflows. Upgrade it before sending a prompt.",
                Some("agent_workflows_unsupported"), None, json!({ "input_sent": false }),
            );
        }
        let record = match self.execution_record(context, &target, request.recognizer.clone()).await
        {
            Ok(record) => record,
            Err(error) => return history_failure("Agent", &error),
        };
        // A retained terminal receipt can be queried after the runtime has reaped
        // the PTY. It must never fall through to a replacement foreground agent.
        if request.action == AgentOperation::Wait
            && record.process_status == ExecutionProcessStatus::Exited
        {
            if let Some(result) = cached_prompt_result(
                record.prompt_receipt.as_ref(),
                &prompt_id,
                record.exit_code,
                json!({ "backend": "native", "execution": record, "prompt_id": prompt_id }),
            ) {
                return result;
            }
        }
        let command = BashCommand {
            thread_id: context.scope.thread_id.clone(),
            wait_for_seconds: Some(0.0),
            action_json: BashCommandAction::Prompt {
                bg_command_id: record.background_id.clone(),
                prompt_id,
                text: request.text,
                recognizer: request.recognizer,
                timeout_seconds: request.timeout_seconds,
            },
        };
        let mut recovery_args = serde_json::to_value(&command)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        recovery_args["workspace_root"] = json!(context.scope.workspace_root);
        let outcome = match self
            .shell_runtime
            .run_action_detailed(&context.slot, command, bound_options(&record))
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => return outcomes::tool_failure("Agent", &error, Some(&recovery_args)),
        };
        let history = self
            .capture_bash_execution(
                &context.slot,
                &outcome,
                None,
                &context.scope.thread_id,
                Some(&context.scope),
            )
            .await;
        self.persist_state(&context.slot).await;
        let workflow = outcome.workflow.clone();
        let mut result = outcomes::bash_success_result(Some(&recovery_args), outcome, true)?;
        if let Some(envelope) = result.structured_content.as_mut() {
            envelope["tool"] = json!("Agent");
            envelope["data"]["backend"] = json!("native");
            envelope["data"]["target"] = json!(target);
        }
        attach_agent_result(
            &mut result,
            workflow.as_ref(),
            &recovery_args,
            &record.execution_id,
            history,
        );
        Ok(result)
    }
}

fn cached_prompt_result(
    receipt: Option<&crate::runtime::workflow::PromptReceipt>,
    prompt_id: &str,
    exit_code: Option<i32>,
    data: Value,
) -> Option<Result<CallToolResult, McpError>> {
    // The durable catalog caches one receipt. Older IDs may still be retained
    // beside the same generation's PTY/tombstone; observe them through the bound runtime.
    let receipt = receipt.filter(|receipt| receipt.prompt_id == prompt_id)?;
    Some(retained_prompt_result(receipt, exit_code, data))
}

fn retained_prompt_result(
    receipt: &crate::runtime::workflow::PromptReceipt,
    exit_code: Option<i32>,
    mut data: Value,
) -> Result<CallToolResult, McpError> {
    use crate::runtime::workflow::PromptDelivery;
    let (status, code, message) = if receipt.delivery == PromptDelivery::Uncertain {
        (ToolResultStatus::CompletedWithIssues, Some("prompt_delivery_uncertain"),
            "The target execution exited with an uncertain prompt receipt. No input was sent by this observation; do not resend automatically.")
    } else {
        match exit_code {
            Some(0) => (ToolResultStatus::Completed, None,
                "The target execution exited with status zero; no input was sent by this observation."),
            Some(_) => (ToolResultStatus::CompletedWithIssues, Some("prompt_execution_failed"),
                "The target execution exited with a nonzero status; no input was sent by this observation."),
            None => (ToolResultStatus::CompletedWithIssues, Some("prompt_exit_status_unknown"),
                "The target execution exited, but its exit status is unknown; no input was sent by this observation."),
        }
    };
    data["prompt_receipt"] = json!(receipt);
    data["prompt_correlated"] = json!(receipt.activity_observed);
    data["input_sent"] = json!(false);
    data["automatic_resend_allowed"] = json!(false);
    workflow_envelope("Agent", status, message, code, None, data)
}

fn required_target(request: &AgentRequest) -> Result<String, HistoryError> {
    request.target.as_ref().filter(|value| !value.is_empty()).cloned().ok_or_else(|| {
        HistoryError::InvalidInput("target is required for this Agent action".into())
    })
}

fn validate_native_prompt(request: &AgentRequest) -> Result<(String, String), HistoryError> {
    bounded_timeout(request.timeout_seconds)?;
    let target = required_target(request)?;
    let prompt_id = request
        .prompt_id
        .as_ref()
        .filter(|id| !id.is_empty() && id.len() <= 128 && !id.contains('\0'))
        .cloned()
        .ok_or_else(|| {
            HistoryError::InvalidInput(
                "native prompt/wait requires a nonempty prompt_id of at most 128 bytes without NUL"
                    .into(),
            )
        })?;
    match request.action {
        AgentOperation::Prompt if request.text.as_ref().is_none_or(String::is_empty) => {
            Err(HistoryError::InvalidInput("prompt requires nonempty text".into()))
        }
        AgentOperation::Wait if request.text.is_some() => Err(HistoryError::InvalidInput(
            "wait only observes an existing receipt; omit text".into(),
        )),
        _ if request.text.as_ref().is_some_and(|text| text.len() > 16 * 1024) => {
            Err(HistoryError::InvalidInput("prompt text exceeds 16 KiB".into()))
        }
        _ => Ok((target, prompt_id)),
    }
}

fn native_observation_action(request: AgentRequest) -> Result<ExecutionAction, HistoryError> {
    if request.action == AgentOperation::List {
        return Ok(ExecutionAction::List { limit: None });
    }
    let execution_id = required_target(&request)?;
    Ok(match request.action {
        AgentOperation::Get => ExecutionAction::Get { execution_id },
        AgentOperation::Explain => {
            ExecutionAction::Explain { execution_id, recognizer: request.recognizer }
        }
        AgentOperation::Read => ExecutionAction::Read {
            execution_id,
            source: request.source,
            cursor: request.cursor,
            page_bytes: request.page_bytes,
        },
        AgentOperation::Wait => ExecutionAction::Wait {
            execution_ids: vec![execution_id],
            after: request.cursor,
            events: vec![
                ExecutionWatchEvent::AwaitingInput,
                ExecutionWatchEvent::AwaitingApproval,
                ExecutionWatchEvent::Exited,
                ExecutionWatchEvent::Unknown,
            ],
            timeout_seconds: request.timeout_seconds,
        },
        AgentOperation::List | AgentOperation::Prompt => {
            return Err(HistoryError::InvalidInput("unsupported native observation action".into()))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_receipt_ids_fall_through_to_bound_runtime_observation() {
        use crate::runtime::workflow::{PromptDelivery, PromptOutcome, PromptReceipt};
        let receipt = PromptReceipt {
            prompt_id: "latest".into(),
            delivery: PromptDelivery::Delivered,
            activity_observed: false,
            outcome: PromptOutcome::Exited,
            generation: 1,
            baseline_activity_seq: 1,
        };
        assert!(cached_prompt_result(Some(&receipt), "older", Some(0), json!({})).is_none());
        assert!(cached_prompt_result(None, "older", Some(0), json!({})).is_none());
        assert!(cached_prompt_result(Some(&receipt), "latest", Some(0), json!({})).is_some());
    }

    #[test]
    fn archived_prompt_receipts_preserve_uncertainty_and_unknown_exit() -> anyhow::Result<()> {
        use crate::runtime::workflow::{PromptDelivery, PromptOutcome, PromptReceipt};
        for (exit_code, delivery, error_code) in [
            (None, PromptDelivery::Delivered, "prompt_exit_status_unknown"),
            (Some(7), PromptDelivery::Delivered, "prompt_execution_failed"),
            (Some(0), PromptDelivery::Uncertain, "prompt_delivery_uncertain"),
        ] {
            let receipt = PromptReceipt {
                prompt_id: "archived".into(),
                delivery,
                activity_observed: false,
                outcome: PromptOutcome::Exited,
                generation: 1,
                baseline_activity_seq: 1,
            };
            let result = retained_prompt_result(&receipt, exit_code, json!({}))?;
            let envelope = result.structured_content.ok_or_else(|| anyhow::anyhow!("envelope"))?;
            assert_eq!(envelope["status"], "completed_with_issues");
            assert_eq!(envelope["errorCode"], error_code);
            assert_eq!(envelope["data"]["input_sent"], false);
            assert_eq!(envelope["data"]["automatic_resend_allowed"], false);
            assert!(envelope.get("nextAction").is_none());
        }
        Ok(())
    }

    #[test]
    fn observation_wait_cannot_submit_text() -> anyhow::Result<()> {
        let request: AgentRequest = serde_json::from_value(json!({
            "thread_id": "test", "workspace_root": "/",
            "action": "wait", "target": "background:one",
            "prompt_id": "request-one", "text": "must not be sent"
        }))?;
        assert!(validate_native_prompt(&request).is_err());
        Ok(())
    }

    #[test]
    fn native_prompt_has_an_explicit_bounded_identity() -> anyhow::Result<()> {
        for prompt_id in [String::new(), "a".repeat(129), "nul\0id".to_string()] {
            let request: AgentRequest = serde_json::from_value(json!({
                "thread_id": "test", "workspace_root": "/",
                "action": "prompt", "target": "background:one",
                "prompt_id": prompt_id, "text": "hello"
            }))?;
            assert!(validate_native_prompt(&request).is_err());
        }
        Ok(())
    }
}
