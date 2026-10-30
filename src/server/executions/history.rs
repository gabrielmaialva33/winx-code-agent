//! Non-consuming runtime capture and bounded durable history I/O.
use crate::server::usage::lifecycle::{phase, Phase};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;
use tracing::warn;

use super::super::{SharedBashState, WinxService};
use crate::runtime::{BashCommandRuntimeResult, ShellActionOptions, ShellRuntime};
use crate::state::execution_history::{
    CapturedOutput, ExecutionCapture, ExecutionCatalog, ExecutionProcessStatus, ExecutionRecord,
    ExecutionScope, HistoryError, HistoryLimits, HistoryResult,
};
use crate::types::{BashCommand, BashCommandAction};

pub(in crate::server) struct WorkflowHistory {
    catalog: Option<ExecutionCatalog>,
    observing: Mutex<HashSet<(String, PathBuf, String)>>,
    #[cfg(test)]
    _temporary: Option<tempfile::TempDir>,
}

impl Default for WorkflowHistory {
    fn default() -> Self {
        #[cfg(test)]
        let temporary = tempfile::tempdir().ok();
        #[cfg(test)]
        let root = temporary.as_ref().map(|directory| directory.path().to_path_buf());
        #[cfg(not(test))]
        let root = home::home_dir().map(|home| home.join(".local/share/winx/executions"));
        Self {
            catalog: root.map(|root| ExecutionCatalog::new(root, HistoryLimits::default())),
            observing: Mutex::new(HashSet::new()),
            #[cfg(test)]
            _temporary: temporary,
        }
    }
}

impl WorkflowHistory {
    pub(in crate::server) fn catalog(&self) -> HistoryResult<ExecutionCatalog> {
        self.catalog.clone().ok_or_else(|| {
            HistoryError::Io(std::io::Error::other(
                "a private execution history directory is unavailable",
            ))
        })
    }
}

