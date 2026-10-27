use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;
use serde_json::json;

use super::super::executions::{
    bounded_timeout, history_failure, workflow_envelope, WorkflowContext,
};
use super::super::outcomes::ToolResultStatus;
use super::super::workflow_types::{AgentOperation, AgentRequest, WorkflowAuthority};
use super::super::WinxService;
use crate::state::execution_history::{HistoryError, OutputSource};
use crate::tools::herdr::{HerdrAction, HerdrAdapter, HerdrContext, HerdrError, HerdrRequest};

async fn run_adapter(
    adapter: &HerdrAdapter,
    context: &WorkflowContext,
    request: HerdrRequest,
    authority: &WorkflowAuthority,
) -> Result<serde_json::Value, HerdrError> {
    let prompt_guard = if request.action == HerdrAction::Prompt {
        let guard = context.slot.lock().await;
        let state = guard
            .as_ref()
            .ok_or_else(|| HerdrError::Denied("session is no longer initialized".into()))?;
        if state.current_thread_id != context.scope.thread_id
            || state.workspace_root != context.scope.workspace_root
        {
            return Err(HerdrError::Denied(
                "session thread/workspace binding changed before submission".into(),
            ));
        }
        if !authority.bash_allowed || !matches!(state.mode, crate::types::Modes::Wcgw) {
            return Err(HerdrError::Denied(
                "prompt requires current wcgw mode and BashCommand authority".into(),
            ));
        }
        Some(guard)
    } else {
        None
    };
    let herdr_context = HerdrContext {
        principal_key: authority.principal_key.clone(),
        workspace_root: context.scope.workspace_root.clone(),
        allow_prompt: authority.bash_allowed && context.allow_prompt,
    };
    let result = adapter.run(&herdr_context, request).await;
    drop(prompt_guard);
    result
}

impl WinxService {
    pub(in crate::server) async fn handle_herdr_agent(
        &self,
        context: &WorkflowContext,
        request: AgentRequest,
        authority: &WorkflowAuthority,
    ) -> Result<CallToolResult, McpError> {
        if request.prompt_id.is_some()
            || request.recognizer.is_some()
            || request.cursor.is_some()
            || request.page_bytes.is_some()
            || request.source != OutputSource::Output
        {
            return history_failure("Agent", &HistoryError::InvalidInput(
                "Herdr uses configured target aliases, recent text and lifecycle waits; native prompt receipts, recognizers and output cursors are not supported by this backend".into(),
            ));
        }
        if request.action != AgentOperation::Prompt && request.text.is_some() {
            return history_failure(
                "Agent",
                &HistoryError::InvalidInput("text is only accepted by prompt".into()),
            );
        }
        if request.action != AgentOperation::Wait && request.timeout_seconds.is_some() {
            return history_failure("Agent", &HistoryError::InvalidInput(
                "Herdr timeout_seconds is a lifecycle wait limit; use a separate wait action after prompt".into(),
            ));
        }
        if request.action != AgentOperation::Wait && !request.until.is_empty() {
            return history_failure(
                "Agent",
                &HistoryError::InvalidInput("until is only accepted by wait".into()),
            );
        }
        let timeout = match bounded_timeout(request.timeout_seconds) {
            Ok(timeout) => timeout,
            Err(error) => return history_failure("Agent", &error),
        };
        let adapter = match HerdrAdapter::from_env() {
            Ok(adapter) => adapter,
            Err(error) => return herdr_failure(&error),
        };
        let action = match request.action {
            AgentOperation::List => HerdrAction::List,
            AgentOperation::Get => HerdrAction::Get,
            AgentOperation::Explain => HerdrAction::Explain,
            AgentOperation::Read => HerdrAction::Read,
            AgentOperation::Prompt => HerdrAction::Prompt,
            AgentOperation::Wait => HerdrAction::Wait,
        };
        let herdr_request = HerdrRequest {
            action,
            target: request.target,
            text: request.text,
            lines: request.lines,
            timeout_ms: (request.action == AgentOperation::Wait)
                .then(|| u64::try_from(timeout.as_millis()).unwrap_or(30_000)),
            until: request.until,
        };
        match run_adapter(&adapter, context, herdr_request, authority).await {
            Ok(value) => herdr_success(action, &value),
            Err(error) => herdr_failure(&error),
        }
    }
}

fn herdr_success(
    action: HerdrAction,
    value: &serde_json::Value,
) -> Result<CallToolResult, McpError> {
    let mut data = json!({ "backend": "herdr", "result": value });
    let message = if action == HerdrAction::Prompt {
        data["submission_status"] = json!("accepted");
        data["input_sent"] = json!(true);
        data["prompt_correlated"] = json!(false);
        data["automatic_resend_allowed"] = json!(false);
        "Herdr prompt submitted; completion has not been observed. Use a separate wait action to inspect lifecycle state."
    } else {
        "Herdr action completed."
    };
    workflow_envelope("Agent", ToolResultStatus::Completed, message, None, None, data)
}

