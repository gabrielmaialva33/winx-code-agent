//! In-memory lifecycle state for MCP Tasks.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rmcp::model::{Task, TaskStatus};
use serde_json::Value;
use tokio::task::AbortHandle;

/// Retain completed task results for five minutes unless the client asks for a
/// different TTL. This is deliberately bounded: Winx task results can contain a
/// large build log.
pub const DEFAULT_TASK_TTL_MS: u64 = 5 * 60 * 1_000;
pub const MIN_TASK_TTL_MS: u64 = 1_000;
pub const MAX_TASK_TTL_MS: u64 = 24 * 60 * 60 * 1_000;
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
        self.expires_at = self.task.ttl.map(|ttl| Instant::now() + Duration::from_millis(ttl));
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

    pub fn list(&mut self) -> Vec<Task> {
        self.prune();
        let mut tasks = self.entries.values().map(|entry| entry.task.clone()).collect::<Vec<_>>();
        tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        tasks
    }
}

/// Negotiate a bounded result-retention TTL from the task augmentation object.
pub fn requested_ttl(task: Option<&serde_json::Map<String, Value>>) -> u64 {
    task.and_then(|metadata| metadata.get("ttl"))
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TASK_TTL_MS)
        .clamp(MIN_TASK_TTL_MS, MAX_TASK_TTL_MS)
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
        .with_ttl(ttl)
    }

    #[test]
    fn requested_ttl_is_bounded() {
        let low = serde_json::from_value(serde_json::json!({"ttl": 1})).unwrap();
        let high = serde_json::from_value(serde_json::json!({"ttl": u64::MAX})).unwrap();
        assert_eq!(requested_ttl(Some(&low)), MIN_TASK_TTL_MS);
        assert_eq!(requested_ttl(Some(&high)), MAX_TASK_TTL_MS);
        assert_eq!(requested_ttl(None), DEFAULT_TASK_TTL_MS);
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
