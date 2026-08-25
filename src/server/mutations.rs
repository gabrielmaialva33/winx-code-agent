//! Bounded mutation deduplication and recovery-loop control for edit tools.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tracing::warn;

use super::{outcomes, WinxService};
use crate::state::bash_state::{EditMutationPostcondition, EditMutationReceipt};
use crate::tool_registry::ToolKind;

const MUTATION_RECEIPT_TTL_MS: u64 = 30 * 60 * 1_000;
const RECOVERY_TTL: Duration = Duration::from_secs(30 * 60);
const RECOVERY_ENTRY_CAP: usize = 256;
const RECOVERY_ATTEMPT_LIMIT: u32 = 3;

#[derive(Clone, Default)]
pub(super) struct MutationCoordinator {
    inner: Arc<Mutex<CoordinatorState>>,
}

#[derive(Default)]
struct CoordinatorState {
    running: HashMap<String, watch::Sender<u64>>,
    recovery: HashMap<RecoveryKey, RecoveryEntry>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RecoveryKey {
    thread_id: String,
    path: String,
}

struct RecoveryEntry {
    attempts: u32,
    updated_at: Instant,
}

#[derive(Clone, Debug)]
struct MutationMetadata {
    fingerprint: String,
    receipt_id: String,
    tool: String,
    thread_id: String,
    workspace_root: Option<String>,
    target_paths: Vec<String>,
    verification: Option<VerificationPlan>,
}

#[derive(Clone, Debug)]
struct VerificationPlan {
    id: String,
    command: String,
    wait_for_seconds: Option<f32>,
}

pub(super) enum MutationStart {
    Bypass,
    Owner(MutationOwner),
    Replay(CallToolResult),
}

pub(super) struct MutationOwner {
    coordinator: MutationCoordinator,
    metadata: MutationMetadata,
    released: bool,
}

impl MutationOwner {
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.coordinator.release(&self.metadata.fingerprint);
        self.released = true;
    }
}

impl Drop for MutationOwner {
    fn drop(&mut self) {
        self.release();
    }
}

impl MutationCoordinator {
    fn lock(&self) -> MutexGuard<'_, CoordinatorState> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn acquire(&self, metadata: MutationMetadata) -> MutationOwner {
        loop {
            let mut receiver = {
                let mut state = self.lock();
                if let Some(sender) = state.running.get(&metadata.fingerprint) {
                    Some(sender.subscribe())
                } else {
                    let (sender, _) = watch::channel(0_u64);
                    state.running.insert(metadata.fingerprint.clone(), sender);
                    None
                }
            };
            let Some(receiver) = receiver.as_mut() else {
                return MutationOwner { coordinator: self.clone(), metadata, released: false };
            };
            let _ = receiver.changed().await;
        }
    }

    fn release(&self, fingerprint: &str) {
        if let Some(sender) = self.lock().running.remove(fingerprint) {
            sender.send_replace(1);
        }
    }
}

impl WinxService {
    pub(super) async fn begin_edit_mutation(
        &self,
        tool: ToolKind,
        arguments: Option<&Value>,
    ) -> MutationStart {
        let Some(metadata) = mutation_metadata(tool, arguments) else {
            return MutationStart::Bypass;
        };
        let owner = self.mutations.acquire(metadata).await;
        if let Some(result) = self.persisted_mutation_result(&owner.metadata).await {
            return MutationStart::Replay(result);
        }
        MutationStart::Owner(owner)
    }

    async fn persisted_mutation_result(
        &self,
        metadata: &MutationMetadata,
    ) -> Option<CallToolResult> {
        if metadata.thread_id.is_empty() {
            return None;
        }
        let (slot, _guard) = self.session_for(&metadata.thread_id).await;
        let receipt = {
            let mut state = slot.lock().await;
            state.as_mut()?.edit_mutation_receipt(
                &metadata.fingerprint,
                now_unix_ms(),
                MUTATION_RECEIPT_TTL_MS,
            )
        }?;
        if receipt.tool != metadata.tool {
            return None;
        }

        if postconditions_match(&receipt.postconditions).await {
            Some(mutation_replay_result(metadata, &receipt))
        } else {
            Some(mutation_drift_result(metadata, &receipt))
        }
    }

