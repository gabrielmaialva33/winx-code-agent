use super::*;
use crate::runtime::ShellActionOptions;
use crate::tool_policy::EditPermissionSet;
use rmcp::model::CallToolRequestParams;
use serde_json::json;

#[test]
fn execution_discovery_is_read_only_and_session_bound() -> anyhow::Result<()> {
    let tool = winx_tools().into_iter().find(|tool| tool.name == "Execution");
    assert!(tool.is_some(), "Execution must be discoverable in the full catalog");
    let value = serde_json::to_value(tool)?;
    assert_eq!(value["annotations"]["readOnlyHint"], true);
    let required = value["inputSchema"]["required"].as_array();
    assert!(required.is_some_and(|fields| fields.iter().any(|field| field == "thread_id")));
    assert!(required.is_some_and(|fields| fields.iter().any(|field| field == "workspace_root")));
    Ok(())
}

#[tokio::test]
async fn execution_requires_an_initialized_session() -> anyhow::Result<()> {
    let service = WinxService::new();
    let request: CallToolRequestParams = serde_json::from_value(json!({
        "name": "Execution",
        "arguments": {
            "thread_id": format!("workflow_uninitialized_{:032x}", rand::random::<u128>()),
            "workspace_root": "/",
            "action": {"type": "list"}
        }
    }))?;
    let call = service
        .execute_tool_call(request, ShellActionOptions::default(), EditPermissionSet::default())
        .await;
    assert!(
        call.is_ok(),
        "known Execution tool should return an actionable initialization result: {:?}",
        call.as_ref().err()
    );
    let envelope = call?.result.structured_content.unwrap_or_default();
    assert_eq!(envelope["status"], "needs_initialize");
    Ok(())
}

#[cfg(unix)]
mod real_runtime {
    use super::*;
    use anyhow::{ensure, Context};
    use serde_json::Value;

    struct Session {
        service: WinxService,
        directory: tempfile::TempDir,
        root: std::path::PathBuf,
        thread: String,
    }

    impl Session {
        async fn new() -> anyhow::Result<Self> {
            let directory = tempfile::tempdir()?;
            let root = directory.path().canonicalize()?;
            let mut session = Self {
                service: WinxService::new(),
                directory,
                root,
                thread: format!("execution_workflow_{:032x}", rand::random::<u128>()),
            };
            let initialized = session.call("Initialize", json!({
                "type": "first_call", "any_workspace_path": session.root, "mode_name": "wcgw",
            })).await?;
            ensure!(initialized["status"] == "completed", "{initialized}");
            session.thread = initialized["data"]["thread_id"]
                .as_str()
                .context("Initialize thread ID")?
                .to_string();
            Ok(session)
        }

        async fn call(&self, tool: &'static str, mut arguments: Value) -> anyhow::Result<Value> {
            arguments["thread_id"] = json!(self.thread);
            arguments["workspace_root"] = json!(self.root);
            let request = CallToolRequestParams::new(tool)
                .with_arguments(arguments.as_object().context("arguments object")?.clone());
            self.service
                .execute_tool_call(
                    request,
                    ShellActionOptions::default(),
                    crate::tool_policy::ToolPolicy::default().edit_permissions(),
                )
                .await?
                .result
                .structured_content
                .context("workflow result")
        }

        async fn command(&self, command: &str, background: bool) -> anyhow::Result<Value> {
            self.call(
                "BashCommand",
                json!({
                    "action_json": { "type": "command", "command": command,
                        "is_background": background, "allow_multi": true },
                    "wait_for_seconds": if background { 0.0 } else { 1.0 },
                }),
            )
            .await
        }

        async fn close(self) -> anyhow::Result<()> {
            self.service.shell_runtime.terminate_session(&self.thread).await?;
            drop(self.directory);
            Ok(())
        }
    }

    #[tokio::test]
    async fn archived_foreground_exit_and_output_survive_replacement() -> anyhow::Result<()> {
        let session = Session::new().await?;
        let first = session.command("sh -c 'printf retained-first-output; exit 7'", false).await?;
        let id = first["data"]["execution_id"].as_str().context("execution ID")?.to_string();
        assert_eq!(first["data"]["exit_code"], 7, "{first}");
        assert_eq!(first["data"]["history_recorded"], true, "{first}");
        session.command("printf replacement-output", false).await?;
        let retained = session
            .call(
                "Execution",
                json!({
                    "action": { "type": "get", "execution_id": id },
                }),
            )
            .await?;
        assert_eq!(retained["data"]["execution"]["exit_code"], 7, "{retained}");
        assert_eq!(retained["data"]["execution"]["process_status"], "exited");
        let page = session
            .call(
                "Execution",
                json!({
                    "action": { "type": "read", "execution_id": id, "source": "output" },
                }),
            )
            .await?;
        let text = page["data"]["page"]["text"].as_str().context("page text")?;
        assert!(text.contains("retained-first-output"), "{page}");
        assert!(!text.contains("replacement-output"), "{page}");
        session.close().await
    }

