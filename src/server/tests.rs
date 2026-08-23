use std::sync::Arc;
use std::time::Duration;

use super::catalog::{schema_to_input_schema, strip_schema_titles, winx_tools};
use super::sessions::{root_uri_to_path, MAX_SESSIONS};
use super::tool_dispatch::audit_summary;
use super::*;
use crate::errors::WinxError;
use crate::runtime::ShellRuntime;
use crate::state::BashState;
use crate::types::BashCommand;

mod audit_tests {
    use super::audit_summary;
    use serde_json::json;

    #[test]
    fn bash_audit_summary_never_contains_command_content() {
        let args = json!({
            "action_json": {
                "type": "command",
                "command": "curl -H 'Authorization: secret-value' https://example.invalid"
            }
        });
        let summary = audit_summary("BashCommand", Some(&args));
        assert!(summary.starts_with("command bytes="));
        assert!(!summary.contains("secret-value"));
        assert!(!summary.contains("example.invalid"));
    }
}

mod session_registry_tests {
    use super::*;

    #[derive(Clone, Default)]
    struct RecordingRuntime {
        terminated: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl ShellRuntime for RecordingRuntime {
        fn configure_session<'a>(
            &'a self,
            _bash_state: &'a mut BashState,
            _transition: crate::runtime::ShellSessionTransition,
        ) -> crate::runtime::ShellRuntimeConfigureFuture<'a> {
            Box::pin(async { Ok(crate::runtime::ShellSessionConfiguration::default()) })
        }

