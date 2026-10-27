use crate::server::usage::lifecycle::{phase, Phase};

use std::time::Instant;

use serde_json::json;

use super::history::{history_call, now_ms};
use super::{bounded_timeout, WorkflowContext, WorkflowReply};
use crate::server::workflow_types::ExecutionWatchEvent;
use crate::server::WinxService;
use crate::state::execution_history::{
    ExecutionProcessStatus, ExecutionRecord, HistoryChanges, HistoryError, HistoryResult,
    MAX_WATCHED_EXECUTIONS,
};
use crate::state::turn::TurnState;

/// One budget spans session admission, baseline I/O and lifecycle observation.
#[derive(Clone, Copy)]
pub(in crate::server) struct WaitTiming {
    started: Instant,
    deadline: tokio::time::Instant,
    poll_only: bool,
}

impl WaitTiming {
    pub(in crate::server) fn new(timeout_seconds: Option<f32>) -> HistoryResult<Self> {
        let timeout = bounded_timeout(timeout_seconds)?;
        // Zero still allows one bounded catalog poll, but no lifecycle waiting.
        let budget =
            if timeout.is_zero() { std::time::Duration::from_millis(250) } else { timeout };
        Ok(Self {
            started: Instant::now(),
            deadline: tokio::time::Instant::now() + budget,
            poll_only: timeout.is_zero(),
        })
    }

    pub(super) fn deadline(self) -> tokio::time::Instant {
        self.deadline
    }
}

impl WinxService {
    pub(in crate::server) async fn wait_for_executions(
        &self,
        context: &WorkflowContext,
        execution_ids: Vec<String>,
        after: Option<String>,
        events: Vec<ExecutionWatchEvent>,
        timeout_seconds: Option<f32>,
        timing: Option<WaitTiming>,
    ) -> HistoryResult<WorkflowReply> {
        if execution_ids.is_empty() || execution_ids.len() > MAX_WATCHED_EXECUTIONS {
            return Err(HistoryError::InvalidInput(
                "wait requires between 1 and 32 execution_ids".into(),
            ));
        }
        let timing = match timing {
            Some(timing) => timing,
            None => WaitTiming::new(timeout_seconds)?,
        };
        let started = timing.started;
        let deadline = timing.deadline;
        let catalog = self.execution_history.catalog()?;
        let cursor = if let Some(after) = after {
            after
        } else {
            let scope = context.scope.clone();
            before_deadline(
                deadline,
                history_call(catalog.clone(), move |catalog| catalog.list(&scope, now_ms())),
            )
            .await?
            .cursor
        };
        // Validate the cursor and expiry before a timeout can become a no-change result.
        let mut changes = changes_before_deadline(
            &catalog,
            &context.scope,
            &execution_ids,
            &cursor,
            &events,
            deadline,
        )
        .await?;
        loop {
            let timed_out = timing.poll_only || tokio::time::Instant::now() >= deadline;
            if changes.resync_required || !changes.executions.is_empty() || timed_out {
                return Ok(wait_reply(&changes, timed_out, started));
            }
            match tokio::time::timeout_at(
                deadline, self.refresh_workflow_history(context, Some(&execution_ids)),
            ).await {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => return Err(HistoryError::InvalidInput(
                    "live waiting requires the guardian agent_workflows capability; archived reads remain available".into(),
                )),
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    // Keep the last validated cursor: observations not yet queried remain
                    // available to the next wait instead of being silently acknowledged.
                    return Ok(wait_reply(&changes, true, started));
                }
            }
            // A catalog deadline/error is not evidence of no change. Propagate it.
            changes = changes_before_deadline(
                &catalog,
                &context.scope,
                &execution_ids,
                &cursor,
                &events,
                deadline,
            )
            .await?;
            if changes.resync_required || !changes.executions.is_empty() {
                return Ok(wait_reply(&changes, false, started));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let guard = phase(Phase::WaitIdle);
            tokio::time::sleep(remaining.min(std::time::Duration::from_millis(250))).await;
            guard.finish(true);
        }
    }
}

async fn before_deadline<T>(
    deadline: tokio::time::Instant,
    operation: impl std::future::Future<Output = HistoryResult<T>>,
) -> HistoryResult<T> {
    tokio::time::timeout_at(deadline, operation).await.map_err(|_| {
        HistoryError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "execution wait deadline elapsed before the catalog could be observed",
        ))
    })?
}

async fn changes_before_deadline(
    catalog: &crate::state::execution_history::ExecutionCatalog,
    scope: &crate::state::execution_history::ExecutionScope,
    execution_ids: &[String],
    cursor: &str,
    events: &[ExecutionWatchEvent],
    deadline: tokio::time::Instant,
) -> HistoryResult<HistoryChanges> {
    let scope = scope.clone();
    let ids = execution_ids.to_vec();
    let cursor = cursor.to_string();
    let mut changes = before_deadline(
        deadline,
        history_call(catalog.clone(), move |catalog| {
            catalog.changes(&scope, &cursor, &ids, now_ms())
        }),
    )
    .await?;
    if !changes.resync_required {
        let after = changes.after_sequence;
        changes.executions.retain(|record| matches_events(record, after, events));
    }
    Ok(changes)
}