    pub(super) async fn finish_edit_mutation(
        &self,
        mut owner: MutationOwner,
        result: &mut CallToolResult,
    ) {
        if result.is_error == Some(true) || !edit_was_applied(result) {
            owner.release();
            return;
        }
        if owner.metadata.verification.is_none()
            && self.has_fresh_mutation_receipt(&owner.metadata).await
        {
            attach_mutation_metadata(result, &owner.metadata, true, false);
            owner.release();
            return;
        }

        let Some(postconditions) = self.capture_postconditions(&owner.metadata).await else {
            attach_mutation_metadata(result, &owner.metadata, false, false);
            owner.release();
            return;
        };
        let receipt = EditMutationReceipt {
            fingerprint: owner.metadata.fingerprint.clone(),
            receipt_id: owner.metadata.receipt_id.clone(),
            tool: owner.metadata.tool.clone(),
            status: outcomes::result_status(result),
            committed_at_unix_ms: now_unix_ms(),
            postconditions,
            persisted: true,
            verification_pending: false,
        };

        let (slot, _guard) = self.session_for(&owner.metadata.thread_id).await;
        let persisted = {
            let mut state = slot.lock().await;
            let Some(state) = state.as_mut() else {
                attach_mutation_metadata(result, &owner.metadata, false, false);
                owner.release();
                return;
            };
            let fingerprint = receipt.fingerprint.clone();
            state.record_edit_mutation_receipt(receipt);
            match state.save_state_to_disk() {
                Ok(()) => true,
                Err(error) => {
                    state.mark_edit_mutation_receipt_volatile(&fingerprint);
                    warn!(%error, "failed to persist edit mutation receipt");
                    false
                }
            }
        };
        attach_mutation_metadata(result, &owner.metadata, persisted, false);
        owner.release();
    }

    async fn has_fresh_mutation_receipt(&self, metadata: &MutationMetadata) -> bool {
        if metadata.thread_id.is_empty() {
            return false;
        }
        let (slot, _guard) = self.session_for(&metadata.thread_id).await;
        let mut state = slot.lock().await;
        state.as_mut().is_some_and(|state| {
            state
                .edit_mutation_receipt(
                    &metadata.fingerprint,
                    now_unix_ms(),
                    MUTATION_RECEIPT_TTL_MS,
                )
                .is_some_and(|receipt| receipt.persisted)
        })
    }

    /// Persist a committed file mutation before awaiting its verification
    /// command. If the client disconnects or the adapter restarts during that
    /// command, the next identical call replays this receipt instead of writing
    /// the file a second time.
    pub(super) async fn checkpoint_committed_edit(&self, tool: ToolKind, arguments: &Value) {
        let Some(metadata) = mutation_metadata(tool, Some(arguments)) else {
            return;
        };
        let pending = metadata.verification.is_some();
        let postconditions = self.capture_postconditions(&metadata).await;
        let (slot, _guard) = self.session_for(&metadata.thread_id).await;
        let mut state = slot.lock().await;
        let Some(state) = state.as_mut() else { return };
        if let Some(postconditions) = postconditions {
            let fingerprint = metadata.fingerprint.clone();
            state.record_edit_mutation_receipt(EditMutationReceipt {
                fingerprint: metadata.fingerprint,
                receipt_id: metadata.receipt_id,
                tool: metadata.tool,
                status: if pending {
                    "completed_with_issues".to_string()
                } else {
                    "completed".to_string()
                },
                committed_at_unix_ms: now_unix_ms(),
                postconditions,
                persisted: true,
                verification_pending: pending,
            });
            if let Err(error) = state.save_state_to_disk() {
                state.mark_edit_mutation_receipt_volatile(&fingerprint);
                warn!(%error, "failed to checkpoint committed edit before verification");
            }
            return;
        }
        if let Err(error) = state.save_state_to_disk() {
            warn!(%error, "failed to checkpoint committed edit before verification");
        }
    }