    #[tokio::test]
    async fn background_observer_retains_exit_without_consuming_status_output() -> anyhow::Result<()>
    {
        let session = Session::new().await?;
        let command = session
            .command("sh -c 'sleep 0.2; printf observer-preserved-output; exit 11'", true)
            .await?;
        let id = command["data"]["execution_id"].as_str().context("execution ID")?.to_string();
        let background =
            command["data"]["bg_command_id"].as_str().context("background ID")?.to_string();
        let context = session.service.workflow_context(&session.thread, &session.root).await?;
        let catalog = session.service.execution_history.catalog()?;
        // Read the store directly: an Execution query would also reconcile the
        // runtime and would not prove that the passive observer captured exit.
        let record = tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let record = catalog
                    .get(&context.scope, &id, super::super::super::executions::history::now_ms())?
                    .context("persisted execution")?;
                if record.process_status
                    == crate::state::execution_history::ExecutionProcessStatus::Exited
                {
                    return Ok::<_, anyhow::Error>(record);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("observer did not retain exit")??;
        assert_eq!(record.exit_code, Some(11));
        let status = session.call("BashCommand", json!({
            "action_json": { "type": "status_check", "status_check": true, "bg_command_id": background },
            "wait_for_seconds": 0.0,
        })).await?;
        assert!(
            status["output"]
                .as_str()
                .is_some_and(|text| text.contains("observer-preserved-output")),
            "{status}"
        );
        drop(context);
        session.close().await
    }

    #[tokio::test]
    async fn wait_any_returns_changed_execution_and_preserves_nonzero_exit() -> anyhow::Result<()> {
        let session = Session::new().await?;
        let first = session.command("sh -c 'sleep 0.6; exit 3'", true).await?;
        let second = session.command("sh -c 'sleep 1.2; exit 0'", true).await?;
        let first_id = first["data"]["execution_id"].as_str().context("first ID")?;
        let second_id = second["data"]["execution_id"].as_str().context("second ID")?;
        let listed = session.call("Execution", json!({ "action": { "type": "list" } })).await?;
        let cursor = listed["data"]["catalog"]["cursor"].as_str().context("catalog cursor")?;
        let waited = session
            .call(
                "Execution",
                json!({
                    "action": { "type": "wait", "execution_ids": [first_id, second_id],
                        "after": cursor, "events": ["exited"], "timeout_seconds": 4.0 },
                }),
            )
            .await?;
        assert_eq!(waited["data"]["timed_out"], false, "{waited}");
        let changes =
            waited["data"]["changes"]["executions"].as_array().context("execution changes")?;
        assert!(
            changes
                .iter()
                .any(|record| record["execution_id"] == first_id && record["exit_code"] == 3),
            "{waited}"
        );
        session.close().await
    }

    #[tokio::test]
    async fn lifecycle_real_catalog_wait_keeps_deadline_cursor_and_one_inspection(
    ) -> anyhow::Result<()> {
        use crate::server::usage::lifecycle::{Phase, UsageTrace};
        let session = Session::new().await?;
        let launched = session.command("sh -c 'sleep 3'", true).await?;
        let id = launched["data"]["execution_id"].as_str().context("execution ID")?;
        let listed = session.call("Execution", json!({"action":{"type":"list"}})).await?;
        let cursor = listed["data"]["catalog"]["cursor"].as_str().context("cursor")?;
        let trace = UsageTrace::for_test("Execution", "wait");
        let started = std::time::Instant::now();
        let waited = Box::pin(trace.scope(session.call("Execution",json!({"action":{
            "type":"wait","execution_ids":[id],"after":cursor,"events":["exited"],"timeout_seconds":0.15
        }})))).await?;
        assert_eq!(waited["data"]["timed_out"], true, "{waited}");
        assert!(waited["data"]["changes"]["executions"].as_array().is_some_and(Vec::is_empty));
        let next_cursor =
            waited["data"]["changes"]["cursor"].as_str().context("retained cursor")?;
        assert_ne!(next_cursor, "");
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(trace.test_calls(Phase::SessionAdmission), Some(1));
        assert_eq!(trace.test_calls(Phase::RuntimeInspect), Some(1));
        assert_eq!(trace.test_calls(Phase::RuntimeCapture), None);
        assert_eq!(trace.test_calls(Phase::WaitIdle), Some(1));
        assert!(trace.test_calls(Phase::Catalog).is_some_and(|calls| calls >= 3));
        let polled = session.call("Execution",json!({"action":{
            "type":"wait","execution_ids":[id],"after":next_cursor,"events":["exited"],"timeout_seconds":0
        }})).await?;
        assert_eq!(polled["data"]["timed_out"], true, "{polled}");
        assert_eq!(polled["data"]["changes"]["resync_required"], false);
        session.close().await
    }

    #[tokio::test]
    async fn native_agent_prompt_requires_trusted_bash_authority() -> anyhow::Result<()> {
        let session = Session::new().await?;
        let request = CallToolRequestParams::new("Agent").with_arguments(
            json!({
                "thread_id": session.thread, "workspace_root": session.root,
                "action": "prompt", "target": "invented", "prompt_id": "one", "text": "do work",
                "principal_key": "local", "bash_allowed": true,
            })
            .as_object()
            .context("arguments")?
            .clone(),
        );
        let execution = session
            .service
            .execute_tool_call_with_authority(
                request,
                ShellActionOptions::default(),
                EditPermissionSet::default(),
                super::super::super::workflow_types::WorkflowAuthority {
                    principal_key: "http:reader".into(),
                    bash_allowed: false,
                },
            )
            .await?;
        let result = execution.result.structured_content.context("denied result")?;
        assert_eq!(result["status"], "denied", "{result}");
        assert_eq!(result["data"]["input_sent"], false);
        session.close().await
    }

    #[tokio::test]
    async fn workflow_tools_reject_wrong_workspace_in_local_transport_too() -> anyhow::Result<()> {
        let session = Session::new().await?;
        let other = tempfile::tempdir()?;
        let request = CallToolRequestParams::new("Execution").with_arguments(
            json!({ "thread_id": session.thread, "workspace_root": other.path(),
                "action": { "type": "list" } })
            .as_object()
            .context("arguments")?
            .clone(),
        );
        let result = session
            .service
            .execute_tool_call(request, ShellActionOptions::default(), EditPermissionSet::default())
            .await?
            .result
            .structured_content
            .context("binding result")?;
        assert_ne!(result["status"], "completed", "{result}");
        assert!(result["data"].get("catalog").is_none(), "{result}");
        session.close().await
    }
}

#[tokio::test]
async fn execution_wait_deadline_includes_session_context_acquisition() -> anyhow::Result<()> {
    let service = WinxService::new();
    let directory = tempfile::tempdir()?;
    let thread = format!("execution_wait_context_{:032x}", rand::random::<u128>());
    let (slot, _session_guard) = service.session_for(&thread).await;
    for (timeout_seconds, maximum_ms) in [(0.0_f32, 500), (0.04_f32, 250)] {
        let held = slot.lock().await;
        let request: CallToolRequestParams = serde_json::from_value(json!({
            "name": "Execution",
            "arguments": {
                "thread_id": thread,
                "workspace_root": directory.path(),
                "action": {
                    "type": "wait", "execution_ids": ["watched"],
                    "timeout_seconds": timeout_seconds
                }
            }
        }))?;
        let trace = crate::server::usage::lifecycle::UsageTrace::for_test("Execution", "wait");
        let started = std::time::Instant::now();
        let result = Box::pin(tokio::time::timeout(
            std::time::Duration::from_secs(1),
            trace.scope(service.execute_tool_call(
                request,
                ShellActionOptions::default(),
                EditPermissionSet::default(),
            )),
        ))
        .await;
        let elapsed = started.elapsed();
        drop(held);
        let result = result.map_err(|_| {
            anyhow::anyhow!(
                "Execution wait ignored its deadline while acquiring the session context"
            )
        })??;
        let envelope = result.result.structured_content.unwrap_or_default();
        assert_eq!(envelope["status"], "failed", "{envelope}");
        assert_eq!(envelope["errorCode"], "execution_history_unavailable", "{envelope}");
        assert_eq!(
            trace.test_calls(crate::server::usage::lifecycle::Phase::SessionAdmission),
            Some(1)
        );
        assert_eq!(trace.test_calls(crate::server::usage::lifecycle::Phase::WaitIdle), None);
        assert_eq!(trace.test_calls(crate::server::usage::lifecycle::Phase::Catalog), None);
        let maximum = std::time::Duration::from_millis(maximum_ms);
        assert!(elapsed < maximum, "session admission exceeded wait budget: {elapsed:?}");
    }
    Ok(())
}

#[tokio::test]
async fn native_agent_wait_deadline_includes_session_context_acquisition() -> anyhow::Result<()> {
    let service = WinxService::new();
    let directory = tempfile::tempdir()?;
    let thread = format!("agent_wait_context_{:032x}", rand::random::<u128>());
    let (slot, _session_guard) = service.session_for(&thread).await;
    for (timeout_seconds, maximum_ms) in [(0.0_f32, 500), (0.04_f32, 250)] {
        let held = slot.lock().await;
        let request: CallToolRequestParams = serde_json::from_value(json!({
            "name": "Agent",
            "arguments": {
                "thread_id": thread,
                "workspace_root": directory.path(),
                "backend": "native", "action": "wait", "target": "watched",
                "timeout_seconds": timeout_seconds
            }
        }))?;
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            service.execute_tool_call(
                request,
                ShellActionOptions::default(),
                EditPermissionSet::default(),
            ),
        )
        .await;
        let elapsed = started.elapsed();
        drop(held);
        let result = result.map_err(|_| {
            anyhow::anyhow!("Agent wait ignored its deadline while acquiring the session context")
        })??;
        let envelope = result.result.structured_content.unwrap_or_default();
        assert_eq!(envelope["status"], "failed", "{envelope}");
        assert_eq!(envelope["errorCode"], "execution_history_unavailable", "{envelope}");
        assert!(
            elapsed < std::time::Duration::from_millis(maximum_ms),
            "session admission exceeded wait budget: {elapsed:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn workflow_tools_reject_thread_ids_that_normalize_to_empty() -> anyhow::Result<()> {
    let service = WinxService::new();
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let thread = format!("explicit_workflow_binding_{:032x}", rand::random::<u128>());
    let (slot, _session_guard) = service.session_for(&thread).await;
    let mut state = crate::state::bash_state::BashState::new();
    state.current_thread_id = thread;
    state.workspace_root = root.clone();
    state.cwd = root.clone();
    state.initialized = true;
    *slot.lock().await = Some(state);
    for thread_id in [" ", "---", "\t\n"] {
        for tool in ["Execution", "Agent"] {
            let action = if tool == "Execution" { json!({"type":"list"}) } else { json!("list") };
            let request: CallToolRequestParams = serde_json::from_value(json!({
                "name": tool,
                "arguments": { "thread_id": thread_id, "workspace_root": root, "action": action }
            }))?;
            let result = service
                .execute_tool_call(
                    request,
                    ShellActionOptions::default(),
                    EditPermissionSet::default(),
                )
                .await?
                .result;
            let envelope = result.structured_content.unwrap_or_default();
            assert_ne!(
                envelope["status"], "completed",
                "{tool} used implicit active session: {envelope}"
            );
            assert!(envelope["data"].get("catalog").is_none(), "{envelope}");
        }
    }
    Ok(())
}

#[cfg(unix)]
mod accepted_workspace_regression {
    use super::*;
    use std::sync::Arc;

    use crate::runtime::{
        EmbeddedShellRuntime, ShellRuntime, ShellRuntimeConfigureFuture,
        ShellRuntimeDetailedFuture, ShellRuntimeFuture, ShellRuntimeUnitFuture,
        ShellSessionTransition,
    };
    use crate::state::bash_state::BashState;
    use crate::state::execution_history::ExecutionScope;
    use crate::types::{BashCommand, BashCommandAction};

    #[derive(Default)]
    struct PausedOutcomeRuntime {
        embedded: EmbeddedShellRuntime,
        accepted: tokio::sync::Notify,
        resume: tokio::sync::Notify,
    }

    impl ShellRuntime for PausedOutcomeRuntime {
        fn configure_session<'a>(
            &'a self,
            state: &'a mut BashState,
            transition: ShellSessionTransition,
        ) -> ShellRuntimeConfigureFuture<'a> {
            self.embedded.configure_session(state, transition)
        }

        fn run_action<'a>(
            &'a self,
            state: &'a SharedBashState,
            command: BashCommand,
        ) -> ShellRuntimeFuture<'a> {
            self.embedded.run_action(state, command)
        }

        fn run_action_detailed<'a>(
            &'a self,
            state: &'a SharedBashState,
            command: BashCommand,
            options: ShellActionOptions,
        ) -> ShellRuntimeDetailedFuture<'a> {
            Box::pin(async move {
                let pause = matches!(command.action_json, BashCommandAction::Command { .. });
                let outcome = self.embedded.run_action_detailed(state, command, options).await;
                if pause {
                    self.accepted.notify_one();
                    self.resume.notified().await;
                }
                outcome
            })
        }

        fn interrupt<'a>(&'a self, state: &'a SharedBashState) -> ShellRuntimeUnitFuture<'a> {
            self.embedded.interrupt(state)
        }

        fn terminate_session<'a>(&'a self, thread_id: &'a str) -> ShellRuntimeUnitFuture<'a> {
            self.embedded.terminate_session(thread_id)
        }
    }

    async fn call(
        service: &WinxService,
        name: &'static str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let request: CallToolRequestParams = serde_json::from_value(json!({
            "name": name, "arguments": arguments,
        }))?;
        Ok(service
            .execute_tool_call(request, ShellActionOptions::default(), EditPermissionSet::default())
            .await?
            .result
            .structured_content
            .unwrap_or_default())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accepted_execution_keeps_its_workspace_across_delayed_return_and_later_status(
    ) -> anyhow::Result<()> {
        let original = tempfile::tempdir()?;
        let replacement = tempfile::tempdir()?;
        let original_root = original.path().canonicalize()?;
        let replacement_root = replacement.path().canonicalize()?;
        let thread = format!("accepted_workspace_{:032x}", rand::random::<u128>());
        let runtime = Arc::new(PausedOutcomeRuntime::default());
        let service = Arc::new(WinxService::with_runtime(
            super::super::super::sessions::SessionIsolation::Lenient,
            runtime.clone(),
        ));
        let initialized = call(
            &service,
            "Initialize",
            json!({
                "type": "first_call", "thread_id": thread,
                "any_workspace_path": original_root, "mode_name": "wcgw",
            }),
        )
        .await?;
        assert_eq!(initialized["status"], "completed", "{initialized}");
        let command_service = service.clone();
        let command_thread = thread.clone();
        let command_root = original_root.clone();
        let command = tokio::spawn(async move {
            call(&command_service, "BashCommand", json!({
                "thread_id": command_thread, "workspace_root": command_root,
                "action_json": {"type":"command", "command":"printf original-workspace-output; sleep 5",
                    "is_background": false, "allow_multi": true},
                "wait_for_seconds": 0.0,
            })).await
        });
        let admitted =
            tokio::time::timeout(std::time::Duration::from_secs(3), runtime.accepted.notified())
                .await;
        if admitted.is_err() {
            runtime.resume.notify_one();
            let _ = runtime.terminate_session(&thread).await;
            return Err(anyhow::anyhow!("command did not produce its native outcome"));
        }
        let changed = call(
            &service,
            "Initialize",
            json!({
                "type": "user_asked_change_workspace", "thread_id": thread,
                "any_workspace_path": replacement_root, "mode_name": "wcgw",
            }),
        )
        .await;
        runtime.resume.notify_one();
        let outcome = command.await??;
        let changed = changed?;
        assert_eq!(changed["status"], "completed", "{changed}");
        let id = outcome["data"]["execution_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("execution identity missing: {outcome}"))?
            .to_string();
        // The PTY survives a workspace transition. Later status observations
        // must retain the command's original scope as well as the delayed reply.
        let _status = call(
            &service,
            "BashCommand",
            json!({
                "thread_id": thread, "workspace_root": replacement_root,
                "action_json": {"type":"status_check", "status_check": true},
                "wait_for_seconds": 0.0,
            }),
        )
        .await?;
        let catalog = service.execution_history.catalog()?;
        let now = super::super::super::executions::history::now_ms();
        let in_original = catalog.get(
            &ExecutionScope { thread_id: thread.clone(), workspace_root: original_root },
            &id,
            now,
        )?;
        let in_replacement = catalog.get(
            &ExecutionScope { thread_id: thread.clone(), workspace_root: replacement_root },
            &id,
            now,
        )?;
        runtime.terminate_session(&thread).await?;
        assert!(
            in_original.is_some(),
            "accepted execution vanished from its original workspace: {outcome}"
        );
        assert!(in_replacement.is_none(), "old execution leaked into the replacement workspace");
        Ok(())
    }
}
