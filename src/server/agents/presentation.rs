use rmcp::model::CallToolResult;
use serde_json::{json, Value};

use crate::runtime::workflow::{PromptDelivery, PromptOutcome, WorkflowObservation};
use crate::state::execution_history::{ExecutionRecord, HistoryResult};

pub(in crate::server) fn attach_agent_result(
    result: &mut CallToolResult,
    workflow: Option<&WorkflowObservation>,
    arguments: &Value,
    execution_id: &str,
    history: HistoryResult<Option<ExecutionRecord>>,
) {
    // The requested execution was already resolved and generation-bound. Its
    // identity remains available even if persisting the new observation fails.
    if let Some(envelope) = result.structured_content.as_mut() {
        envelope["data"]["execution_id"] = json!(execution_id);
    }
    attach_workflow_result(result, workflow, arguments);
    attach_history_result(result, history);
}

pub(in crate::server) fn attach_history_result(
    result: &mut CallToolResult,
    history: HistoryResult<Option<ExecutionRecord>>,
) {
    let Some(envelope) = result.structured_content.as_mut() else { return };
    match history {
        Ok(Some(record)) => {
            envelope["data"]["history_recorded"] = json!(true);
            envelope["data"]["execution_id"] = json!(record.execution_id);
            envelope["data"]["history_sequence"] = json!(record.sequence);
        }
        Ok(None) => {
            envelope["data"]["history_recorded"] = json!(false);
        }
        Err(error) => {
            envelope["data"]["history_recorded"] = json!(false);
            envelope["data"]["history_error"] = json!(error.to_string());
            // The command already ran. Preserve its process state and never
            // suggest resubmitting it because the history store failed.
            if envelope["status"] == "completed" {
                envelope["status"] = json!("completed_with_issues");
                envelope["errorCode"] = json!("execution_history_unavailable");
                envelope["message"] =
                    json!("Command completed, but execution history could not be retained.");
            }
        }
    }
}