    async fn capture_postconditions(
        &self,
        metadata: &MutationMetadata,
    ) -> Option<Vec<EditMutationPostcondition>> {
        if metadata.target_paths.is_empty() || metadata.thread_id.is_empty() {
            return None;
        }
        let (slot, _guard) = self.session_for(&metadata.thread_id).await;
        let (cwd, workspace_root) = {
            let state = slot.lock().await;
            let state = state.as_ref()?;
            (state.cwd.clone(), state.workspace_root.clone())
        };
        let target_paths = metadata.target_paths.clone();
        match tokio::task::spawn_blocking(move || {
            target_paths
                .iter()
                .map(|path| capture_postcondition(path, &cwd, &workspace_root))
                .collect::<Option<Vec<_>>>()
        })
        .await
        {
            Ok(Some(postconditions)) => Some(postconditions),
            Ok(None) => {
                warn!(tool = %metadata.tool, "could not capture edit postcondition; replay receipt disabled");
                None
            }
            Err(error) => {
                warn!(%error, tool = %metadata.tool, "edit postcondition worker failed");
                None
            }
        }
    }

    pub(super) fn apply_edit_recovery_budget(
        &self,
        tool: ToolKind,
        arguments: Option<&Value>,
        result: &mut CallToolResult,
    ) {
        if !tool.is_file_mutation() {
            return;
        }
        if result.is_error != Some(true) {
            self.clear_edit_recovery(arguments);
            return;
        }
        let Some(Value::Object(envelope)) = result.structured_content.as_mut() else {
            return;
        };
        let conflict = matches!(
            envelope.get("errorCode").and_then(Value::as_str),
            Some("search_block_not_found" | "search_block_ambiguous")
        );
        if !conflict {
            return;
        }
        let thread_id = string_argument(arguments, "thread_id").unwrap_or_default();
        let paths = envelope
            .get("requiredReads")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("path").and_then(Value::as_str))
            .map(str::to_string)
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return;
        }

        let now = Instant::now();
        let attempt = {
            let mut state = self.mutations.lock();
            state.recovery.retain(|_, entry| now.duration_since(entry.updated_at) <= RECOVERY_TTL);
            if state.recovery.len() >= RECOVERY_ENTRY_CAP {
                if let Some(oldest) = state
                    .recovery
                    .iter()
                    .min_by_key(|(_, entry)| entry.updated_at)
                    .map(|(key, _)| key.clone())
                {
                    state.recovery.remove(&oldest);
                }
            }
            paths
                .iter()
                .map(|path| {
                    let entry = state
                        .recovery
                        .entry(RecoveryKey { thread_id: thread_id.clone(), path: path.clone() })
                        .or_insert(RecoveryEntry { attempts: 0, updated_at: now });
                    entry.attempts = entry.attempts.saturating_add(1);
                    entry.updated_at = now;
                    entry.attempts
                })
                .max()
                .unwrap_or(1)
        };

        let data = envelope.entry("data").or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(data) = data {
            data.insert("recovery_attempt".to_string(), json!(attempt));
            data.insert("recovery_attempt_limit".to_string(), json!(RECOVERY_ATTEMPT_LIMIT));
            data.insert("recovery_escalated".to_string(), Value::Bool(attempt >= 2));
        }
        if attempt < RECOVERY_ATTEMPT_LIMIT {
            return;
        }

        let message = "SEARCH recovery exhausted after three conflicts on the same target. Stop \
                       repeating edit/read cycles, inspect the exact current file and surrounding \
                       context, then formulate a materially different edit or ask the user for \
                       direction. No file was changed by this call.";
        envelope.insert("status".to_string(), json!(outcomes::ToolResultStatus::RecoveryExhausted));
        envelope.insert(
            "errorCode".to_string(),
            Value::String("search_recovery_exhausted".to_string()),
        );
        envelope.insert("message".to_string(), Value::String(message.to_string()));
        envelope.insert("retryable".to_string(), Value::Bool(false));
        envelope.insert("retrySameCall".to_string(), Value::Bool(false));
        envelope.remove("nextAction");
        result.content.insert(0, ContentBlock::text(format!("RECOVERY EXHAUSTED. {message}")));
    }

    fn clear_edit_recovery(&self, arguments: Option<&Value>) {
        let thread_id = string_argument(arguments, "thread_id").unwrap_or_default();
        if thread_id.is_empty() {
            return;
        }
        self.mutations.lock().recovery.retain(|key, _| key.thread_id != thread_id);
    }
}

