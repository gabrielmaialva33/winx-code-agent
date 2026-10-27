#![allow(clippy::expect_used)]

use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

use super::*;
use crate::errors::WinxError;
use crate::runtime::workflow::WorkflowObservation;
use crate::runtime::{
    ShellExecutionToken, ShellRuntimeConfigureFuture, ShellRuntimeDetailedFuture,
    ShellRuntimeFuture, ShellRuntimeUnitFuture, ShellSessionTransition,
};
use crate::state::bash_state::BashState;
use crate::state::execution_history::OutputSource;
use crate::state::turn::{TurnExplanation, TurnState};
use crate::tools::bash_command::{BashCommandResult, BashCommandState, BashProcessStatus};

pub(in crate::server::executions) fn history_at(root: PathBuf) -> WorkflowHistory {
    WorkflowHistory {
        catalog: Some(ExecutionCatalog::new(root, HistoryLimits::default())),
        observing: Mutex::new(HashSet::new()),
        _temporary: None,
    }
}

struct ScriptedRuntime {
    replies: StdMutex<VecDeque<(bool, Option<BashCommandRuntimeResult>)>>,
    expected: ShellExecutionToken,
}

impl ShellRuntime for ScriptedRuntime {
    fn configure_session<'a>(
        &'a self,
        _state: &'a mut BashState,
        _transition: ShellSessionTransition,
    ) -> ShellRuntimeConfigureFuture<'a> {
        Box::pin(async { Err(WinxError::BashStateNotInitialized) })
    }

    fn run_action<'a>(
        &'a self,
        _state: &'a SharedBashState,
        _command: BashCommand,
    ) -> ShellRuntimeFuture<'a> {
        Box::pin(async { Err(WinxError::BashStateNotInitialized) })
    }

    fn run_action_detailed<'a>(
        &'a self,
        _state: &'a SharedBashState,
        command: BashCommand,
        options: ShellActionOptions,
    ) -> ShellRuntimeDetailedFuture<'a> {
        Box::pin(async move {
            assert_eq!(options.expected_execution.as_ref(), Some(&self.expected));
            assert!(options.require_generation_binding);
            let (included, reply) = self
                .replies
                .lock()
                .expect("reply lock")
                .pop_front()
                .expect("unexpected runtime call");
            assert!(matches!(command.action_json,
                BashCommandAction::Inspect { include_output: Some(actual), .. } if actual == included));
            reply.ok_or(WinxError::BashStateNotInitialized)
        })
    }

    fn interrupt<'a>(&'a self, _state: &'a SharedBashState) -> ShellRuntimeUnitFuture<'a> {
        Box::pin(async { Err(WinxError::BashStateNotInitialized) })
    }

    fn terminate_session<'a>(&'a self, _thread: &'a str) -> ShellRuntimeUnitFuture<'a> {
        Box::pin(async { Err(WinxError::BashStateNotInitialized) })
    }
}

fn outcome(included: bool, exit: Option<i32>, seq: u64, text: &str) -> BashCommandRuntimeResult {
    BashCommandRuntimeResult {
        result: BashCommandResult {
            output: String::new(),
            state: BashCommandState {
                process_status: if exit.is_some() {
                    BashProcessStatus::Exited
                } else {
                    BashProcessStatus::Running
                },
                background_id: None,
                running_for_seconds: None,
                exit_code: exit,
                cwd: PathBuf::from("/workspace"),
                turn_state: None,
            },
        },
        compact_output: None,
        command_generation: Some(1),
        execution_token: Some(ShellExecutionToken {
            guardian_epoch: "guardian".into(),
            session_epoch: "session".into(),
            generation: 1,
        }),
        action_context: None,
        generation_bound_actions: true,
        dropped_output_file: None,
        output_truncated: false,
        workflow: Some(WorkflowObservation {
            output: text.into(),
            output_included: included,
            screen: Vec::new(),
            activity_seq: seq,
            explanation: TurnExplanation {
                state: TurnState::Unknown,
                recognizer: "generic".into(),
                source: "test".into(),
                matched_rule: None,
                priority: None,
                reason: None,
            },
            prompt_receipt: None,
        }),
    }
}

#[tokio::test]
async fn failed_full_inspection_is_retried_without_new_activity_including_after_exit(
) -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    for exit in [None, Some(7)] {
        let scope = ExecutionScope {
            thread_id: format!("retry-{exit:?}"),
            workspace_root: temporary.path().into(),
        };
        let catalog =
            ExecutionCatalog::new(temporary.path().join("history"), HistoryLimits::default());
        let initial = outcome(true, None, 1, "old");
        let record = catalog.record(&scope, capture(&initial, None).expect("initial capture"))?;
        let runtime = ScriptedRuntime {
            expected: initial.execution_token.expect("execution token"),
            replies: StdMutex::new(VecDeque::from([
                (false, Some(outcome(false, exit, 2, ""))),
                (true, None),
                (false, Some(outcome(false, exit, 2, ""))),
                (true, Some(outcome(true, exit, 2, "complete final output"))),
            ])),
        };
        let slot = Arc::new(Mutex::new(None));
        let pending = refresh_record(&runtime, &slot, &catalog, &scope, &record, None).await?;
        assert_eq!(serde_json::to_value(&pending)?["output_pending"], true);
        assert_eq!(pending.exit_code, exit);
        let complete = refresh_record(&runtime, &slot, &catalog, &scope, &pending, None).await?;
        assert_eq!(serde_json::to_value(&complete)?["output_pending"], false);
        assert_eq!(complete.exit_code, exit);
        assert_eq!(
            catalog
                .read(&scope, &record.execution_id, OutputSource::Output, None, 1024, now_ms())?
                .text,
            "complete final output"
        );
        assert!(runtime.replies.lock().expect("reply lock").is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn pending_terminal_output_does_not_erase_exit_when_runtime_is_gone() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let scope =
        ExecutionScope { thread_id: "gone".into(), workspace_root: temporary.path().into() };
    let catalog = ExecutionCatalog::new(temporary.path().join("history"), HistoryLimits::default());
    let initial = outcome(true, None, 1, "retained prefix");
    catalog.record(&scope, capture(&initial, None).expect("initial capture"))?;
    let terminal = outcome(false, Some(9), 2, "");
    let pending = catalog.record(&scope, capture(&terminal, None).expect("terminal capture"))?;
    let runtime = ScriptedRuntime {
        expected: initial.execution_token.expect("execution token"),
        replies: StdMutex::new(VecDeque::from([(false, None)])),
    };
    let slot = Arc::new(Mutex::new(None));
    let retained = refresh_record(&runtime, &slot, &catalog, &scope, &pending, None).await?;
    assert_eq!(retained.process_status, ExecutionProcessStatus::Exited);
    assert_eq!(retained.exit_code, Some(9));
    assert_eq!(serde_json::to_value(&retained)?["output_pending"], true);
    assert!(runtime.replies.lock().expect("reply lock").is_empty());
    assert_eq!(
        catalog
            .read(&scope, &retained.execution_id, OutputSource::Output, None, 1024, now_ms())?
            .text,
        "retained prefix"
    );
    Ok(())
}