pub(in crate::server) fn attach_workflow_result(
    result: &mut CallToolResult,
    workflow: Option<&WorkflowObservation>,
    arguments: &Value,
) {
    let Some(workflow) = workflow else { return };
    let Some(envelope) = result.structured_content.as_mut() else { return };
    envelope["data"]["explanation"] = json!(workflow.explanation);
    envelope["data"]["activity_seq"] = json!(workflow.activity_seq);
    let Some(receipt) = workflow.prompt_receipt.as_ref() else { return };
    // Never preserve a generic command recovery action for a prompt receipt.
    // A safe observation action is rebuilt below only with a known execution ID.
    if let Some(object) = envelope.as_object_mut() {
        object.remove("nextAction");
        object.remove("retryAfterMs");
    }
    envelope["data"]["prompt_receipt"] = json!(receipt);
    envelope["data"]["prompt_correlated"] = json!(receipt.activity_observed);
    let (status, code, message) = if receipt.delivery == PromptDelivery::Uncertain {
        ("completed_with_issues", Some("prompt_delivery_uncertain"),
         "Prompt delivery is uncertain. Inspect this receipt; do not automatically resend input.")
    } else {
        match receipt.outcome {
            PromptOutcome::GenerationChanged =>
                ("completed_with_issues", Some("prompt_generation_changed"),
                 "The target execution changed; this receipt cannot describe its replacement."),
            PromptOutcome::AwaitingInput if receipt.activity_observed =>
                ("awaiting_input", None, "Post-prompt activity was observed and the target is awaiting input."),
            PromptOutcome::AwaitingApproval if receipt.activity_observed =>
                ("awaiting_approval", None, "The target is awaiting approval after the prompt."),
            PromptOutcome::Exited => match envelope["data"]["exit_code"].as_i64() {
                Some(0) => ("completed", None, "The target execution exited with status zero."),
                Some(_) => ("completed_with_issues", Some("prompt_execution_failed"),
                    "The target execution exited with a nonzero status."),
                None => ("completed_with_issues", Some("prompt_exit_status_unknown"),
                    "The target execution exited, but its exit status is unknown."),
            },
            _ if receipt.activity_observed =>
                ("running", None, "The prompt was delivered and activity was observed; continue waiting on this receipt."),
            _ =>
                ("completed_with_issues", Some("prompt_activity_unconfirmed"),
                 "The prompt was delivered, but post-prompt work has not been confirmed. Continue observing this receipt without resending it."),
        }
    };
    envelope["status"] = json!(status);
    envelope["message"] = json!(message);
    envelope["retrySameCall"] = json!(false);
    if let Some(code) = code {
        envelope["errorCode"] = json!(code);
    } else if let Some(object) = envelope.as_object_mut() {
        object.remove("errorCode");
    }
    if matches!(receipt.outcome, PromptOutcome::Waiting | PromptOutcome::TimedOut)
        || receipt.delivery == PromptDelivery::Uncertain
    {
        let execution_id = envelope["data"]["execution_id"].clone();
        if execution_id.is_string() {
            envelope["retryAfterMs"] = json!(1_000);
            envelope["nextAction"] = json!({
                "tool": "Agent",
                "instruction": "Wait for retryAfterMs, then observe the same execution and prompt receipt. This call never resends text.",
                "arguments": {
                    "thread_id": arguments["thread_id"],
                    "workspace_root": arguments["workspace_root"],
                    "backend": "native",
                    "action": "wait",
                    "target": execution_id,
                    "prompt_id": receipt.prompt_id,
                    "timeout_seconds": 5.0,
                }
            });
        }
    } else if let Some(object) = envelope.as_object_mut() {
        object.remove("nextAction");
        object.remove("retryAfterMs");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::workflow::PromptReceipt;
    use crate::state::execution_history::HistoryError;
    use crate::state::turn::{recognizer_for, TurnInput};

    fn observation(outcome: PromptOutcome) -> WorkflowObservation {
        WorkflowObservation {
            output: String::new(),
            output_included: false,
            screen: Vec::new(),
            activity_seq: 5,
            explanation: recognizer_for("auto").explain_input(&TurnInput {
                screen: &[],
                osc_title: "",
                osc_progress: "",
            }),
            prompt_receipt: Some(PromptReceipt {
                prompt_id: "one".into(),
                delivery: PromptDelivery::Delivered,
                activity_observed: false,
                outcome,
                generation: 1,
                baseline_activity_seq: 4,
            }),
        }
    }

    fn result() -> CallToolResult {
        let mut result = CallToolResult::success(Vec::new());
        result.structured_content = Some(json!({
            "status": "completed", "data": { "exit_code": null },
            "nextAction": { "tool": "BashCommand", "arguments": { "text": "must never resend" } },
            "retryAfterMs": 10,
        }));
        result
    }

    #[test]
    fn known_target_receipt_recovery_survives_history_failure() -> anyhow::Result<()> {
        let mut result = result();
        let error = HistoryError::Io(std::io::Error::other("history unavailable"));
        attach_agent_result(
            &mut result,
            Some(&observation(PromptOutcome::TimedOut)),
            &json!({ "thread_id": "thread", "workspace_root": "/workspace" }),
            "execution-one",
            Err(error),
        );
        let envelope = result.structured_content.ok_or_else(|| anyhow::anyhow!("envelope"))?;
        assert_eq!(envelope["data"]["execution_id"], "execution-one");
        assert_eq!(envelope["data"]["history_recorded"], false);
        assert_eq!(envelope["nextAction"]["tool"], "Agent");
        assert_eq!(envelope["nextAction"]["arguments"]["target"], "execution-one");
        assert_eq!(envelope["nextAction"]["arguments"]["action"], "wait");
        assert!(envelope["nextAction"]["arguments"].get("text").is_none());
        assert_eq!(envelope["retrySameCall"], false);
        Ok(())
    }

    #[test]
    fn receipt_without_execution_id_never_keeps_legacy_recovery() -> anyhow::Result<()> {
        let mut result = result();
        attach_workflow_result(&mut result, Some(&observation(PromptOutcome::Waiting)), &json!({}));
        let envelope = result.structured_content.ok_or_else(|| anyhow::anyhow!("envelope"))?;
        assert!(envelope.get("nextAction").is_none());
        assert!(envelope.get("retryAfterMs").is_none());
        Ok(())
    }

    #[test]
    fn exit_without_status_is_not_reported_as_success() -> anyhow::Result<()> {
        let mut result = result();
        attach_workflow_result(&mut result, Some(&observation(PromptOutcome::Exited)), &json!({}));
        let envelope = result.structured_content.ok_or_else(|| anyhow::anyhow!("envelope"))?;
        assert_eq!(envelope["status"], "completed_with_issues");
        assert_eq!(envelope["errorCode"], "prompt_exit_status_unknown");
        assert!(envelope.get("nextAction").is_none());
        Ok(())
    }
}
