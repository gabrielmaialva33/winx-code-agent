//! Bounded, non-consuming observation of multiple retained executions.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::json;

use super::history::{history_call, now_ms, refresh_record};
use super::{history_error_details, WaitTiming, WorkflowContext, WorkflowReply};
use crate::server::outcomes::ToolResultStatus;
use crate::server::WinxService;
use crate::state::execution_history::{
    CatalogObservation, ExecutionProcessStatus, ExecutionRecord, HistoryError, HistoryResult,
    ObservationRequest, OutputPage, OutputSource,
};
use crate::state::turn::TurnState;

#[derive(Serialize)]
struct ExecutionSummary {
    execution_id: String,
    process_status: ExecutionProcessStatus,
    exit_code: Option<i32>,
    turn_state: Option<TurnState>,
    observed_at_unix_ms: u64,
    finished_at_unix_ms: Option<u64>,
    expires_at_unix_ms: Option<u64>,
    activity_seq: Option<u64>,
    output_activity_seq: Option<u64>,
    output_pending: bool,
    sequence: u64,
}

impl From<&ExecutionRecord> for ExecutionSummary {
    fn from(record: &ExecutionRecord) -> Self {
        Self {
            execution_id: record.execution_id.clone(),
            process_status: record.process_status,
            exit_code: record.exit_code,
            turn_state: record.turn_state,
            observed_at_unix_ms: record.observed_at_unix_ms,
            finished_at_unix_ms: record.finished_at_unix_ms,
            expires_at_unix_ms: record.expires_at_unix_ms,
            activity_seq: record.activity_seq,
            output_activity_seq: record.output_activity_seq,
            output_pending: record.output_pending,
            sequence: record.sequence,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ObservationStage {
    Refresh,
    Read,
}

#[derive(Clone, Serialize)]
struct ObservationError {
    stage: ObservationStage,
    status: ToolResultStatus,
    code: &'static str,
    message: String,
}

impl ObservationError {
    fn new(stage: ObservationStage, error: &HistoryError) -> Self {
        let (status, code) = history_error_details(error);
        Self { stage, status, code, message: error.to_string() }
    }
}

#[derive(Serialize)]
struct ObservedItem {
    execution_id: String,
    source: OutputSource,
    status: ToolResultStatus,
    execution: Option<ExecutionSummary>,
    page: Option<OutputPage>,
    errors: Vec<ObservationError>,
}

impl WinxService {
    pub(super) async fn observe_executions(
        &self,
        context: &WorkflowContext,
        items: Vec<ObservationRequest>,
        timeout_seconds: Option<f32>,
        timing: Option<WaitTiming>,
    ) -> HistoryResult<WorkflowReply> {
        let timing = match timing {
            Some(timing) => timing,
            None => WaitTiming::new(timeout_seconds)?,
        };
        // The same absolute budget already covered session admission. This is
        // one observation pass, never a lifecycle poll or a per-item timeout.
        tokio::time::timeout_at(timing.deadline(), self.observe_once(context, items))
            .await
            .map_err(|_| {
                HistoryError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "execution observation deadline elapsed",
                ))
            })?
    }

    async fn observe_once(
        &self,
        context: &WorkflowContext,
        items: Vec<ObservationRequest>,
    ) -> HistoryResult<WorkflowReply> {
        let catalog = self.execution_history.catalog()?;
        let scope = context.scope.clone();
        let baseline =
            history_call(catalog.clone(), move |catalog| catalog.list(&scope, now_ms())).await?;
        let supported =
            self.shell_runtime.supports_agent_workflows_for(&context.slot).await.unwrap_or(false);
        let mut refresh_errors = HashMap::new();
        if supported {
            // The catalog has one record per identity, so requesting several
            // sources for an ID still performs exactly one refresh pass.
            for record in baseline.executions.iter().filter(|record| {
                (record.process_status != ExecutionProcessStatus::Exited || record.output_pending)
                    && items.iter().any(|item| item.execution_id == record.execution_id)
            }) {
                if let Err(error) = refresh_record(
                    self.shell_runtime.as_ref(),
                    &context.slot,
                    &catalog,
                    &context.scope,
                    record,
                    None,
                )
                .await
                {
                    refresh_errors.insert(
                        record.execution_id.clone(),
                        ObservationError::new(ObservationStage::Refresh, &error),
                    );
                }
            }
        }
        let scope = context.scope.clone();
        let snapshot =
            history_call(catalog, move |catalog| catalog.observe(&scope, &items, now_ms())).await?;
        Ok(observation_reply(snapshot, &refresh_errors))
    }
}

fn observation_reply(
    snapshot: CatalogObservation,
    refresh_errors: &HashMap<String, ObservationError>,
) -> WorkflowReply {
    let items: Vec<_> = snapshot
        .items
        .into_iter()
        .map(|item| {
            let mut errors: Vec<_> =
                refresh_errors.get(&item.execution_id).cloned().into_iter().collect();
            let page = match item.page {
                Ok(page) => Some(page),
                Err(error) => {
                    errors.push(ObservationError::new(ObservationStage::Read, &error));
                    None
                }
            };
            ObservedItem {
                execution_id: item.execution_id,
                source: item.source,
                status: errors.first().map_or(ToolResultStatus::Completed, |error| error.status),
                execution: item.record.as_ref().map(ExecutionSummary::from),
                page,
                errors,
            }
        })
        .collect();
    let items_with_issues = items.iter().filter(|item| !item.errors.is_empty()).count();
    let has_issues = items_with_issues > 0;
    WorkflowReply {
        status: if has_issues {
            ToolResultStatus::CompletedWithIssues
        } else {
            ToolResultStatus::Completed
        },
        error_code: has_issues.then_some("execution_observation_issues"),
        message: if has_issues {
            "Retained execution state and output pages; some items have observation issues."
        } else {
            "Retained execution state and bounded output pages."
        }
        .into(),
        output: None,
        data: json!({
            "items": items,
            "items_with_issues": items_with_issues,
            "catalog_sequence": snapshot.sequence,
            "catalog_cursor": snapshot.cursor,
        }),
    }
}

#[cfg(test)]
mod tests;
