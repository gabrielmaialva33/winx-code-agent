//! In-memory lifecycle state for the SEP-2663 MCP Tasks extension.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rmcp::model::{DetailedTask, JsonObject, Task, TaskPayload, TaskStatus};
use serde_json::Value;
use tokio::task::AbortHandle;

/// Retain completed task results for five minutes unless the client asks for a
/// different TTL. This is deliberately bounded: Winx task results can contain a
/// large build log.
pub const DEFAULT_TASK_TTL_MS: u64 = 5 * 60 * 1_000;
pub const MAX_TASKS: usize = 32;

#[derive(Debug)]
pub struct TaskEntry {
    pub task: Task,
    pub result: Option<Value>,
    pub abort_handle: Option<AbortHandle>,
    pub thread_id: String,
    expires_at: Option<Instant>,
}

impl TaskEntry {
    pub fn working(task: Task, thread_id: String) -> Self {
        Self { task, result: None, abort_handle: None, thread_id, expires_at: None }
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
        if self.entries.len() >= MAX_TASKS {
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
}