pub(in crate::server) fn now_ms() -> u64 {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

pub(in crate::server) async fn history_call<T, F>(
    catalog: ExecutionCatalog,
    operation: F,
) -> HistoryResult<T>
where
    T: Send + 'static,
    F: FnOnce(&ExecutionCatalog) -> HistoryResult<T> + Send + 'static,
{
    let guard = phase(Phase::Catalog);
    let result = tokio::task::spawn_blocking(move || operation(&catalog))
        .await
        .map_err(|error| HistoryError::Io(std::io::Error::other(error)))
        .and_then(std::convert::identity);
    guard.finish(result.is_ok());
    result
}

pub(in crate::server) fn execution_id(outcome: &BashCommandRuntimeResult) -> Option<String> {
    if let Some(background) = outcome.result.state.background_id.as_ref() {
        Some(format!("background:{background}"))
    } else {
        outcome.execution_token.as_ref().map(|token| {
            format!("{}:{}:{}", token.guardian_epoch, token.session_epoch, token.generation)
        })
    }
}

pub(in crate::server) fn capture(
    outcome: &BashCommandRuntimeResult,
    label: Option<String>,
) -> Option<ExecutionCapture> {
    let workflow = outcome.workflow.as_ref();
    Some(ExecutionCapture {
        execution_id: execution_id(outcome)?,
        execution_token: outcome.execution_token.clone(),
        command_generation: outcome.command_generation,
        state: outcome.result.state.clone(),
        output: workflow.map_or_else(
            || CapturedOutput::Fragment(outcome.result.output.clone()),
            |observation| {
                if observation.output_included {
                    CapturedOutput::Snapshot(observation.output.clone())
                } else {
                    CapturedOutput::None
                }
            },
        ),
        screen: workflow
            .filter(|observation| observation.output_included)
            .map(|observation| observation.screen.join("\n")),
        explanation: workflow.map(|observation| observation.explanation.clone()),
        prompt_receipt: workflow.and_then(|observation| observation.prompt_receipt.clone()),
        activity_seq: workflow.map(|observation| observation.activity_seq),
        observed_at_unix_ms: now_ms(),
        label,
        output_truncated: outcome.output_truncated,
        dropped_output_file: outcome.dropped_output_file.clone(),
    })
}

pub(in crate::server) fn bound_options(record: &ExecutionRecord) -> ShellActionOptions {
    ShellActionOptions {
        compact_output: true,
        expected_execution: record.execution_token.clone(),
        expected_generation: record.command_generation,
        require_generation_binding: true,
        ..ShellActionOptions::default()
    }
}

pub(in crate::server) async fn inspect(
    runtime: &dyn ShellRuntime,
    slot: &SharedBashState,
    scope: &ExecutionScope,
    record: &ExecutionRecord,
    recognizer: Option<String>,
    include_output: bool,
) -> crate::errors::Result<BashCommandRuntimeResult> {
    let guard = phase(if include_output { Phase::RuntimeCapture } else { Phase::RuntimeInspect });
    let result = runtime
        .run_action_detailed(
            slot,
            BashCommand {
                thread_id: scope.thread_id.clone(),
                wait_for_seconds: Some(0.0),
                action_json: BashCommandAction::Inspect {
                    bg_command_id: record.background_id.clone(),
                    recognizer,
                    include_output: Some(include_output),
                },
            },
            bound_options(record),
        )
        .await;
    guard.finish(result.is_ok());
    result
}

pub(in crate::server) async fn refresh_record(
    runtime: &dyn ShellRuntime,
    slot: &SharedBashState,
    catalog: &ExecutionCatalog,
    scope: &ExecutionScope,
    record: &ExecutionRecord,
    recognizer: Option<String>,
) -> HistoryResult<ExecutionRecord> {
    if record.process_status == ExecutionProcessStatus::Exited && !record.output_pending {
        return Ok(record.clone());
    }
    let first = match inspect(runtime, slot, scope, record, recognizer.clone(), false).await {
        Ok(outcome) => outcome,
        Err(error) => {
            let scope = scope.clone();
            let id = record.execution_id.clone();
            let reason = format!("runtime observation unavailable: {error}");
            return history_call(catalog.clone(), move |catalog| {
                catalog.mark_unknown(&scope, &id, &reason, now_ms())?.ok_or(HistoryError::NotFound)
            })
            .await;
        }
    };
    let needs_output = first.workflow.as_ref().is_some_and(|observation| {
        record.output_pending
            || record.output_activity_seq != Some(observation.activity_seq)
            || !first.result.state.is_running()
            || record.output_coverage != "retained_runtime_output"
    });
    let mut outcome = first;
    if needs_output {
        // A replacement between these two bound observations must not erase the
        // terminal state already observed by the first call.
        if let Ok(full) = inspect(runtime, slot, scope, record, recognizer, true).await {
            outcome = full;
        }
    }
    let capture = capture(&outcome, record.label.clone()).ok_or(HistoryError::IdentityConflict)?;
    if capture.execution_id != record.execution_id {
        return Err(HistoryError::IdentityConflict);
    }
    let scope = scope.clone();
    history_call(catalog.clone(), move |catalog| {
        catalog.update_existing(&scope, capture)?.ok_or(HistoryError::NotFound)
    })
    .await
}

fn capture_scope(
    outcome: &BashCommandRuntimeResult,
    expected_thread_id: &str,
    update_scope: Option<&ExecutionScope>,
) -> HistoryResult<ExecutionScope> {
    let context = outcome.action_context.as_ref().ok_or_else(|| {
        HistoryError::InvalidInput(
            "runtime action context is unavailable; execution history was not recorded".into(),
        )
    })?;
    let expected_thread_id = crate::types::normalize_thread_id(expected_thread_id);
    if expected_thread_id.is_empty() || context.thread_id != expected_thread_id {
        return Err(HistoryError::IdentityConflict);
    }
    let scope = update_scope.cloned().unwrap_or_else(|| ExecutionScope {
        thread_id: context.thread_id.clone(),
        workspace_root: context.workspace_root.clone(),
    });
    if scope.thread_id != expected_thread_id || !scope.workspace_root.is_absolute() {
        return Err(HistoryError::IdentityConflict);
    }
    Ok(scope)
}

impl WinxService {
    pub(in crate::server) async fn capture_bash_execution(
        &self,
        slot: &SharedBashState,
        outcome: &BashCommandRuntimeResult,
        label: Option<String>,
        expected_thread_id: &str,
        update_scope: Option<&ExecutionScope>,
    ) -> HistoryResult<Option<ExecutionRecord>> {
        // Only accepted Command actions carry a label. Status/input observations
        // and Agent calls may update an existing record, never create one elsewhere.
        let create = label.is_some() && update_scope.is_none();
        let Some(capture) = capture(outcome, label) else { return Ok(None) };
        let scope = capture_scope(outcome, expected_thread_id, update_scope)?;
        let catalog = self.execution_history.catalog()?;
        let saved_scope = scope.clone();
        let retained = history_call(catalog.clone(), move |catalog| {
            if create {
                catalog.record(&saved_scope, capture).map(Some)
            } else {
                catalog.update_existing(&saved_scope, capture)
            }
        })
        .await?;
        let Some(mut record) = retained else { return Ok(None) };
        let supported =
            self.shell_runtime.supports_agent_workflows_for(slot).await.unwrap_or(false);
        if supported && record.execution_token.is_some() {
            // Retain the launch's original scope even if the adapter was reconfigured.
            if outcome.workflow.is_none() {
                if let Ok(full) =
                    inspect(self.shell_runtime.as_ref(), slot, &scope, &record, None, true).await
                {
                    if let Some(capture) = self::capture(&full, record.label.clone()) {
                        if capture.execution_id != record.execution_id {
                            return Err(HistoryError::IdentityConflict);
                        }
                        let saved_scope = scope.clone();
                        let refreshed = history_call(catalog.clone(), move |catalog| {
                            catalog.update_existing(&saved_scope, capture)
                        })
                        .await?;
                        let Some(refreshed) = refreshed else { return Ok(None) };
                        record = refreshed;
                    }
                }
            }
            self.observe_execution(slot.clone(), scope, record.clone()).await;
        }
        Ok(Some(record))
    }

    async fn observe_execution(
        &self,
        slot: SharedBashState,
        scope: ExecutionScope,
        record: ExecutionRecord,
    ) {
        if record.process_status != ExecutionProcessStatus::Running {
            return;
        }
        let key = (scope.thread_id.clone(), scope.workspace_root.clone(), record.execution_id);
        if !self.execution_history.observing.lock().await.insert(key.clone()) {
            return;
        }
        let history = Arc::downgrade(&self.execution_history);
        let runtime = self.shell_runtime.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let Some(history) = history.upgrade() else { break };
                let result = async {
                    let catalog = history.catalog()?;
                    let query_scope = scope.clone();
                    let id = key.2.clone();
                    let record = history_call(catalog.clone(), move |catalog| {
                        catalog.get(&query_scope, &id, now_ms())
                    })
                    .await?;
                    let Some(record) = record else { return Ok(false) };
                    if record.process_status != ExecutionProcessStatus::Running {
                        return Ok(false);
                    }
                    let record =
                        refresh_record(runtime.as_ref(), &slot, &catalog, &scope, &record, None)
                            .await?;
                    Ok::<bool, HistoryError>(
                        record.process_status == ExecutionProcessStatus::Running,
                    )
                }
                .await;
                if !matches!(result, Ok(true)) {
                    if let Err(error) = result {
                        warn!(%error, "execution history observer stopped");
                    }
                    history.observing.lock().await.remove(&key);
                    break;
                }
            }
        });
    }

    pub(in crate::server) async fn refresh_before_launch(&self, slot: &SharedBashState) {
        let scope = {
            let guard = slot.lock().await;
            let Some(state) = guard.as_ref() else { return };
            ExecutionScope {
                thread_id: state.current_thread_id.clone(),
                workspace_root: state.workspace_root.clone(),
            }
        };
        let Ok(catalog) = self.execution_history.catalog() else { return };
        if !self.shell_runtime.supports_agent_workflows_for(slot).await.unwrap_or(false) {
            return;
        }
        let query_scope = scope.clone();
        let Ok(pending) = history_call(catalog.clone(), move |catalog| {
            catalog.pending_foreground(&query_scope, now_ms())
        })
        .await
        else {
            return;
        };
        for record in &pending {
            if let Err(error) =
                refresh_record(self.shell_runtime.as_ref(), slot, &catalog, &scope, record, None)
                    .await
            {
                warn!(%error, "pre-launch execution history reconciliation failed");
            }
        }
    }
}

#[cfg(test)]
pub(super) mod tests;