fn herdr_failure(error: &HerdrError) -> Result<CallToolResult, McpError> {
    let (status, code) = match error {
        HerdrError::Disabled => (ToolResultStatus::Failed, "herdr_disabled"),
        HerdrError::Configuration(_) => (ToolResultStatus::Failed, "herdr_configuration"),
        HerdrError::Denied(_) => (ToolResultStatus::Denied, "herdr_scope_denied"),
        HerdrError::Unavailable(_) => (ToolResultStatus::Failed, "herdr_unavailable"),
        HerdrError::InvalidRequest(_) => (ToolResultStatus::InvalidInput, "invalid_herdr_request"),
        HerdrError::InvalidResponse(_) => (ToolResultStatus::Failed, "invalid_herdr_response"),
        HerdrError::OutputLimit => (ToolResultStatus::Failed, "herdr_output_limit"),
        HerdrError::Timeout { uncertain: true } | HerdrError::DeliveryUncertain(_) => {
            (ToolResultStatus::CompletedWithIssues, "herdr_delivery_uncertain")
        }
        HerdrError::Timeout { uncertain: false } => (ToolResultStatus::Failed, "herdr_timeout"),
        HerdrError::Failed(_) => (ToolResultStatus::Failed, "herdr_failed"),
    };
    workflow_envelope(
        "Agent",
        status,
        &error.to_string(),
        Some(code),
        None,
        json!({
            "backend": "herdr",
            "input_may_have_been_sent": matches!(error, HerdrError::Timeout { uncertain: true } | HerdrError::DeliveryUncertain(_)),
            "automatic_resend_allowed": false,
        }),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::state::bash_state::BashState;
    use crate::types::Modes;
    use anyhow::{ensure, Context};
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn herdr_prompt_success_confirms_submission_only() -> anyhow::Result<()> {
        let result = herdr_success(
            HerdrAction::Prompt,
            &json!({ "agent": { "terminal_id": "terminal-1" } }),
        )?;
        let envelope = result.structured_content.context("envelope")?;
        assert_eq!(envelope["data"]["submission_status"], "accepted");
        assert_eq!(envelope["data"]["input_sent"], true);
        assert_eq!(envelope["data"]["prompt_correlated"], false);
        assert_eq!(envelope["data"]["automatic_resend_allowed"], false);
        assert!(envelope["message"]
            .as_str()
            .is_some_and(|text| text.contains("completion has not been observed")));
        Ok(())
    }

    #[tokio::test]
    async fn stale_herdr_prompt_context_is_denied_before_cli_spawn() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        let executable = root.join("herdr-fixture");
        let marker = root.join("spawned");
        fs::write(&executable, format!(
            "#!/usr/bin/python3\nfrom pathlib import Path\nPath({}).write_text('spawned')\nprint('{{\"id\":\"x\",\"result\":{{\"type\":\"agent_info\",\"agent\":{{\"terminal_id\":\"terminal-1\"}}}}}}')\n",
            serde_json::to_string(&marker.to_string_lossy())?,
        ))?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
        let config = root.join("operator.json");
        fs::write(
            &config,
            serde_json::to_vec(&json!({
                "executable": executable, "socket_path": root.join("fake.sock"), "deadline_ms": 1000,
                "bindings": [{ "principal_key": "local", "workspace_root": root,
                    "targets": { "writer": "terminal-1" } }],
            }))?,
        )?;
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600))?;
        let adapter = HerdrAdapter::from_config_path(&config)?;
        let service = WinxService::new();
        for mutation in ["mode", "thread", "workspace", "absent"] {
            let thread = format!("stale_herdr_{}_{:032x}", mutation, rand::random::<u128>());
            let (slot, _session) = service.tool_session_for(&thread).await;
            let mut state = BashState::new();
            state.current_thread_id = thread.clone();
            state.workspace_root = root.clone();
            state.mode = Modes::Wcgw;
            *slot.lock().await = Some(state);
            let context = service.workflow_context(&thread, &root).await?;
            {
                let mut guard = slot.lock().await;
                if mutation == "absent" {
                    *guard = None;
                } else {
                    let state = guard.as_mut().context("state")?;
                    match mutation {
                        "mode" => state.mode = Modes::Architect,
                        "thread" => state.current_thread_id = "replacement-thread".into(),
                        _ => state.workspace_root = root.join("replacement-workspace"),
                    }
                }
            }
            let request = HerdrRequest {
                action: HerdrAction::Prompt,
                target: Some("writer".into()),
                text: Some("must not be sent".into()),
                ..Default::default()
            };
            let result =
                run_adapter(&adapter, &context, request, &WorkflowAuthority::default()).await;
            ensure!(
                matches!(result, Err(HerdrError::Denied(_))),
                "stale {mutation} context must be denied: {result:?}"
            );
            ensure!(!marker.exists(), "stale {mutation} context spawned an external process");
        }
        Ok(())
    }
}