        fn run_action<'a>(
            &'a self,
            _bash_state: &'a SharedBashState,
            _command: BashCommand,
        ) -> crate::runtime::ShellRuntimeFuture<'a> {
            Box::pin(async {
                Err(WinxError::CommandExecutionError(
                    "recording runtime does not execute commands".to_string(),
                ))
            })
        }

        fn interrupt<'a>(
            &'a self,
            _bash_state: &'a SharedBashState,
        ) -> crate::runtime::ShellRuntimeUnitFuture<'a> {
            Box::pin(async { Ok(()) })
        }

        fn terminate_session<'a>(
            &'a self,
            thread_id: &'a str,
        ) -> crate::runtime::ShellRuntimeUnitFuture<'a> {
            let terminated = self.terminated.clone();
            let thread_id = thread_id.to_string();
            Box::pin(async move {
                terminated
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(thread_id);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn distinct_threads_get_distinct_sessions() {
        let service = WinxService::new();
        let (first, _) = service.session_for("thread_a").await;
        let (second, _) = service.session_for("thread_b").await;
        assert!(!Arc::ptr_eq(&first, &second));
        let (first_again, _) = service.session_for("thread_a").await;
        assert!(Arc::ptr_eq(&first, &first_again));
    }

    #[tokio::test]
    async fn empty_thread_id_falls_back_to_last_active() {
        let service = WinxService::new();
        let (_first, _) = service.session_for("thread_a").await;
        let (second, _) = service.session_for("thread_b").await;
        let (fallback, _) = service.session_for("").await;
        assert!(Arc::ptr_eq(&second, &fallback));
        assert!(service.active_slot(None).await.is_some());
    }

    #[tokio::test]
    async fn strict_isolation_empty_thread_id_does_not_steal_active_session() {
        let service = WinxService::with_isolation(SessionIsolation::Strict);
        let (first, _) = service.session_for("thread_a").await;
        let (anonymous, _) = service.session_for("").await;
        assert!(!Arc::ptr_eq(&first, &anonymous));
        let (anonymous_again, _) = service.session_for("").await;
        assert!(Arc::ptr_eq(&anonymous, &anonymous_again));
        let (second, _) = service.session_for("thread_b").await;
        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn lru_eviction_caps_live_sessions() {
        let service = WinxService::new();
        for index in 0..(MAX_SESSIONS + 5) {
            let (_, _) = service.session_for(&format!("t{index}")).await;
        }
        let registry = service.sessions.lock().await;
        assert!(
            registry.slots.len() <= MAX_SESSIONS,
            "session count {} over cap",
            registry.slots.len()
        );
    }

    #[tokio::test]
    async fn lru_eviction_releases_runtime_owned_session() {
        let runtime = RecordingRuntime::default();
        let terminated = runtime.terminated.clone();
        let service = WinxService::with_runtime(SessionIsolation::Lenient, Arc::new(runtime));

        let (_, first_guard) = service.session_for("evict_me").await;
        drop(first_guard);
        tokio::time::sleep(Duration::from_millis(2)).await;
        for index in 0..MAX_SESSIONS {
            let (_, guard) = service.session_for(&format!("replacement_{index}")).await;
            drop(guard);
        }

        let terminated =
            terminated.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        assert_eq!(terminated, vec!["evict_me"]);
    }

    #[tokio::test]
    async fn in_flight_session_is_not_evicted() {
        let service = WinxService::new();
        let (_keep_slot, _keep_guard) = service.session_for("keep").await;
        for index in 0..(MAX_SESSIONS + 10) {
            let (_, _) = service.session_for(&format!("filler{index}")).await;
        }
        let registry = service.sessions.lock().await;
        assert!(
            registry.slots.contains_key("keep"),
            "an in-flight session must survive LRU eviction churn"
        );
    }
}

mod task_lifecycle_tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[derive(Clone, Default)]
    struct PromotionContractRuntime {
        interrupted: Arc<std::sync::Mutex<Vec<crate::runtime::ShellExecutionToken>>>,
        terminated: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl ShellRuntime for PromotionContractRuntime {
        fn configure_session<'a>(
            &'a self,
            _bash_state: &'a mut BashState,
            _transition: crate::runtime::ShellSessionTransition,
        ) -> crate::runtime::ShellRuntimeConfigureFuture<'a> {
            Box::pin(async { Ok(crate::runtime::ShellSessionConfiguration::default()) })
        }

        fn run_action<'a>(
            &'a self,
            _bash_state: &'a SharedBashState,
            _command: BashCommand,
        ) -> crate::runtime::ShellRuntimeFuture<'a> {
            Box::pin(async {
                Err(WinxError::CommandExecutionError(
                    "promotion test does not execute commands".to_string(),
                ))
            })
        }

        fn interrupt<'a>(
            &'a self,
            _bash_state: &'a SharedBashState,
        ) -> crate::runtime::ShellRuntimeUnitFuture<'a> {
            Box::pin(async { Ok(()) })
        }

        fn interrupt_execution<'a>(
            &'a self,
            _bash_state: &'a SharedBashState,
            expected: Option<crate::runtime::ShellExecutionToken>,
        ) -> crate::runtime::ShellRuntimeBoolFuture<'a> {
            let interrupted = self.interrupted.clone();
            Box::pin(async move {
                if let Some(execution) = expected {
                    interrupted
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(execution);
                    Ok(true)
                } else {
                    Ok(false)
                }
            })
        }

        fn terminate_session<'a>(
            &'a self,
            thread_id: &'a str,
        ) -> crate::runtime::ShellRuntimeUnitFuture<'a> {
            let terminated = self.terminated.clone();
            let thread_id = thread_id.to_string();
            Box::pin(async move {
                terminated
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(thread_id);
                Ok(())
            })
        }
    }

    #[derive(Clone, Default)]
    struct PollFailureAfterBindingRuntime {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        current_generation: Arc<std::sync::atomic::AtomicU64>,
        interrupted: Arc<std::sync::Mutex<Vec<crate::runtime::ShellExecutionToken>>>,
        next_generation_interrupted: Arc<std::sync::atomic::AtomicBool>,
    }

    impl ShellRuntime for PollFailureAfterBindingRuntime {
        fn configure_session<'a>(
            &'a self,
            _bash_state: &'a mut BashState,
            _transition: crate::runtime::ShellSessionTransition,
        ) -> crate::runtime::ShellRuntimeConfigureFuture<'a> {
            Box::pin(async { Ok(crate::runtime::ShellSessionConfiguration::default()) })
        }

        fn run_action<'a>(
            &'a self,
            _bash_state: &'a SharedBashState,
            _command: BashCommand,
        ) -> crate::runtime::ShellRuntimeFuture<'a> {
            Box::pin(async {
                Err(WinxError::CommandExecutionError("use detailed task runtime".to_string()))
            })
        }

        fn run_action_detailed<'a>(
            &'a self,
            _bash_state: &'a SharedBashState,
            _command: BashCommand,
            _options: crate::runtime::ShellActionOptions,
        ) -> crate::runtime::ShellRuntimeDetailedFuture<'a> {
            let calls = Arc::clone(&self.calls);
            let current_generation = Arc::clone(&self.current_generation);
            Box::pin(async move {
                let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call > 0 {
                    // Model a second client starting generation 2 after
                    // generation 1 completed but before the Task's first poll
                    // response was delivered.
                    current_generation.store(2, std::sync::atomic::Ordering::SeqCst);
                    return Err(WinxError::CommandExecutionError(
                        "simulated status transport failure".to_string(),
                    ));
                }
                current_generation.store(1, std::sync::atomic::Ordering::SeqCst);
                let mut result = crate::runtime::BashCommandRuntimeResult::legacy(
                    crate::tools::bash_command::BashCommandResult {
                        output: "generation-one-running".to_string(),
                        state: crate::tools::bash_command::BashCommandState {
                            process_status: crate::tools::bash_command::BashProcessStatus::Running,
                            background_id: None,
                            running_for_seconds: Some(0),
                            exit_code: None,
                            cwd: std::env::temp_dir(),
                            turn_state: None,
                        },
                    },
                );
                result.command_generation = Some(1);
                result.execution_token = Some(crate::runtime::ShellExecutionToken {
                    guardian_epoch: "guardian-a".to_string(),
                    session_epoch: "session-a".to_string(),
                    generation: 1,
                });
                result.generation_bound_actions = true;
                Ok(result)
            })
        }

        fn supports_generation_bound_actions(&self) -> crate::runtime::ShellRuntimeBoolFuture<'_> {
            Box::pin(async { Ok(true) })
        }

        fn interrupt<'a>(
            &'a self,
            _bash_state: &'a SharedBashState,
        ) -> crate::runtime::ShellRuntimeUnitFuture<'a> {
            Box::pin(async { Ok(()) })
        }

        fn interrupt_execution<'a>(
            &'a self,
            _bash_state: &'a SharedBashState,
            expected: Option<crate::runtime::ShellExecutionToken>,
        ) -> crate::runtime::ShellRuntimeBoolFuture<'a> {
            let current = Arc::clone(&self.current_generation);
            let interrupted = Arc::clone(&self.interrupted);
            let next_interrupted = Arc::clone(&self.next_generation_interrupted);
            Box::pin(async move {
                let Some(expected) = expected else { return Ok(false) };
                interrupted
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(expected.clone());
                let matches =
                    current.load(std::sync::atomic::Ordering::SeqCst) == expected.generation;
                if matches && expected.generation == 2 {
                    next_interrupted.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                Ok(matches)
            })
        }

        fn terminate_session<'a>(
            &'a self,
            _thread_id: &'a str,
        ) -> crate::runtime::ShellRuntimeUnitFuture<'a> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn failed_promotion_interrupts_full_execution_token_and_releases_reservation() {
        let runtime = PromotionContractRuntime::default();
        let interrupted = runtime.interrupted.clone();
        let service = WinxService::with_runtime(SessionIsolation::Lenient, Arc::new(runtime));
        let (slot, guard) = service.session_for("promotion-contract").await;
        *slot.lock().await = Some(BashState::new());
        drop(guard);
        let request = rmcp::model::CallToolRequestParams::new("BashCommand").with_arguments(
            serde_json::json!({
                "action_json": {
                    "type": "command",
                    "command": "sleep 30",
                    "is_background": false
                },
                "thread_id": "promotion-contract"
            })
            .as_object()
            .expect("request object")
            .clone(),
        );
        let reservation = service
            .reserve_bash_task(&request, &crate::server::principal::RequestScope::default())
            .await
            .expect("reservation");
        let task_id = reservation.task_id.clone();
        let mut result =
            rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text("running")]);
        result.structured_content = Some(serde_json::json!({"status": "running"}));
        let initial = crate::server::tool_dispatch::ToolCallExecution {
            result,
            command_generation: Some(41),
            execution_token: Some(crate::runtime::ShellExecutionToken {
                guardian_epoch: "embedded".to_string(),
                session_epoch: "test-session".to_string(),
                generation: 41,
            }),
            generation_bound_actions: false,
        };

        let error = service
            .start_reserved_bash_task(
                reservation,
                request,
                crate::server::principal::RequestScope::default(),
                Some(initial),
                false,
            )
            .await
            .expect_err("promotion must reject a changed runtime capability");
        assert!(error.message.contains("safely promote"), "{error:?}");
        assert!(service.tasks.lock().await.get(&task_id).is_none());
        let interrupted = interrupted.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].guardian_epoch, "embedded");
        assert_eq!(interrupted[0].session_epoch, "test-session");
        assert_eq!(interrupted[0].generation, 41);
    }

    #[tokio::test]
    async fn immediate_task_launch_error_remains_failed_until_ttl_without_terminating_session() {
        let runtime = PromotionContractRuntime::default();
        let terminated = runtime.terminated.clone();
        let service = WinxService::with_runtime(SessionIsolation::Lenient, Arc::new(runtime));
        let (slot, guard) = service.session_for("immediate-launch-error").await;
        *slot.lock().await = Some(BashState::new());
        drop(guard);
        let request = rmcp::model::CallToolRequestParams::new("BashCommand").with_arguments(
            serde_json::json!({
                "action_json": {
                    "type": "command",
                    "command": "sleep 30",
                    "is_background": false
                },
                "thread_id": "immediate-launch-error"
            })
            .as_object()
            .expect("request object")
            .clone(),
        );
        let reservation = service
            .reserve_bash_task(&request, &crate::server::principal::RequestScope::default())
            .await
            .expect("reservation");
        let task_id = reservation.task_id.clone();
        service
            .start_reserved_bash_task(
                reservation,
                request,
                crate::server::principal::RequestScope::default(),
                None,
                false,
            )
            .await
            .expect("Task response is created before worker launch");

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while service
            .tasks
            .lock()
            .await
            .get(&task_id)
            .is_some_and(|entry| entry.task.status == rmcp::model::TaskStatus::Working)
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            service.tasks.lock().await.get(&task_id).map(|entry| entry.task.status),
            Some(rmcp::model::TaskStatus::Failed)
        );
        let terminated = terminated.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(terminated.is_empty(), "invalid/tool errors must not terminate the session");
    }

    #[tokio::test]
    async fn abnormal_poll_cleans_exact_token_without_interrupting_next_client_generation() {
        let runtime = PollFailureAfterBindingRuntime::default();
        let interrupted = Arc::clone(&runtime.interrupted);
        let next_interrupted = Arc::clone(&runtime.next_generation_interrupted);
        let service = WinxService::with_runtime(SessionIsolation::Lenient, Arc::new(runtime));
        let (slot, guard) = service.session_for("abnormal-bound-task").await;
        *slot.lock().await = Some(BashState::new());
        drop(guard);
        let request = rmcp::model::CallToolRequestParams::new("BashCommand").with_arguments(
            serde_json::json!({
                "action_json": {
                    "type": "command",
                    "command": "first-client-generation",
                    "is_background": false
                },
                "thread_id": "abnormal-bound-task"
            })
            .as_object()
            .expect("request object")
            .clone(),
        );
        let reservation = service
            .reserve_bash_task(&request, &crate::server::principal::RequestScope::default())
            .await
            .expect("reservation");
        let task_id = reservation.task_id.clone();
        service
            .start_reserved_bash_task(
                reservation,
                request,
                crate::server::principal::RequestScope::default(),
                None,
                false,
            )
            .await
            .expect("returned Task");

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while service
            .tasks
            .lock()
            .await
            .get(&task_id)
            .is_some_and(|entry| entry.task.status == rmcp::model::TaskStatus::Working)
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            service.tasks.lock().await.get(&task_id).map(|entry| entry.task.status),
            Some(rmcp::model::TaskStatus::Failed),
            "returned Task must retain its terminal failure"
        );
        let interrupted = interrupted.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].generation, 1);
        assert!(!next_interrupted.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn task_capacity_is_reserved_before_any_adaptive_runtime_action() {
        let service = WinxService::with_runtime(
            SessionIsolation::Lenient,
            Arc::new(PromotionContractRuntime::default()),
        );
        let request = rmcp::model::CallToolRequestParams::new("BashCommand").with_arguments(
            serde_json::json!({
                "action_json": {
                    "type": "command",
                    "command": "must-not-run",
                    "is_background": false
                },
                "thread_id": "deterministic-saturation"
            })
            .as_object()
            .expect("request object")
            .clone(),
        );
        let scope = crate::server::principal::RequestScope::default();
        let mut reservations = Vec::new();
        for _ in 0..crate::state::task_state::MAX_TASKS {
            reservations.push(
                service
                    .reserve_bash_task(&request, &scope)
                    .await
                    .expect("working registry reservation"),
            );
        }
        let error = service
            .reserve_bash_task(&request, &scope)
            .await
            .err()
            .expect("full working registry must reject before runtime execution");
        assert!(error.message.contains("task limit reached"), "{error:?}");
        assert_eq!(reservations.len(), crate::state::task_state::MAX_TASKS);
    }

    #[tokio::test]
    async fn running_without_execution_binding_terminates_and_releases_reservation() {
        let runtime = PromotionContractRuntime::default();
        let terminated = runtime.terminated.clone();
        let service = WinxService::with_runtime(SessionIsolation::Lenient, Arc::new(runtime));
        let request = rmcp::model::CallToolRequestParams::new("BashCommand").with_arguments(
            serde_json::json!({
                "command": "sleep 30",
                "thread_id": "unbound-running"
            })
            .as_object()
            .expect("request object")
            .clone(),
        );
        let reservation = service
            .reserve_bash_task(&request, &crate::server::principal::RequestScope::default())
            .await
            .expect("reservation");
        let task_id = reservation.task_id.clone();
        let mut result =
            rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text("running")]);
        result.structured_content = Some(serde_json::json!({"status": "running"}));
        let initial = crate::server::tool_dispatch::ToolCallExecution {
            result,
            command_generation: None,
            execution_token: None,
            generation_bound_actions: true,
        };

        service
            .start_reserved_bash_task(
                reservation,
                request,
                crate::server::principal::RequestScope::default(),
                Some(initial),
                false,
            )
            .await
            .expect_err("unbound running promotion must fail");
        assert!(service.tasks.lock().await.get(&task_id).is_none());
        let terminated = terminated.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(terminated.len(), 1);
        assert!(terminated[0].starts_with("unboundrunning_"), "{terminated:?}");
    }

    #[tokio::test]
    async fn interrupt_waits_until_main_shell_is_reusable() {
        let service = WinxService::new();
        let initialized = service
            .handle_initialize(Some(serde_json::json!({
                "type": "first_call",
                "any_workspace_path": std::env::temp_dir(),
                "mode_name": "wcgw",
                "thread_id": "task_cancel_regression"
            })))
            .await;
        assert!(initialized.is_ok(), "test session failed to initialize: {initialized:?}");

        let worker = {
            let service = service.clone();
            tokio::spawn(async move {
                service
                    .handle_bash_command(Some(serde_json::json!({
                        "action_json": {
                            "type": "command",
                            "command": "sleep 30",
                            "is_background": false
                        },
                        "thread_id": "task_cancel_regression"
                    })))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        let execution = {
            let slot = service.sessions.lock().await.slots.get("task_cancel_regression").cloned();
            let slot = slot.expect("initialized session");
            let state = slot.lock().await;
            let shell = state.as_ref().expect("initialized state").pty_shell.lock().await;
            shell.as_ref().map(|shell| crate::runtime::ShellExecutionToken {
                guardian_epoch: "embedded".to_string(),
                session_epoch: format!("{:016x}", shell.incarnation()),
                generation: shell.command_generation(),
            })
        };
        worker.abort();
        service.interrupt_task_execution("task_cancel_regression", execution).await;
        let interrupted = tokio::time::timeout(Duration::from_secs(5), worker).await;
        let settled = match interrupted {
            Ok(Ok(_)) => true,
            Ok(Err(error)) => error.is_cancelled(),
            Err(_) => false,
        };
        assert!(settled, "aborted task worker did not settle as completed or cancelled");

        let recovery = service
            .handle_bash_command(Some(serde_json::json!({
                "action_json": {
                    "type": "command",
                    "command": "printf task-shell-recovered",
                    "is_background": false
                },
                "thread_id": "task_cancel_regression"
            })))
            .await;
        let rendered = format!("{recovery:?}");
        assert!(
            recovery.is_ok() && rendered.contains("task-shell-recovered"),
            "shell was not reusable after cancellation: {rendered}"
        );
    }
}

mod schema_tests {
    #![allow(clippy::expect_used)]

    use super::{root_uri_to_path, schema_to_input_schema, strip_schema_titles, winx_tools};
    use rmcp::model::ProtocolVersion;
    use rmcp::ServerHandler;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn strips_titles_from_schema_nodes_only() {
        let mut value = json!({
            "type": "object",
            "title": "ShouldGo",
            "properties": {
                "title": { "type": "string", "title": "InnerGoes" }
            }
        });
        strip_schema_titles(&mut value);
        assert!(value.get("title").is_none(), "schema-node title not stripped");
        let properties = value.get("properties").and_then(serde_json::Value::as_object);
        assert!(
            properties.is_some_and(|properties| properties.contains_key("title")),
            "property key named 'title' must be preserved"
        );
        assert!(
            properties
                .and_then(|properties| properties.get("title"))
                .and_then(|title| title.get("title"))
                .is_none(),
            "inner schema title not stripped"
        );
    }

    #[test]
    fn real_tool_schema_carries_no_titles() {
        let schema = schema_to_input_schema::<crate::types::Initialize>();
        let blob = serde_json::to_string(&*schema).unwrap_or_default();
        assert!(!blob.contains("\"title\""), "tool schema still contains titles: {blob}");
    }

    #[test]
    fn initialize_schema_has_no_dangling_refs() {
        let schema = schema_to_input_schema::<crate::types::Initialize>();
        let blob = serde_json::to_string(&*schema).unwrap_or_default();
        assert!(
            !blob.contains("#/definitions/"),
            "schema has a draft-07 #/definitions ref: {blob}"
        );
        assert!(
            !blob.contains("definitions/ModeName"),
            "ModeName is referenced but never defined: {blob}"
        );
        assert!(
            blob.contains("wcgw") && blob.contains("architect") && blob.contains("code_writer"),
            "mode_name enum not inlined: {blob}"
        );
    }

    #[test]
    fn bash_schema_advertises_generic_wait_policies() {
        let bash = winx_tools()
            .into_iter()
            .find(|tool| tool.name == "BashCommand")
            .expect("BashCommand catalog entry");
        let blob = serde_json::to_string(&*bash.input_schema).unwrap_or_default();
        assert!(blob.contains("wait_policy"), "wait_policy missing: {blob}");
        for policy in ["adaptive", "return_early", "until_complete"] {
            assert!(blob.contains(policy), "{policy} missing: {blob}");
        }
    }

    #[test]
    fn advertises_latest_protocol_and_tasks_extension() {
        let info = ServerHandler::get_info(&super::WinxService::new());
        assert_eq!(info.protocol_version, ProtocolVersion::V_2026_07_28);
        assert!(info.capabilities.supports_tasks());
        assert!(info.capabilities.extensions.as_ref().is_some_and(|extensions| {
            extensions.contains_key(super::handler::COMPACT_BASH_OUTPUT_EXTENSION)
        }));
    }

    #[test]
    fn every_tool_advertises_orchestration_output_schema() {
        let tools = winx_tools();
        assert_eq!(tools.len(), 9);
        for tool in &tools {
            let output = tool.output_schema.as_ref().expect("tool outputSchema");
            let properties = output
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .expect("outputSchema properties");
            assert!(properties.contains_key("status"), "{} has no status: {output:?}", tool.name);
            assert!(
                properties.contains_key("retrySameCall"),
                "{} has no retrySameCall: {output:?}",
                tool.name
            );
            let blob = serde_json::to_string(output).unwrap_or_default();
            assert!(!blob.contains("\"format\":\"uint\""), "unsupported uint format: {blob}");
        }

        let code_map = tools.iter().find(|tool| tool.name == "CodeMap").expect("CodeMap");
        let properties = code_map
            .output_schema
            .as_ref()
            .and_then(|output| output.get("properties"))
            .and_then(serde_json::Value::as_object)
            .expect("CodeMap outputSchema properties");
        assert!(properties.contains_key("truncated"));
    }

    #[test]
    fn handshake_instructions_lead_with_recovery_contract() {
        let info = ServerHandler::get_info(&super::WinxService::new());
        let instructions = info.instructions.expect("server instructions");
        let prefix = instructions.chars().take(512).collect::<String>();
        assert!(prefix.contains("structuredContent.status"));
        assert!(prefix.contains("Never repeat a failed tool call unchanged"));
        assert!(prefix.contains("required_read"));
    }

    #[test]
    fn local_root_uri_decoding_rejects_remote_authorities() {
        assert_eq!(
            root_uri_to_path("file:///tmp/a%20project"),
            Some(PathBuf::from("/tmp/a project"))
        );
        assert_eq!(
            root_uri_to_path("file://localhost/tmp/project"),
            Some(PathBuf::from("/tmp/project"))
        );
        assert_eq!(root_uri_to_path("file://remote.example/tmp/project"), None);
        assert_eq!(root_uri_to_path("https://example.com/project"), None);
    }
}

mod discovery_protocol_tests {
    #![allow(clippy::expect_used)]

    use super::WinxService;
    use rmcp::{transport::async_rw::AsyncRwTransport, ServiceExt};
    use serde_json::Value;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    async fn stdio_tools_list(protocol_version: &str) -> Value {
        let (client, server) = tokio::io::duplex(256 * 1024);
        let (server_read, server_write) = tokio::io::split(server);
        let transport = AsyncRwTransport::new_server(server_read, server_write);
        let task = tokio::spawn(async move {
            let running = WinxService::new()
                .serve(transport)
                .await
                .expect("server should accept stdio initialization");
            running.waiting().await.expect("server should shut down cleanly");
        });

        let (client_read, mut client_write) = tokio::io::split(client);
        let mut client_read = BufReader::new(client_read);
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": protocol_version,
                "capabilities": {},
                "clientInfo": { "name": "stdio-compat-test", "version": "1" }
            }
        });
        client_write
            .write_all(format!("{initialize}\n").as_bytes())
            .await
            .expect("write initialize request");

        let mut line = String::new();
        client_read.read_line(&mut line).await.expect("read initialize response");
        let initialize_response: Value =
            serde_json::from_str(&line).expect("valid initialize response");
        assert_eq!(initialize_response["id"], 1);
        assert_eq!(initialize_response["result"]["protocolVersion"], protocol_version);

        client_write
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
            )
            .await
            .expect("write initialized notification and tools/list request");

        line.clear();
        client_read.read_line(&mut line).await.expect("read tools/list response");
        let response = serde_json::from_str(&line).expect("valid tools/list response");

        drop(client_read);
        drop(client_write);
        task.await.expect("server task");
        response
    }

    #[tokio::test]
    async fn modern_stdio_tools_list_serializes_required_cache_hints() {
        let response = stdio_tools_list("2026-07-28").await;

        assert_eq!(response["id"], 2);
        assert_eq!(response["result"]["resultType"], "complete");
        assert_eq!(response["result"]["ttlMs"], 0);
        assert_eq!(response["result"]["cacheScope"], "public");
        assert!(response["result"]["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "BashCommand")));
    }

    #[tokio::test]
    async fn legacy_stdio_tools_list_omits_modern_cache_hints() {
        for protocol_version in ["2025-11-25", "2025-06-18", "2025-03-26"] {
            let response = stdio_tools_list(protocol_version).await;

            assert_eq!(response["id"], 2, "protocol {protocol_version}: {response}");
            assert!(
                response["result"].get("ttlMs").is_none(),
                "protocol {protocol_version}: {response}"
            );
            assert!(
                response["result"].get("cacheScope").is_none(),
                "protocol {protocol_version}: {response}"
            );
        }
    }

    #[tokio::test]
    async fn discover_probe_negotiates_modern_stateless_lifecycle() {
        let (client, server) = tokio::io::duplex(32 * 1024);
        let (server_read, server_write) = tokio::io::split(server);
        let transport = AsyncRwTransport::new_server(server_read, server_write);
        let task = tokio::spawn(async move {
            let running = WinxService::new()
                .serve(transport)
                .await
                .expect("server should accept modern discovery");
            running.waiting().await.expect("server should shut down cleanly");
        });

        let (client_read, mut client_write) = tokio::io::split(client);
        let mut client_read = BufReader::new(client_read);
        client_write
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"server/discover\",\"params\":{\"_meta\":{\"io.modelcontextprotocol/protocolVersion\":\"2026-07-28\",\"io.modelcontextprotocol/clientInfo\":{\"name\":\"discovery-test\",\"version\":\"1\"},\"io.modelcontextprotocol/clientCapabilities\":{}}}}\n",
            )
            .await
            .expect("write discovery probe");
        let mut line = String::new();
        client_read.read_line(&mut line).await.expect("read discovery response");
        let response: Value = serde_json::from_str(&line).expect("valid discovery response");
        assert_eq!(response["id"], 1);
        assert!(response["result"]["supportedVersions"]
            .as_array()
            .is_some_and(|versions| versions.iter().any(|version| version == "2026-07-28")));

        drop(client_read);
        drop(client_write);
        task.await.expect("server task");
    }
}

