//! In-memory lifecycle state for the SEP-2663 MCP Tasks extension.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::model::{DetailedTask, JsonObject, Task, TaskPayload, TaskStatus};
use serde_json::Value;
use tokio::sync::Notify;
use tokio::task::AbortHandle;

use crate::runtime::ShellExecutionToken;

/// Retain completed task results for five minutes unless the client asks for a
/// different TTL. This is deliberately bounded: Winx task results can contain a
/// large build log.
pub const DEFAULT_TASK_TTL_MS: u64 = 5 * 60 * 1_000;
pub const MAX_TASKS: usize = 32;

#[derive(Debug, Default)]
pub(crate) struct TaskExecutionControl {
    cancelled: Arc<AtomicBool>,
    generation: AtomicU64,
    execution_token: std::sync::Mutex<Option<ShellExecutionToken>>,
    launch_finished: AtomicBool,
    generation_ready: Notify,
}

impl TaskExecutionControl {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub(crate) fn cancellation_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn publish_generation(&self, generation: u64) {
        self.generation.store(generation, Ordering::SeqCst);
        self.generation_ready.notify_waiters();
    }

    fn publish_execution(&self, token: ShellExecutionToken) {
        let generation = token.generation;
        if let Ok(mut slot) = self.execution_token.lock() {
            *slot = Some(token);
        }
        self.publish_generation(generation);
    }

    fn execution_token(&self) -> Option<ShellExecutionToken> {
        self.execution_token.lock().ok().and_then(|slot| slot.clone())
    }

    fn generation(&self) -> Option<u64> {
        match self.generation.load(Ordering::SeqCst) {
            0 => None,
            generation => Some(generation),
        }
    }

    pub(crate) fn finish_launch(&self) {
        self.launch_finished.store(true, Ordering::SeqCst);
        self.generation_ready.notify_waiters();
    }

    pub(crate) async fn wait_for_generation(&self) -> Option<u64> {
        loop {
            let notified = self.generation_ready.notified();
            if let Some(generation) = self.generation() {
                return Some(generation);
            }
            if self.launch_finished.load(Ordering::SeqCst) {
                return None;
            }
            notified.await;
        }
    }

    pub(crate) async fn wait_for_execution(&self) -> Option<ShellExecutionToken> {
        self.wait_for_generation().await?;
        self.execution_token()
    }
}

#[derive(Debug)]
pub struct TaskEntry {
    pub task: Task,
    pub result: Option<Value>,
    pub abort_handle: Option<AbortHandle>,
    pub thread_id: String,
    command_generation: Option<u64>,
    execution_token: Option<ShellExecutionToken>,
    execution_control: Arc<TaskExecutionControl>,
    expires_at: Option<Instant>,
    terminal_at: Option<Instant>,
}

impl TaskEntry {
    pub fn working(task: Task, thread_id: String) -> Self {
        Self {
            task,
            result: None,
            abort_handle: None,
            thread_id,
            command_generation: None,
            execution_token: None,
            execution_control: Arc::new(TaskExecutionControl::default()),
            expires_at: None,
            terminal_at: None,
        }
    }

    pub fn command_generation(&self) -> Option<u64> {
        self.command_generation.or_else(|| self.execution_control.generation())
    }

    pub fn set_command_generation(&mut self, generation: u64) {
        self.execution_control.publish_generation(generation);
        self.command_generation = Some(generation);
    }

    pub fn execution_token(&self) -> Option<ShellExecutionToken> {
        self.execution_token.clone().or_else(|| self.execution_control.execution_token())
    }

    pub fn set_execution_token(&mut self, token: ShellExecutionToken) {
        self.command_generation = Some(token.generation);
        self.execution_control.publish_execution(token.clone());
        self.execution_token = Some(token);
    }

    pub fn request_cancel(&self) {
        self.execution_control.cancel();
    }

    pub fn is_cancel_requested(&self) -> bool {
        self.execution_control.is_cancelled()
    }

    pub(crate) fn execution_control(&self) -> Arc<TaskExecutionControl> {
        Arc::clone(&self.execution_control)
    }

    pub fn finish(
        &mut self,
        status: TaskStatus,
        status_message: Option<String>,
        result: Option<Value>,
    ) {
        self.task.status = status;
        self.task.status_message = status_message;
        self.task.last_updated_at = rmcp::task_manager::current_timestamp();
        self.result = result;
        self.abort_handle = None;
        self.expires_at = self.task.ttl_ms.map(|ttl| Instant::now() + Duration::from_millis(ttl));
        self.terminal_at = Some(Instant::now());
    }