fn mutation_metadata(tool: ToolKind, arguments: Option<&Value>) -> Option<MutationMetadata> {
    if !tool.is_file_mutation() {
        return None;
    }
    let arguments = arguments?;
    let mut hasher = Sha256::new();
    hash_bytes(&mut hasher, tool.as_str().as_bytes());
    hash_json(&mut hasher, arguments);
    let fingerprint = hex_digest(hasher.finalize().as_slice());
    let verification = arguments
        .get("verify_command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(|command| VerificationPlan {
            id: verification_receipt_id(Some(arguments), command),
            command: command.to_string(),
            wait_for_seconds: arguments
                .get("verify_wait_for_seconds")
                .cloned()
                .and_then(|wait| serde_json::from_value::<f32>(wait).ok()),
        });
    Some(MutationMetadata {
        receipt_id: format!("edit_{}", &fingerprint[..24]),
        fingerprint,
        tool: tool.as_str().to_string(),
        thread_id: string_argument(Some(arguments), "thread_id").unwrap_or_default(),
        workspace_root: string_argument(Some(arguments), "workspace_root"),
        target_paths: mutation_target_paths(Some(arguments)),
        verification,
    })
}

pub(super) fn verification_receipt_id(arguments: Option<&Value>, command: &str) -> String {
    let mut hasher = Sha256::new();
    hash_bytes(&mut hasher, b"VerifyEdit/v1");
    for key in ["thread_id", "workspace_root"] {
        hash_bytes(&mut hasher, string_argument(arguments, key).unwrap_or_default().as_bytes());
    }
    hash_bytes(&mut hasher, command.trim().as_bytes());
    let fingerprint = hex_digest(hasher.finalize().as_slice());
    format!("verify_{}", &fingerprint[..24])
}