mod error_mapping_tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use rmcp::model::ErrorCode;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn recoverable_execution_failures_are_visible_tool_results() {
        let error = WinxError::FileAccessError {
            path: PathBuf::from("/workspace/file.rs"),
            message: "This file exists but hasn't been read in this session.".into(),
        };
        let result = super::outcomes::tool_failure(
            "FileWriteOrEdit",
            &error,
            Some(&json!({"thread_id":"thread","file_path":"/workspace/file.rs"})),
        )
        .expect("tool-level result");
        assert_eq!(result.is_error, Some(true));
        let structured = result.structured_content.expect("structured recovery");
        assert_eq!(structured["status"], "needs_read");
        assert_eq!(structured["nextAction"]["tool"], "ReadFiles");
        assert_eq!(structured["retrySameCall"], false);
    }

    #[test]
    fn serialization_failures_remain_protocol_internal_errors() {
        let failure = WinxError::SerializationError("cannot encode result".into());
        let error = super::outcomes::tool_failure("Tool", &failure, None)
            .expect_err("serialization must remain a protocol error");
        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
    }
}

#[cfg(feature = "loom")]
mod loom_tests {
    use loom::sync::atomic::{AtomicUsize, Ordering};
    use loom::sync::Arc;

    #[derive(Clone)]
    struct PinModel {
        count: Arc<AtomicUsize>,
    }