    pub fn detailed(&self) -> DetailedTask {
        let payload = match self.task.status {
            TaskStatus::Working => TaskPayload::Working,
            TaskStatus::InputRequired => TaskPayload::Failed {
                error: task_error("Winx BashCommand tasks do not request in-task input"),
            },
            TaskStatus::Completed => match self.result.clone() {
                Some(Value::Object(result)) => TaskPayload::Completed { result },
                _ => {
                    TaskPayload::Failed { error: task_error("Completed task has no object result") }
                }
            },
            TaskStatus::Failed => TaskPayload::Failed {
                error: task_error(
                    self.task
                        .status_message
                        .as_deref()
                        .unwrap_or("Task failed without a status message"),
                ),
            },
            TaskStatus::Cancelled => TaskPayload::Cancelled,
            _ => TaskPayload::Failed { error: task_error("Task has an unsupported status") },
        };
        DetailedTask::new(self.task.clone(), payload)
    }

    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|expiry| now >= expiry)
    }
}

#[derive(Debug, Default)]
pub struct TaskRegistry {
    entries: HashMap<String, TaskEntry>,
}

impl TaskRegistry {
    pub fn prune(&mut self) {
        let now = Instant::now();
        self.entries.retain(|_, entry| !entry.is_expired(now));
    }

    pub fn insert(&mut self, task_id: String, entry: TaskEntry) -> Result<(), &'static str> {
        self.prune();
        while self.entries.len() >= MAX_TASKS {
            let oldest_terminal = self
                .entries
                .iter()
                .filter_map(|(id, entry)| entry.terminal_at.map(|at| (id.clone(), at)))
                .min_by_key(|(_, at)| *at)
                .map(|(id, _)| id);
            let Some(oldest_terminal) = oldest_terminal else { break };
            self.entries.remove(&oldest_terminal);
        }
        let working = self
            .entries
            .values()
            .filter(|entry| {
                matches!(entry.task.status, TaskStatus::Working | TaskStatus::InputRequired)
            })
            .count();
        if working >= MAX_TASKS {
            return Err("MCP task limit reached; wait for or cancel an existing task");
        }
        if self.entries.contains_key(&task_id) {
            return Err("MCP task identifier collision");
        }
        self.entries.insert(task_id, entry);
        Ok(())
    }

    pub fn get(&mut self, task_id: &str) -> Option<&TaskEntry> {
        self.prune();
        self.entries.get(task_id)
    }

    pub fn get_mut(&mut self, task_id: &str) -> Option<&mut TaskEntry> {
        self.prune();
        self.entries.get_mut(task_id)
    }

    pub fn remove(&mut self, task_id: &str) -> Option<TaskEntry> {
        self.entries.remove(task_id)
    }
}

fn task_error(message: &str) -> JsonObject {
    serde_json::Map::from_iter([
        ("code".to_string(), Value::from(-32603)),
        ("message".to_string(), Value::from(message)),
    ])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn task(id: &str, ttl: u64) -> Task {
        Task::new(
            id.to_string(),
            TaskStatus::Working,
            "2026-07-21T12:00:00Z".to_string(),
            "2026-07-21T12:00:00Z".to_string(),
        )
        .with_ttl_ms(ttl)
    }

    #[test]
    fn terminal_task_keeps_repeatable_result() {
        let mut registry = TaskRegistry::default();
        registry
            .insert("one".into(), TaskEntry::working(task("one", 60_000), "tid_1".into()))
            .unwrap();
        registry.get_mut("one").unwrap().finish(
            TaskStatus::Completed,
            Some("done".into()),
            Some(serde_json::json!({"content": []})),
        );

        assert_eq!(
            registry.get("one").and_then(|entry| entry.result.clone()),
            Some(serde_json::json!({"content": []}))
        );
        assert_eq!(
            registry.get("one").and_then(|entry| entry.result.clone()),
            Some(serde_json::json!({"content": []}))
        );
    }

    #[test]
    fn terminal_tasks_are_evicted_before_they_exhaust_working_capacity() {
        let mut registry = TaskRegistry::default();
        for index in 0..MAX_TASKS {
            let id = format!("terminal-{index}");
            registry
                .insert(id.clone(), TaskEntry::working(task(&id, 300_000), "tid".into()))
                .unwrap();
            registry.get_mut(&id).unwrap().finish(
                TaskStatus::Completed,
                Some("done".into()),
                Some(serde_json::json!({"content": []})),
            );
        }

        registry
            .insert(
                "new-working".into(),
                TaskEntry::working(task("new-working", 300_000), "tid".into()),
            )
            .unwrap();
        assert_eq!(registry.get("new-working").unwrap().task.status, TaskStatus::Working);
    }
}