fn wait_reply(changes: &HistoryChanges, deadline_reached: bool, started: Instant) -> WorkflowReply {
    let timed_out = deadline_reached && changes.executions.is_empty() && !changes.resync_required;
    WorkflowReply {
        message: if changes.resync_required {
            "The watched catalog changed retention; resynchronize with this snapshot and cursor."
        } else if timed_out {
            "No matching execution change was observed before the bounded deadline."
        } else {
            "At least one watched execution changed."
        }
        .into(),
        output: None,
        data: json!({
            "changes": changes,
            "timed_out": timed_out,
            "elapsed_ms": started.elapsed().as_millis(),
        }),
    }
}

fn matches_events(record: &ExecutionRecord, after: u64, events: &[ExecutionWatchEvent]) -> bool {
    if events.is_empty() {
        return record.sequence > after;
    }
    events.iter().any(|event| match event {
        ExecutionWatchEvent::State => record.state_sequence > after,
        ExecutionWatchEvent::Output => record.output_sequence > after,
        ExecutionWatchEvent::Exited => {
            record.state_sequence > after && record.process_status == ExecutionProcessStatus::Exited
        }
        ExecutionWatchEvent::Unknown => {
            record.state_sequence > after
                && record.process_status == ExecutionProcessStatus::Unknown
        }
        ExecutionWatchEvent::AwaitingInput => {
            record.state_sequence > after && record.turn_state == Some(TurnState::AwaitingInput)
        }
        ExecutionWatchEvent::AwaitingApproval => {
            record.state_sequence > after && record.turn_state == Some(TurnState::AwaitingApproval)
        }
        ExecutionWatchEvent::Busy => {
            record.state_sequence > after && record.turn_state == Some(TurnState::Busy)
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::state::execution_history::{ExecutionCatalog, ExecutionScope, HistoryLimits};

    #[tokio::test]
    async fn wait_for_executions_bounds_its_initial_catalog_read() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("history");
        let mut service = WinxService::new();
        service.execution_history =
            Arc::new(super::super::history::tests::history_at(root.clone()));
        let scope = ExecutionScope {
            thread_id: "wait-public-lock".into(),
            workspace_root: temp.path().canonicalize()?,
        };
        let catalog = service.execution_history.catalog()?;
        catalog.list(&scope, now_ms())?;
        let directory = std::fs::read_dir(&root)?.next().expect("scope directory")?.path();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join("history.lock"))?;
        lock.lock()?;
        let (slot, session_guard) = service.tool_session_for(&scope.thread_id).await;
        let context =
            WorkflowContext { slot, scope, allow_prompt: false, _session_guard: session_guard };
        let started = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            service.wait_for_executions(
                &context,
                vec!["watched".into()],
                None,
                Vec::new(),
                Some(0.04),
                None,
            ),
        )
        .await;
        let elapsed = started.elapsed();
        drop(lock);
        let result = result.expect("the complete public wait must be bounded");
        assert!(matches!(result, Err(HistoryError::Io(ref error))
            if error.kind() == std::io::ErrorKind::TimedOut));
        assert!(
            elapsed < Duration::from_millis(200),
            "wait_for_executions ignored its deadline during baseline I/O: {elapsed:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn wait_catalog_deadline_returns_before_the_lock_and_the_worker_still_terminates(
    ) -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("history");
        let catalog = ExecutionCatalog::new(root.clone(), HistoryLimits::default());
        let scope =
            ExecutionScope { thread_id: "wait-lock".into(), workspace_root: temp.path().into() };
        catalog.list(&scope, now_ms())?;
        let directory = std::fs::read_dir(&root)?.next().expect("scope directory")?.path();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join("history.lock"))?;
        lock.lock()?;
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = finished.clone();
        let started = Instant::now();
        let result = before_deadline(
            tokio::time::Instant::now() + Duration::from_millis(40),
            history_call(catalog, move |catalog| {
                let result = catalog.list(&scope, now_ms());
                worker_finished.store(true, Ordering::SeqCst);
                result
            }),
        )
        .await;
        assert!(matches!(result, Err(HistoryError::Io(ref error))
            if error.kind() == std::io::ErrorKind::TimedOut));
        assert!(started.elapsed() < Duration::from_millis(200));
        // The request deadline drops the join future. The underlying lock retry
        // must also end while the lock is still held, rather than leaking a worker.
        let stopped = tokio::time::timeout(Duration::from_secs(1), async {
            while !finished.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        drop(lock);
        assert!(stopped.is_ok(), "catalog worker survived its bounded lock deadline");
        Ok(())
    }

    #[tokio::test]
    async fn deadline_queries_preserve_invalid_cursor_and_missing_target_results(
    ) -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let catalog = ExecutionCatalog::new(temp.path().join("history"), HistoryLimits::default());
        let scope =
            ExecutionScope { thread_id: "wait-resync".into(), workspace_root: temp.path().into() };
        let baseline = catalog.list(&scope, now_ms())?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let invalid =
            changes_before_deadline(&catalog, &scope, &[], "not-a-cursor", &[], deadline).await;
        assert!(matches!(invalid, Err(HistoryError::InvalidCursor)));
        let missing = changes_before_deadline(
            &catalog,
            &scope,
            &["expired-or-absent".into()],
            &baseline.cursor,
            &[ExecutionWatchEvent::Exited],
            deadline,
        )
        .await?;
        assert!(missing.resync_required);
        assert_eq!(missing.missing_ids, vec!["expired-or-absent"]);
        let reply = wait_reply(&missing, true, Instant::now());
        assert_eq!(reply.data["timed_out"], false);
        assert_eq!(reply.data["changes"]["resync_required"], true);
        Ok(())
    }
}