fn mutation_target_paths(arguments: Option<&Value>) -> Vec<String> {
    let Some(arguments) = arguments else { return Vec::new() };
    if let Some(path) = arguments.get("file_path").and_then(Value::as_str) {
        return vec![path.to_string()];
    }
    arguments
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|file| file.get("file_path").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

fn capture_postcondition(
    path: &str,
    cwd: &std::path::Path,
    workspace_root: &std::path::Path,
) -> Option<EditMutationPostcondition> {
    let resolved = crate::utils::path::resolve_in_workspace(path, cwd, workspace_root).ok()?;
    let content = std::fs::read(&resolved).ok()?;
    Some(EditMutationPostcondition {
        path: resolved.to_string_lossy().into_owned(),
        sha256: hex_digest(Sha256::digest(content).as_slice()),
    })
}

async fn postconditions_match(postconditions: &[EditMutationPostcondition]) -> bool {
    let postconditions = postconditions.to_vec();
    tokio::task::spawn_blocking(move || {
        !postconditions.is_empty()
            && postconditions.iter().all(|condition| {
                std::fs::read(PathBuf::from(&condition.path)).is_ok_and(|content| {
                    hex_digest(Sha256::digest(content).as_slice()) == condition.sha256
                })
            })
    })
    .await
    .unwrap_or(false)
}

fn edit_was_applied(result: &CallToolResult) -> bool {
    let status = outcomes::result_status(result);
    matches!(
        status.as_str(),
        "completed" | "completed_with_issues" | "running" | "awaiting_input" | "awaiting_approval"
    )
}

fn attach_mutation_metadata(
    result: &mut CallToolResult,
    metadata: &MutationMetadata,
    persisted: bool,
    replayed: bool,
) {
    let Some(Value::Object(envelope)) = result.structured_content.as_mut() else {
        return;
    };
    let data = envelope.entry("data").or_insert_with(|| Value::Object(Map::new()));
    let Some(data) = data.as_object_mut() else { return };
    data.insert("mutation_receipt_id".to_string(), Value::String(metadata.receipt_id.clone()));
    data.insert("mutation_replayed".to_string(), Value::Bool(replayed));
    data.insert(
        "mutation_transition".to_string(),
        Value::String(if replayed { "replayed" } else { "committed" }.to_string()),
    );
    data.insert("mutation_receipt_persisted".to_string(), Value::Bool(persisted));
    data.insert("idempotency_window_ms".to_string(), json!(MUTATION_RECEIPT_TTL_MS));
}

fn mutation_replay_result(
    metadata: &MutationMetadata,
    receipt: &EditMutationReceipt,
) -> CallToolResult {
    let needs_verification = metadata
        .verification
        .as_ref()
        .filter(|_| receipt.verification_pending || receipt.status != "completed");
    let text = if receipt.verification_pending {
        format!(
            "IDEMPOTENT REPLAY: {} already committed this exact mutation as {} before its original \
             verification response completed. Winx verified the target hashes and did not write \
             files or run the check again. Execute the VerifyEdit nextAction separately.",
            metadata.tool, receipt.receipt_id
        )
    } else {
        format!(
            "IDEMPOTENT REPLAY: {} already committed this exact mutation as {}. Winx verified the \
             target hashes and did not write files or run verification again.",
            metadata.tool, receipt.receipt_id
        )
    };
    let mut result = CallToolResult::success(vec![ContentBlock::text(text.clone())]);
    let mut structured = json!({
        "status": receipt.status,
        "tool": metadata.tool,
        "message": text,
        "retryable": false,
        "retrySameCall": false,
        "requiredReads": [],
        "data": {
            "thread_id": metadata.thread_id,
            "workspace_root": metadata.workspace_root,
            "file_paths": metadata.target_paths,
            "edit_applied": true,
            "mutation_receipt_id": receipt.receipt_id,
            "mutation_transition": "replayed",
            "mutation_replayed": true,
            "mutation_receipt_persisted": receipt.persisted,
            "idempotency_window_ms": MUTATION_RECEIPT_TTL_MS,
            "result_compacted": true,
            "follow_up_required": needs_verification.is_some()
        }
    });
    if let Some(verification) = needs_verification {
        structured["data"]["verification_id"] = Value::String(verification.id.clone());
        structured["data"]["verification_status"] = Value::String(
            if receipt.verification_pending { "pending" } else { "failed" }.to_string(),
        );
        structured["nextAction"] = json!({
            "tool": "VerifyEdit",
            "instruction": if receipt.verification_pending {
                "The edit is already committed. Run this exact verification receipt separately; never repeat the edit."
            } else {
                "The edit is already committed. Make corrective changes first, then run this exact verification receipt; never repeat the edit."
            },
            "arguments": verification_retry_arguments(metadata, verification)
        });
    }
    result.structured_content = Some(structured);
    result
}

fn verification_retry_arguments(
    metadata: &MutationMetadata,
    verification: &VerificationPlan,
) -> Value {
    let mut arguments = json!({
        "verification_id": verification.id,
        "command": verification.command,
        "thread_id": metadata.thread_id,
    });
    if let Some(workspace_root) = metadata.workspace_root.as_ref() {
        arguments["workspace_root"] = Value::String(workspace_root.clone());
    }
    if let Some(wait) = verification.wait_for_seconds {
        arguments["wait_for_seconds"] = json!(wait);
    }
    arguments
}

fn mutation_drift_result(
    metadata: &MutationMetadata,
    receipt: &EditMutationReceipt,
) -> CallToolResult {
    let paths =
        receipt.postconditions.iter().map(|condition| condition.path.clone()).collect::<Vec<_>>();
    let message = format!(
        "{} already committed this exact mutation as {}, but a target changed afterward. Winx \
         refused to replay or overwrite the newer state. Read the current target and formulate a \
         new edit; do not repeat this call unchanged.",
        metadata.tool, receipt.receipt_id
    );
    let next_arguments = json!({
        "file_paths": paths,
        "thread_id": metadata.thread_id,
        "workspace_root": metadata.workspace_root,
    });
    let required_reads = receipt
        .postconditions
        .iter()
        .map(|condition| json!({"path": condition.path, "ranges": []}))
        .collect::<Vec<_>>();
    let mut result = CallToolResult::error(vec![ContentBlock::text(message.clone())]);
    result.structured_content = Some(json!({
        "status": "conflict",
        "tool": metadata.tool,
        "message": message,
        "errorCode": "mutation_postcondition_changed",
        "retryable": false,
        "retrySameCall": false,
        "nextAction": {
            "tool": "ReadFiles",
            "instruction": "Read the current targets, preserve newer changes, and create a materially new edit call.",
            "arguments": next_arguments
        },
        "requiredReads": required_reads,
        "data": {
            "thread_id": metadata.thread_id,
            "workspace_root": metadata.workspace_root,
            "file_paths": metadata.target_paths,
            "edit_applied": false,
            "prior_edit_applied": true,
            "mutation_receipt_id": receipt.receipt_id,
            "mutation_transition": "postcondition_changed",
            "mutation_replayed": false
        }
    }));
    result
}

fn hash_json(hasher: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hasher.update(b"n"),
        Value::Bool(value) => hasher.update(if *value { b"t" } else { b"f" }),
        Value::Number(value) => {
            hasher.update(b"d");
            hash_bytes(hasher, value.to_string().as_bytes());
        }
        Value::String(value) => {
            hasher.update(b"s");
            hash_bytes(hasher, value.as_bytes());
        }
        Value::Array(values) => {
            hasher.update(b"a");
            hasher.update(values.len().to_le_bytes());
            for value in values {
                hash_json(hasher, value);
            }
        }
        Value::Object(values) => {
            hasher.update(b"o");
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            hasher.update(keys.len().to_le_bytes());
            for key in keys {
                hash_bytes(hasher, key.as_bytes());
                hash_json(hasher, &values[key]);
            }
        }
    }
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(bytes.len().to_le_bytes());
    hasher.update(bytes);
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

fn string_argument(arguments: Option<&Value>, key: &str) -> Option<String> {
    arguments?.get(key)?.as_str().map(str::to_string)
}

fn now_unix_ms() -> u64 {
    let millis = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_fingerprint_is_object_order_independent() {
        let left = json!({"thread_id": "t", "file_path": "/x", "content": "a"});
        let right = json!({"content": "a", "file_path": "/x", "thread_id": "t"});
        assert_eq!(
            mutation_metadata(ToolKind::FileWriteOrEdit, Some(&left)).map(|item| item.fingerprint),
            mutation_metadata(ToolKind::FileWriteOrEdit, Some(&right)).map(|item| item.fingerprint)
        );
    }

    #[test]
    fn mutation_fingerprint_changes_with_payload() {
        let left = json!({"thread_id": "t", "file_path": "/x", "content": "a"});
        let right = json!({"thread_id": "t", "file_path": "/x", "content": "b"});
        assert_ne!(
            mutation_metadata(ToolKind::FileWriteOrEdit, Some(&left)).map(|item| item.fingerprint),
            mutation_metadata(ToolKind::FileWriteOrEdit, Some(&right)).map(|item| item.fingerprint)
        );
    }

    #[test]
    fn verification_receipt_binds_command_and_project_session() {
        let arguments = json!({"thread_id": "t", "workspace_root": "/workspace"});
        let original = verification_receipt_id(Some(&arguments), "cargo test");
        assert_eq!(original, verification_receipt_id(Some(&arguments), " cargo test "));
        assert_ne!(original, verification_receipt_id(Some(&arguments), "cargo check"));
        assert_ne!(
            original,
            verification_receipt_id(
                Some(&json!({"thread_id": "other", "workspace_root": "/workspace"})),
                "cargo test"
            )
        );
    }

    #[test]
    fn interrupted_verification_replay_points_only_to_verify_edit() -> Result<(), &'static str> {
        let arguments = json!({
            "thread_id": "thread",
            "workspace_root": "/workspace",
            "file_path": "/workspace/file.rs",
            "verify_command": "cargo test",
            "verify_wait_for_seconds": 5
        });
        let metadata = mutation_metadata(ToolKind::FileWriteOrEdit, Some(&arguments))
            .ok_or("missing metadata")?;
        let receipt = EditMutationReceipt {
            fingerprint: metadata.fingerprint.clone(),
            receipt_id: metadata.receipt_id.clone(),
            tool: metadata.tool.clone(),
            status: "completed_with_issues".to_string(),
            committed_at_unix_ms: now_unix_ms(),
            postconditions: vec![EditMutationPostcondition {
                path: "/workspace/file.rs".to_string(),
                sha256: "hash".to_string(),
            }],
            persisted: true,
            verification_pending: true,
        };

        let result = mutation_replay_result(&metadata, &receipt);
        let structured = result.structured_content.ok_or("missing structured replay")?;
        assert_eq!(structured["status"], "completed_with_issues");
        assert_eq!(structured["nextAction"]["tool"], "VerifyEdit");
        assert_eq!(structured["nextAction"]["arguments"]["command"], "cargo test");
        assert_eq!(structured["data"]["edit_applied"], true);
        assert_eq!(structured["data"]["verification_status"], "pending");
        Ok(())
    }
}