    struct GuardModel {
        count: Arc<AtomicUsize>,
    }

    impl PinModel {
        fn new() -> Self {
            Self { count: Arc::new(AtomicUsize::new(0)) }
        }

        fn acquire(&self) -> GuardModel {
            self.count.fetch_add(1, Ordering::SeqCst);
            GuardModel { count: self.count.clone() }
        }

        fn is_pinned(&self) -> bool {
            self.count.load(Ordering::SeqCst) > 0
        }
    }

    impl Drop for GuardModel {
        fn drop(&mut self) {
            self.count.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn loom_concurrent_guards_balance_to_zero() {
        loom::model(|| {
            let pin = PinModel::new();
            let first = pin.clone();
            let second = pin.clone();
            let first_handle = loom::thread::spawn(move || drop(first.acquire()));
            let second_handle = loom::thread::spawn(move || drop(second.acquire()));
            assert!(first_handle.join().is_ok());
            assert!(second_handle.join().is_ok());
            assert_eq!(pin.count.load(Ordering::SeqCst), 0, "pin must settle back to 0");
        });
    }

    #[test]
    fn loom_live_guard_always_reads_pinned() {
        loom::model(|| {
            let pin = PinModel::new();
            let observer = pin.clone();
            let held = pin.acquire();
            let worker = pin.clone();
            let handle = loom::thread::spawn(move || drop(worker.acquire()));
            assert!(observer.is_pinned(), "session with a live guard must read pinned");
            assert!(handle.join().is_ok());
            assert!(observer.is_pinned(), "still pinned while the first guard lives");
            drop(held);
            assert!(!observer.is_pinned(), "all guards gone -> unpinned");
        });
    }
}
