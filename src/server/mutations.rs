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
use crate::errors::{Result, WinxError};
use crate::state::bash_state::{
    EditMutationPostcondition, EditMutationReceipt, EditVerificationExecution,
    EditVerificationState,
};
use crate::tools::edit_files::{EditCommand, PreparedEditContext};

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
    verifications: HashMap<String, watch::Sender<u64>>,
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
    legacy_fingerprint: Option<String>,
    receipt_id: String,
    response_tool: String,
    thread_id: String,
    workspace_root: Option<String>,
    target_paths: Vec<String>,
    verification: Option<VerificationPlan>,
}

#[derive(Clone, Debug)]
struct VerificationPlan {
    /// Identity persisted with the canonical mutation receipt. This binds the
    /// check to the committed edit, not to whichever public wire invoked it.
    id: String,
    /// Compatibility identity delivered to the caller. Legacy edit surfaces
    /// must keep returning the existing `VerifyEdit` receipt during Phase 1.
    delivery_id: String,
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

pub(super) enum VerificationStart {
    Execute(VerificationOwner),
    Poll(VerificationOwner, Option<EditVerificationExecution>),
    Replay(CallToolResult),
}

pub(super) struct VerificationOwner {
    coordinator: MutationCoordinator,
    verification_id: String,
    thread_id: String,
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

impl VerificationOwner {
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.coordinator.release_verification(&self.verification_id);
        self.released = true;
    }
}

impl Drop for VerificationOwner {
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

    async fn acquire_verification(
        &self,
        verification_id: String,
        thread_id: String,
    ) -> VerificationOwner {
        loop {
            let mut receiver = {
                let mut state = self.lock();
                if let Some(sender) = state.verifications.get(&verification_id) {
                    Some(sender.subscribe())
                } else {
                    let (sender, _) = watch::channel(0_u64);
                    state.verifications.insert(verification_id.clone(), sender);
                    None
                }
            };
            let Some(receiver) = receiver.as_mut() else {
                return VerificationOwner {
                    coordinator: self.clone(),
                    verification_id,
                    thread_id,
                    released: false,
                };
            };
            let _ = receiver.changed().await;
        }
    }

    fn release_verification(&self, verification_id: &str) {
        if let Some(sender) = self.lock().verifications.remove(verification_id) {
            sender.send_replace(1);
        }
    }
}

impl WinxService {
    pub(super) async fn resolve_legacy_verification_id(
        &self,
        thread_id: &str,
        verification_id: &str,
    ) -> Result<String> {
        let (slot, _guard) = self.session_for(thread_id).await;
        let mut state = slot.lock().await;
        let state = state.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
        let shadow = state
            .edit_mutation_receipt_by_legacy_verification_id(
                verification_id,
                now_unix_ms(),
                MUTATION_RECEIPT_TTL_MS,
            )
            .ok_or_else(|| {
                WinxError::InvalidInput(
                    "verification_id is unknown or expired; use the exact VerifyEdit nextAction from the committed edit"
                        .to_string(),
                )
            })?;
        shadow.verification_id.ok_or_else(|| {
            WinxError::InvalidInput(
                "legacy verification receipt predates receipt-bound verification; repeat the edit only if its current postconditions no longer exist"
                    .to_string(),
            )
        })
    }

    pub(super) async fn begin_receipt_bound_verification(
        &self,
        thread_id: &str,
        verification_id: &str,
        command: &str,
        wait_for_seconds: Option<f32>,
    ) -> Result<VerificationStart> {
        let thread_id = crate::types::normalize_thread_id(thread_id);
        let mut owner = self
            .mutations
            .acquire_verification(verification_id.to_string(), thread_id.clone())
            .await;
        let (slot, _guard) = self.session_for(&thread_id).await;
        let mut state_guard = slot.lock().await;
        let state = state_guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
        let receipt = state
            .edit_mutation_receipt_by_verification_id(
                verification_id,
                now_unix_ms(),
                MUTATION_RECEIPT_TTL_MS,
            )
            .ok_or_else(|| {
                WinxError::InvalidInput(
                    "verification_id is unknown or expired; use the exact receipt-bound BashCommand nextAction from the committed edit"
                        .to_string(),
                )
            })?;
        let expected = canonical_verification_receipt_id(
            &receipt.receipt_id,
            &state.current_thread_id,
            &state.workspace_root.to_string_lossy(),
            command,
            wait_for_seconds,
        );
        if expected != verification_id {
            return Err(WinxError::InvalidInput(
                "verification_id does not match this command, wait policy, and project session"
                    .to_string(),
            ));
        }
        let verification_state = receipt.verification_state.unwrap_or_else(|| {
            if receipt.verification_pending {
                EditVerificationState::Pending
            } else if receipt.status == "completed" {
                EditVerificationState::Passed
            } else {
                EditVerificationState::Failed
            }
        });
        match verification_state {
            EditVerificationState::Passed => {
                owner.release();
                Ok(VerificationStart::Replay(verification_replay_result(
                    verification_id,
                    &thread_id,
                )))
            }
            EditVerificationState::Running if receipt.verification_execution.is_some() => {
                Ok(VerificationStart::Poll(owner, receipt.verification_execution))
            }
            EditVerificationState::Skipped => Err(WinxError::InvalidInput(
                "verification was skipped because the mutation committed only a prefix; issue a new edit for the uncommitted suffix"
                    .to_string(),
            )),
            EditVerificationState::Pending
            | EditVerificationState::Reserved
            | EditVerificationState::Running
            | EditVerificationState::Interrupted
            | EditVerificationState::Failed => {
                if !state.update_edit_mutation_verification(
                    verification_id,
                    "completed_with_issues",
                    EditVerificationState::Reserved,
                    None,
                    now_unix_ms(),
                    MUTATION_RECEIPT_TTL_MS,
                ) {
                    return Err(WinxError::InvalidInput(
                        "verification receipt disappeared before reservation".to_string(),
                    ));
                }
                state.save_state_to_disk()?;
                Ok(VerificationStart::Execute(owner))
            }
        }
    }

    pub(super) async fn begin_edit_mutation(
        &self,
        prepared: Option<&PreparedEditContext>,
    ) -> std::result::Result<MutationStart, rmcp::ErrorData> {
        let Some(prepared) = prepared else {
            return Ok(MutationStart::Bypass);
        };
        let Some(metadata) = mutation_metadata(prepared) else {
            return Ok(MutationStart::Bypass);
        };
        let owner = self.mutations.acquire(metadata).await;
        if let Some(result) = self.persisted_mutation_result(&owner.metadata, prepared).await? {
            return Ok(MutationStart::Replay(result));
        }
        Ok(MutationStart::Owner(owner))
    }

    async fn persisted_mutation_result(
        &self,
        metadata: &MutationMetadata,
        prepared: &PreparedEditContext,
    ) -> std::result::Result<Option<CallToolResult>, rmcp::ErrorData> {
        if metadata.thread_id.is_empty() {
            return Ok(None);
        }
        let (slot, _guard) = self.session_for(&metadata.thread_id).await;
        let mut state_guard = slot.lock().await;
        let Some(state) = state_guard.as_mut() else { return Ok(None) };
        if let Err(error) = prepared.authorize_current_state(state) {
            return outcomes::tool_failure(
                &metadata.response_tool,
                &error,
                Some(&prepared.original_arguments),
            )
            .map(Some);
        }
        let desired_verification_id = metadata.verification.as_ref().map(|plan| plan.id.as_str());
        let exact_canonical = state.edit_mutation_receipt_variant(
            &metadata.fingerprint,
            desired_verification_id,
            now_unix_ms(),
            MUTATION_RECEIPT_TTL_MS,
        );
        let exact_legacy = metadata.legacy_fingerprint.as_deref().and_then(|fingerprint| {
            state.edit_mutation_receipt_variant(
                fingerprint,
                desired_verification_id,
                now_unix_ms(),
                MUTATION_RECEIPT_TTL_MS,
            )
        });
        let (receipt, exact_variant) = if let Some(receipt) = exact_canonical.or(exact_legacy) {
            (receipt, true)
        } else if let Some(receipt) = state.edit_mutation_receipt(
            &metadata.fingerprint,
            now_unix_ms(),
            MUTATION_RECEIPT_TTL_MS,
        ) {
            (receipt, false)
        } else {
            let Some(fingerprint) = metadata.legacy_fingerprint.as_deref() else {
                return Ok(None);
            };
            let Some(receipt) =
                state.edit_mutation_receipt(fingerprint, now_unix_ms(), MUTATION_RECEIPT_TTL_MS)
            else {
                return Ok(None);
            };
            (receipt, false)
        };

        // Keep the session operation barrier held from authorization through
        // postcondition validation and replay construction. A concurrent mode
        // tightening is therefore ordered either wholly before (and denies) or
        // wholly after this replay decision.
        if !postconditions_match(&receipt.postconditions).await {
            return Ok(Some(mutation_drift_result(metadata, &receipt)));
        }

        let undo_ids = receipt
            .undo_ids
            .iter()
            .enumerate()
            .map(|(index, undo_id)| {
                let path = receipt.postconditions.get(index).map(|item| item.path.as_str());
                undo_id
                    .as_ref()
                    .filter(|undo_id| {
                        path.is_some_and(|path| state.has_receipt_bound_checkpoint(path, undo_id))
                    })
                    .cloned()
            })
            .collect::<Vec<_>>();
        let next_undo_id = receipt.next_undo_id.as_ref().filter(|undo_id| {
            receipt.postconditions.iter().any(|condition| {
                state.next_undo_id_for(&condition.path).as_deref() == Some(undo_id.as_str())
            })
        });
        let (mut receipt_for_result, pair) = canonicalize_replay_receipt_pair(
            metadata,
            &receipt,
            exact_variant,
            undo_ids,
            next_undo_id.cloned(),
        );
        state.record_edit_mutation_receipts(pair);
        if let Err(error) = state.save_state_to_disk() {
            state.mark_edit_mutation_receipt_volatile(&metadata.fingerprint);
            if let Some(fingerprint) = metadata.legacy_fingerprint.as_deref() {
                state.mark_edit_mutation_receipt_volatile(fingerprint);
            }
            receipt_for_result.persisted = false;
            warn!(%error, "failed to persist canonical replay receipt");
        }
        Ok(Some(mutation_replay_result(metadata, &receipt_for_result)))
    }

    pub(super) async fn finish_receipt_bound_verification(
        &self,
        mut owner: VerificationOwner,
        result: &CallToolResult,
        command_generation: Option<u64>,
        execution_token: Option<&crate::runtime::ShellExecutionToken>,
    ) {
        let status = outcomes::result_status(result);
        let active = matches!(status.as_str(), "running" | "awaiting_input" | "awaiting_approval");
        let exit_code = result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("data"))
            .and_then(|data| data.get("exit_code"))
            .and_then(Value::as_i64);
        let failed = result.is_error == Some(true) || exit_code.is_some_and(|code| code != 0);
        let receipt_status = if active || failed { "completed_with_issues" } else { "completed" };
        let execution = if active {
            execution_token.map_or_else(
                || {
                    command_generation.map(|generation| EditVerificationExecution {
                        generation,
                        guardian_epoch: String::new(),
                        session_epoch: String::new(),
                    })
                },
                |token| {
                    Some(EditVerificationExecution {
                        generation: token.generation,
                        guardian_epoch: token.guardian_epoch.clone(),
                        session_epoch: token.session_epoch.clone(),
                    })
                },
            )
        } else {
            None
        };
        let verification_state = if active && execution.is_some() {
            EditVerificationState::Running
        } else if active {
            // An active shell result without a generation/token cannot be
            // polled safely. Preserve retryability but never bind the receipt
            // to whichever foreground process happens to exist later.
            EditVerificationState::Interrupted
        } else if failed {
            EditVerificationState::Failed
        } else {
            EditVerificationState::Passed
        };

        let (slot, _guard) = self.session_for(&owner.thread_id).await;
        let mut state = slot.lock().await;
        let Some(state) = state.as_mut() else { return };
        if !state.update_edit_mutation_verification(
            &owner.verification_id,
            receipt_status,
            verification_state,
            execution.as_ref(),
            now_unix_ms(),
            MUTATION_RECEIPT_TTL_MS,
        ) {
            return;
        }
        if let Err(error) = state.save_state_to_disk() {
            state.mark_edit_mutation_verification_volatile(&owner.verification_id);
            if active {
                let _ = state.update_edit_mutation_verification(
                    &owner.verification_id,
                    "completed_with_issues",
                    EditVerificationState::Interrupted,
                    None,
                    now_unix_ms(),
                    MUTATION_RECEIPT_TTL_MS,
                );
            }
            warn!(%error, "failed to persist receipt-bound verification outcome");
        }
        owner.release();
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
        if self.has_fresh_mutation_receipt(&owner.metadata).await {
            attach_mutation_metadata(result, &owner.metadata, true, false);
            owner.release();
            return;
        }

        // The edit handler checkpoints the exact planner-produced postcondition
        // before returning. Never re-read a mutable target here: doing so would
        // bind idempotency to bytes written by an external actor in the seam
        // between commit and receipt persistence.
        attach_mutation_metadata(result, &owner.metadata, false, false);
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
    pub(super) async fn checkpoint_committed_edit(
        &self,
        prepared: &PreparedEditContext,
        postconditions: &[EditMutationPostcondition],
        uncommitted_paths: &[String],
        undo_ids: &[Option<String>],
        next_undo_id: Option<String>,
    ) {
        let Some(metadata) = mutation_metadata(prepared) else {
            return;
        };
        let pending = metadata.verification.is_some() && uncommitted_paths.is_empty();
        let (slot, _guard) = self.session_for(&metadata.thread_id).await;
        let mut state = slot.lock().await;
        let Some(state) = state.as_mut() else { return };
        if !postconditions.is_empty() {
            let fingerprint = metadata.fingerprint.clone();
            let status = if pending || !uncommitted_paths.is_empty() {
                "completed_with_issues"
            } else {
                "completed"
            };
            let receipts = mutation_receipts(
                &metadata,
                status.to_string(),
                postconditions.to_vec(),
                pending,
                uncommitted_paths.to_vec(),
                next_undo_id.clone(),
                undo_ids.to_vec(),
            );
            state.record_edit_mutation_receipts(receipts);
            if let Err(error) = state.save_state_to_disk() {
                state.mark_edit_mutation_receipt_volatile(&fingerprint);
                if let Some(fingerprint) = metadata.legacy_fingerprint.as_deref() {
                    state.mark_edit_mutation_receipt_volatile(fingerprint);
                }
                warn!(%error, "failed to checkpoint committed edit before verification");
            }
            return;
        }
        if let Err(error) = state.save_state_to_disk() {
            warn!(%error, "failed to checkpoint committed edit before verification");
        }
    }

    pub(super) fn apply_edit_recovery_budget(
        &self,
        prepared: Option<&PreparedEditContext>,
        result: &mut CallToolResult,
    ) {
        let Some(prepared) = prepared else {
            return;
        };
        if result.is_error != Some(true) {
            self.clear_edit_recovery(&prepared.thread_id);
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
        let thread_id = prepared.thread_id.clone();
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

    fn clear_edit_recovery(&self, thread_id: &str) {
        if thread_id.is_empty() {
            return;
        }
        self.mutations.lock().recovery.retain(|key, _| key.thread_id != thread_id);
    }
}

fn mutation_metadata(prepared: &PreparedEditContext) -> Option<MutationMetadata> {
    if matches!(prepared.command, EditCommand::Undo { legacy_lifo: true, .. }) {
        return None;
    }
    let mut hasher = Sha256::new();
    hash_bytes(&mut hasher, b"EditFiles/v1");
    hash_json(&mut hasher, &prepared.canonical_value());
    let fingerprint = hex_digest(hasher.finalize().as_slice());
    let receipt_id = format!("edit_{}", &fingerprint[..24]);
    let legacy_fingerprint = prepared.surface.is_legacy().then(|| {
        let mut legacy = Sha256::new();
        hash_bytes(&mut legacy, prepared.surface.tool_name().as_bytes());
        hash_json(&mut legacy, &prepared.original_arguments);
        hex_digest(legacy.finalize().as_slice())
    });
    let verification = prepared.verification.as_ref().map(|verification| {
        let wait_for_seconds = Some(verification.wait_for_seconds.unwrap_or(15.0));
        let id = canonical_verification_receipt_id(
            &receipt_id,
            &prepared.thread_id,
            &prepared.canonical_workspace_root,
            &verification.command,
            wait_for_seconds,
        );
        let delivery_id = if prepared.surface == crate::tools::edit_files::EditSurface::EditFiles {
            id.clone()
        } else {
            legacy_verification_receipt_id(
                &receipt_id,
                &prepared.thread_id,
                &prepared.canonical_workspace_root,
                &verification.command,
                wait_for_seconds,
            )
        };
        VerificationPlan {
            id,
            delivery_id,
            command: verification.command.clone(),
            wait_for_seconds,
        }
    });
    Some(MutationMetadata {
        receipt_id,
        fingerprint,
        legacy_fingerprint,
        response_tool: prepared.surface.tool_name().to_string(),
        thread_id: prepared.thread_id.clone(),
        workspace_root: prepared.workspace_root.clone(),
        target_paths: prepared.target_paths(),
        verification,
    })
}

fn mutation_receipts(
    metadata: &MutationMetadata,
    status: String,
    postconditions: Vec<EditMutationPostcondition>,
    verification_pending: bool,
    uncommitted_paths: Vec<String>,
    next_undo_id: Option<String>,
    undo_ids: Vec<Option<String>>,
) -> Vec<EditMutationReceipt> {
    let committed_at_unix_ms = now_unix_ms();
    let verification_id = metadata.verification.as_ref().map(|plan| plan.id.clone());
    let verification_state = metadata.verification.as_ref().map(|_| {
        if !uncommitted_paths.is_empty() {
            EditVerificationState::Skipped
        } else if verification_pending {
            EditVerificationState::Pending
        } else if status == "completed" {
            EditVerificationState::Passed
        } else {
            EditVerificationState::Failed
        }
    });
    let mut receipts = vec![EditMutationReceipt {
        schema_version: 1,
        fingerprint: metadata.fingerprint.clone(),
        receipt_id: metadata.receipt_id.clone(),
        tool: "EditFiles".to_string(),
        status: status.clone(),
        committed_at_unix_ms,
        postconditions: postconditions.clone(),
        persisted: true,
        verification_pending,
        verification_id: verification_id.clone(),
        legacy_verification_id: None,
        verification_state,
        verification_execution: None,
        uncommitted_paths: uncommitted_paths.clone(),
        next_undo_id: next_undo_id.clone(),
        undo_ids: undo_ids.clone(),
    }];
    if let Some(fingerprint) = metadata.legacy_fingerprint.as_ref() {
        receipts.push(EditMutationReceipt {
            schema_version: 1,
            fingerprint: fingerprint.clone(),
            receipt_id: metadata.receipt_id.clone(),
            tool: metadata.response_tool.clone(),
            status,
            committed_at_unix_ms,
            postconditions,
            persisted: true,
            verification_pending,
            verification_id,
            legacy_verification_id: metadata
                .verification
                .as_ref()
                .map(|plan| plan.delivery_id.clone()),
            verification_state,
            verification_execution: None,
            uncommitted_paths,
            next_undo_id,
            undo_ids,
        });
    }
    receipts
}

fn canonicalize_replay_receipt_pair(
    metadata: &MutationMetadata,
    receipt: &EditMutationReceipt,
    exact_variant: bool,
    undo_ids: Vec<Option<String>>,
    next_undo_id: Option<String>,
) -> (EditMutationReceipt, Vec<EditMutationReceipt>) {
    let partial = !receipt.uncommitted_paths.is_empty();
    let (status, verification_pending, verification_state, verification_execution) =
        if exact_variant {
            (
                receipt.status.clone(),
                receipt.verification_pending,
                receipt.verification_state,
                receipt.verification_execution.clone(),
            )
        } else if metadata.verification.is_some() {
            if partial {
                (
                    "completed_with_issues".to_string(),
                    false,
                    Some(EditVerificationState::Skipped),
                    None,
                )
            } else {
                (
                    "completed_with_issues".to_string(),
                    true,
                    Some(EditVerificationState::Pending),
                    None,
                )
            }
        } else {
            (
                if partial { "completed_with_issues" } else { "completed" }.to_string(),
                false,
                None,
                None,
            )
        };
    let canonical = EditMutationReceipt {
        schema_version: 1,
        fingerprint: metadata.fingerprint.clone(),
        receipt_id: metadata.receipt_id.clone(),
        tool: "EditFiles".to_string(),
        status: status.clone(),
        committed_at_unix_ms: receipt.committed_at_unix_ms,
        postconditions: receipt.postconditions.clone(),
        persisted: true,
        verification_pending,
        verification_id: metadata.verification.as_ref().map(|plan| plan.id.clone()),
        legacy_verification_id: None,
        verification_state,
        verification_execution: verification_execution.clone(),
        uncommitted_paths: receipt.uncommitted_paths.clone(),
        next_undo_id: next_undo_id.clone(),
        undo_ids: undo_ids.clone(),
    };
    let mut pair = vec![canonical.clone()];
    if let Some(fingerprint) = metadata.legacy_fingerprint.as_ref() {
        pair.push(EditMutationReceipt {
            schema_version: 1,
            fingerprint: fingerprint.clone(),
            receipt_id: metadata.receipt_id.clone(),
            tool: metadata.response_tool.clone(),
            status,
            committed_at_unix_ms: receipt.committed_at_unix_ms,
            postconditions: receipt.postconditions.clone(),
            persisted: true,
            verification_pending,
            verification_id: metadata.verification.as_ref().map(|plan| plan.id.clone()),
            legacy_verification_id: metadata
                .verification
                .as_ref()
                .map(|plan| plan.delivery_id.clone()),
            verification_state,
            verification_execution,
            uncommitted_paths: receipt.uncommitted_paths.clone(),
            next_undo_id,
            undo_ids,
        });
    }
    (canonical, pair)
}

#[cfg(test)]
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

pub(super) fn canonical_verification_receipt_id(
    mutation_receipt_id: &str,
    thread_id: &str,
    workspace_root: &str,
    command: &str,
    wait_for_seconds: Option<f32>,
) -> String {
    let mut hasher = Sha256::new();
    hash_bytes(&mut hasher, b"EditFilesVerification/v1");
    hash_bytes(&mut hasher, mutation_receipt_id.as_bytes());
    hash_bytes(&mut hasher, thread_id.as_bytes());
    hash_bytes(&mut hasher, workspace_root.as_bytes());
    hash_bytes(&mut hasher, command.trim().as_bytes());
    hash_bytes(
        &mut hasher,
        wait_for_seconds.map_or_else(String::new, |wait| wait.to_bits().to_string()).as_bytes(),
    );
    let fingerprint = hex_digest(hasher.finalize().as_slice());
    format!("verify_{}", &fingerprint[..24])
}

pub(super) fn verification_receipt_id_for_prepared(
    prepared: &PreparedEditContext,
) -> Option<String> {
    mutation_metadata(prepared)?.verification.map(|plan| plan.id)
}

pub(super) fn verification_delivery_id_for_prepared(
    prepared: &PreparedEditContext,
) -> Option<String> {
    mutation_metadata(prepared)?.verification.map(|plan| plan.delivery_id)
}

fn legacy_verification_receipt_id(
    mutation_receipt_id: &str,
    thread_id: &str,
    workspace_root: &str,
    command: &str,
    wait_for_seconds: Option<f32>,
) -> String {
    let mut hasher = Sha256::new();
    hash_bytes(&mut hasher, b"VerifyEdit/v2");
    hash_bytes(&mut hasher, mutation_receipt_id.as_bytes());
    hash_bytes(&mut hasher, thread_id.as_bytes());
    hash_bytes(&mut hasher, workspace_root.as_bytes());
    hash_bytes(&mut hasher, command.trim().as_bytes());
    hash_bytes(
        &mut hasher,
        wait_for_seconds.map_or_else(String::new, |wait| wait.to_bits().to_string()).as_bytes(),
    );
    let fingerprint = hex_digest(hasher.finalize().as_slice());
    format!("verify_{}", &fingerprint[..24])
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
    if !receipt.uncommitted_paths.is_empty() {
        return partial_mutation_replay_result(metadata, receipt);
    }
    let verification_changed = metadata.verification.as_ref().is_some_and(|verification| {
        receipt.verification_id.as_deref() != Some(verification.id.as_str())
    });
    let needs_verification = metadata.verification.as_ref().filter(|_| {
        receipt.verification_pending || receipt.status != "completed" || verification_changed
    });
    let text = if needs_verification.is_some() {
        format!(
            "IDEMPOTENT REPLAY: {} already committed this exact mutation as {}. Winx verified the \
             target hashes and did not write files again. Execute the receipt-bound verification \
             nextAction separately.",
            metadata.response_tool, receipt.receipt_id
        )
    } else {
        format!(
            "IDEMPOTENT REPLAY: {} already committed this exact mutation as {}. Winx verified the \
             target hashes and did not write files or run verification again.",
            metadata.response_tool, receipt.receipt_id
        )
    };
    let mut result = CallToolResult::success(vec![ContentBlock::text(text.clone())]);
    let mut structured = json!({
        "status": receipt.status,
        "tool": metadata.response_tool,
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
        structured["data"]["verification_id"] = Value::String(verification.delivery_id.clone());
        structured["data"]["verification_status"] = Value::String(
            if receipt.verification_pending || verification_changed { "pending" } else { "failed" }
                .to_string(),
        );
        structured["nextAction"] = json!({
            "tool": verification_retry_tool(metadata),
            "instruction": if receipt.verification_pending || verification_changed {
                "The edit is already committed. Run this exact verification receipt separately; never repeat the edit."
            } else {
                "The edit is already committed. Make corrective changes first, then run this exact verification receipt; never repeat the edit."
            },
            "arguments": verification_retry_arguments(metadata, verification)
        });
    }
    if let Some(next_undo_id) = receipt.next_undo_id.as_ref() {
        structured["data"]["next_undo_id"] = Value::String(next_undo_id.clone());
    }
    if !receipt.undo_ids.is_empty() {
        structured["data"]["undo_ids"] = json!(receipt.undo_ids);
    }
    result.structured_content = Some(structured);
    result
}

fn partial_mutation_replay_result(
    metadata: &MutationMetadata,
    receipt: &EditMutationReceipt,
) -> CallToolResult {
    let committed_paths =
        receipt.postconditions.iter().map(|condition| condition.path.clone()).collect::<Vec<_>>();
    let message = format!(
        "IDEMPOTENT PARTIAL REPLAY: {} already committed {} file(s) before a write-stage failure. Winx did not write them again. Re-read and retry only the {} uncommitted file(s).",
        metadata.response_tool,
        committed_paths.len(),
        receipt.uncommitted_paths.len()
    );
    let mut read_arguments = json!({
        "file_paths": receipt.uncommitted_paths,
        "thread_id": metadata.thread_id,
    });
    if let Some(workspace_root) = metadata.workspace_root.as_ref() {
        read_arguments["workspace_root"] = Value::String(workspace_root.clone());
    }
    let required_reads = receipt
        .uncommitted_paths
        .iter()
        .map(|path| json!({"path": path, "ranges": []}))
        .collect::<Vec<_>>();
    let mut result = CallToolResult::success(vec![ContentBlock::text(message.clone())]);
    result.structured_content = Some(json!({
        "status": "completed_with_issues",
        "tool": metadata.response_tool,
        "message": message,
        "errorCode": "partial_commit",
        "retryable": false,
        "retrySameCall": false,
        "requiredReads": required_reads,
        "nextAction": {
            "tool": "ReadFiles",
            "instruction": "Read only the uncommitted suffix, then issue a new edit containing only those files. Never repeat the original batch.",
            "arguments": read_arguments
        },
        "data": {
            "thread_id": metadata.thread_id,
            "workspace_root": metadata.workspace_root,
            "edit_applied": true,
            "verification_skipped": metadata.verification.is_some(),
            "verification_status": metadata.verification.as_ref().map(|_| "skipped"),
            "committed_paths": committed_paths,
            "uncommitted_paths": receipt.uncommitted_paths,
            "mutation_receipt_id": receipt.receipt_id,
            "mutation_transition": "partial_replayed",
            "mutation_replayed": true,
            "mutation_receipt_persisted": receipt.persisted
            ,"undo_ids": receipt.undo_ids
        }
    }));
    result
}

fn verification_replay_result(verification_id: &str, thread_id: &str) -> CallToolResult {
    let message = "IDEMPOTENT VERIFICATION REPLAY: this receipt already passed. Winx did not run the command again.";
    let mut result = CallToolResult::success(vec![ContentBlock::text(message)]);
    result.structured_content = Some(json!({
        "status": "completed",
        "tool": "BashCommand",
        "message": message,
        "retryable": false,
        "retrySameCall": false,
        "requiredReads": [],
        "data": {
            "thread_id": thread_id,
            "action": "verify",
            "verification_id": verification_id,
            "verification_passed": true,
            "verification_replayed": true
        }
    }));
    result
}

fn verification_retry_arguments(
    metadata: &MutationMetadata,
    verification: &VerificationPlan,
) -> Value {
    let mut arguments = if metadata.response_tool == "EditFiles" {
        json!({
            "action_json": {
                "type": "verify",
                "verification_id": verification.id,
                "command": verification.command,
            },
            "thread_id": metadata.thread_id,
        })
    } else {
        json!({
            "verification_id": verification.delivery_id,
            "command": verification.command,
            "thread_id": metadata.thread_id,
        })
    };
    if let Some(workspace_root) = metadata.workspace_root.as_ref() {
        arguments["workspace_root"] = Value::String(workspace_root.clone());
    }
    if let Some(wait) = verification.wait_for_seconds {
        arguments["wait_for_seconds"] = json!(wait);
    }
    arguments
}

fn verification_retry_tool(metadata: &MutationMetadata) -> &'static str {
    if metadata.response_tool == "EditFiles" {
        "BashCommand"
    } else {
        "VerifyEdit"
    }
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
        metadata.response_tool, receipt.receipt_id
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
        "tool": metadata.response_tool,
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
            hash_len(hasher, values.len());
            for value in values {
                hash_json(hasher, value);
            }
        }
        Value::Object(values) => {
            hasher.update(b"o");
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            hash_len(hasher, keys.len());
            for key in keys {
                hash_bytes(hasher, key.as_bytes());
                hash_json(hasher, &values[key]);
            }
        }
    }
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hash_len(hasher, bytes.len());
    hasher.update(bytes);
}

fn hash_len(hasher: &mut Sha256, len: usize) {
    hasher.update(u64::try_from(len).unwrap_or(u64::MAX).to_le_bytes());
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

#[cfg(test)]
fn string_argument(arguments: Option<&Value>, key: &str) -> Option<String> {
    arguments?.get(key)?.as_str().map(str::to_string)
}

fn now_unix_ms() -> u64 {
    let millis = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::tools::edit_files::{
        EditChange, EditCommand, EditSurface, EditVerification, PreparedEditContext,
    };

    fn prepared(content: &str, verification: bool) -> PreparedEditContext {
        let original_arguments = json!({
            "thread_id": "thread",
            "workspace_root": "/workspace",
            "file_path": "/workspace/file.rs",
            "percentage_to_change": 100,
            "text_or_search_replace_blocks": content,
            "verify_command": verification.then_some("cargo test"),
            "verify_wait_for_seconds": verification.then_some(5)
        });
        PreparedEditContext {
            surface: EditSurface::FileWriteOrEdit,
            command: EditCommand::Apply {
                changes: vec![EditChange::Replace {
                    file_path: "/workspace/file.rs".to_string(),
                    content: content.to_string(),
                }],
            },
            verification: verification.then(|| EditVerification {
                command: "cargo test".to_string(),
                wait_for_seconds: Some(5.0),
            }),
            thread_id: "thread".to_string(),
            workspace_root: Some("/workspace".to_string()),
            canonical_workspace_root: "/workspace".to_string(),
            targets: vec![crate::tools::edit_files::CanonicalEditTarget::from_preflight(
                "/workspace/file.rs".into(),
            )
            .expect("canonical test target")],
            original_arguments,
            effective_permissions: crate::tool_policy::ToolPolicy::default().edit_permissions(),
        }
    }

    fn modern_prepared(command: &str) -> PreparedEditContext {
        let mut prepared = prepared("content", true);
        prepared.surface = EditSurface::EditFiles;
        prepared.verification =
            Some(EditVerification { command: command.to_string(), wait_for_seconds: Some(5.0) });
        prepared.original_arguments = json!({
            "operation": "apply",
            "files": [{
                "file_path": "/workspace/file.rs",
                "mode": "replace",
                "content": "content"
            }],
            "verify_command": command,
            "verify_wait_for_seconds": 5,
            "thread_id": "thread",
            "workspace_root": "/workspace"
        });
        prepared
    }

    fn pending_verification_receipt(
        verification_id: &str,
        state: EditVerificationState,
        execution: Option<EditVerificationExecution>,
    ) -> EditMutationReceipt {
        EditMutationReceipt {
            schema_version: 1,
            fingerprint: "verification-fingerprint".to_string(),
            receipt_id: "edit-verification-lifecycle".to_string(),
            tool: "EditFiles".to_string(),
            status: "completed_with_issues".to_string(),
            committed_at_unix_ms: now_unix_ms(),
            postconditions: Vec::new(),
            persisted: true,
            verification_pending: true,
            verification_id: Some(verification_id.to_string()),
            legacy_verification_id: None,
            verification_state: Some(state),
            verification_execution: execution,
            uncommitted_paths: Vec::new(),
            next_undo_id: None,
            undo_ids: Vec::new(),
        }
    }

    #[test]
    fn mutation_fingerprint_is_object_order_independent() {
        let left = prepared("a", false);
        let mut right = prepared("a", false);
        right.original_arguments = json!({
            "text_or_search_replace_blocks": "a",
            "percentage_to_change": 100,
            "file_path": "/workspace/file.rs",
            "workspace_root": "/workspace",
            "thread_id": "thread"
        });
        assert_eq!(
            mutation_metadata(&left).map(|item| item.fingerprint),
            mutation_metadata(&right).map(|item| item.fingerprint)
        );
    }

    #[test]
    fn mutation_fingerprint_changes_with_payload() {
        let left = prepared("a", false);
        let right = prepared("b", false);
        assert_ne!(
            mutation_metadata(&left).map(|item| item.fingerprint),
            mutation_metadata(&right).map(|item| item.fingerprint)
        );
    }

    #[test]
    fn schema_zero_legacy_receipt_migrates_to_atomic_canonical_shadow_pair() {
        let legacy = prepared("content", true);
        let metadata = mutation_metadata(&legacy).expect("legacy metadata");
        let old_fingerprint = metadata.legacy_fingerprint.as_ref().expect("legacy fingerprint");
        let old = EditMutationReceipt {
            schema_version: 0,
            fingerprint: old_fingerprint.clone(),
            receipt_id: "schema-zero-receipt".to_string(),
            tool: "FileWriteOrEdit".to_string(),
            status: "completed".to_string(),
            committed_at_unix_ms: 1_000,
            postconditions: vec![EditMutationPostcondition {
                path: "/workspace/file.rs".to_string(),
                sha256: "hash".to_string(),
            }],
            persisted: true,
            verification_pending: false,
            verification_id: None,
            legacy_verification_id: None,
            verification_state: None,
            verification_execution: None,
            uncommitted_paths: Vec::new(),
            next_undo_id: None,
            undo_ids: Vec::new(),
        };
        let (_, pair) = canonicalize_replay_receipt_pair(&metadata, &old, false, Vec::new(), None);
        assert_eq!(pair.len(), 2);
        assert!(pair.iter().all(|receipt| receipt.schema_version == 1));
        assert!(pair.iter().all(|receipt| receipt.receipt_id == metadata.receipt_id));

        let mut state = crate::state::bash_state::BashState::new();
        state.record_edit_mutation_receipts(pair);
        assert!(state.edit_mutation_receipt(&metadata.fingerprint, 1_001, 10_000).is_some());
        assert!(state.edit_mutation_receipt(old_fingerprint, 1_001, 10_000).is_some());
        let verification = metadata.verification.expect("verification metadata");
        let shadow = state
            .edit_mutation_receipt_by_legacy_verification_id(
                &verification.delivery_id,
                1_001,
                10_000,
            )
            .expect("migrated legacy verification shadow");
        assert_eq!(shadow.verification_id.as_deref(), Some(verification.id.as_str()));
    }

    #[test]
    fn distinct_mutations_with_same_verification_command_have_distinct_receipts() {
        let first = mutation_metadata(&prepared("first", true)).expect("first metadata");
        let second = mutation_metadata(&prepared("second", true)).expect("second metadata");
        assert_ne!(first.receipt_id, second.receipt_id);
        assert_ne!(
            first.verification.as_ref().map(|plan| &plan.id),
            second.verification.as_ref().map(|plan| &plan.id)
        );
        assert_ne!(
            first.verification.as_ref().map(|plan| &plan.delivery_id),
            second.verification.as_ref().map(|plan| &plan.delivery_id)
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

    #[tokio::test]
    async fn abandoned_reservation_retries_launch_and_only_bound_running_state_polls() {
        let service = WinxService::new();
        let command = "cargo test";
        let wait = Some(5.0);
        let verification_id = canonical_verification_receipt_id(
            "edit-verification-lifecycle",
            "",
            "/workspace",
            command,
            wait,
        );
        let (slot, setup_guard) = service.session_for("").await;
        let mut state = crate::state::bash_state::BashState::new();
        state.current_thread_id.clear();
        state.cwd = "/workspace".into();
        state.workspace_root = "/workspace".into();
        state.initialized = true;
        state.record_edit_mutation_receipt(pending_verification_receipt(
            &verification_id,
            EditVerificationState::Pending,
            None,
        ));
        *slot.lock().await = Some(state);
        drop(setup_guard);

        let first = service
            .begin_receipt_bound_verification("", &verification_id, command, wait)
            .await
            .expect("first reservation");
        let VerificationStart::Execute(first_owner) = first else {
            panic!("pending receipt must reserve an execution");
        };
        // Models cancellation between reservation and launch. Dropping the
        // request owner releases single-flight ownership while the durable
        // Reserved state remains deliberately unbound.
        drop(first_owner);
        let second = service
            .begin_receipt_bound_verification("", &verification_id, command, wait)
            .await
            .expect("retry abandoned reservation");
        let VerificationStart::Execute(second_owner) = second else {
            panic!("an abandoned unbound reservation must never poll a foreground command");
        };
        drop(second_owner);

        {
            let mut guard = slot.lock().await;
            let state = guard.as_mut().expect("state");
            assert!(state.update_edit_mutation_verification(
                &verification_id,
                "completed_with_issues",
                EditVerificationState::Running,
                None,
                now_unix_ms(),
                MUTATION_RECEIPT_TTL_MS,
            ));
        }
        let unbound_running = service
            .begin_receipt_bound_verification("", &verification_id, command, wait)
            .await
            .expect("unbound running recovery");
        let VerificationStart::Execute(unbound_owner) = unbound_running else {
            panic!("Running without an execution binding must not observe foreign foreground");
        };
        drop(unbound_owner);

        let binding = EditVerificationExecution {
            generation: 17,
            guardian_epoch: "guardian".to_string(),
            session_epoch: "session".to_string(),
        };
        {
            let mut guard = slot.lock().await;
            let state = guard.as_mut().expect("state");
            assert!(state.update_edit_mutation_verification(
                &verification_id,
                "completed_with_issues",
                EditVerificationState::Running,
                Some(&binding),
                now_unix_ms(),
                MUTATION_RECEIPT_TTL_MS,
            ));
        }
        let bound_running = service
            .begin_receipt_bound_verification("", &verification_id, command, wait)
            .await
            .expect("bound running recovery");
        let VerificationStart::Poll(owner, Some(actual)) = bound_running else {
            panic!("only a verifiably bound running receipt may poll");
        };
        assert_eq!(actual, binding);
        drop(owner);
    }

    #[test]
    fn interrupted_verification_replay_points_only_to_verify_edit(
    ) -> std::result::Result<(), &'static str> {
        let metadata = mutation_metadata(&prepared("content", true)).ok_or("missing metadata")?;
        let receipt = EditMutationReceipt {
            schema_version: 1,
            fingerprint: metadata.fingerprint.clone(),
            receipt_id: metadata.receipt_id.clone(),
            tool: "EditFiles".to_string(),
            status: "completed_with_issues".to_string(),
            committed_at_unix_ms: now_unix_ms(),
            postconditions: vec![EditMutationPostcondition {
                path: "/workspace/file.rs".to_string(),
                sha256: "hash".to_string(),
            }],
            persisted: true,
            verification_pending: true,
            verification_id: metadata.verification.as_ref().map(|plan| plan.id.clone()),
            legacy_verification_id: None,
            verification_state: Some(EditVerificationState::Pending),
            verification_execution: None,
            uncommitted_paths: Vec::new(),
            next_undo_id: None,
            undo_ids: Vec::new(),
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

    #[test]
    fn changed_verification_replays_commit_into_receipt_bound_bash_only(
    ) -> std::result::Result<(), &'static str> {
        let old = mutation_metadata(&modern_prepared("cargo test")).ok_or("old metadata")?;
        let new = mutation_metadata(&modern_prepared("cargo check")).ok_or("new metadata")?;
        assert_eq!(old.fingerprint, new.fingerprint);
        assert_ne!(
            old.verification.as_ref().map(|plan| &plan.id),
            new.verification.as_ref().map(|plan| &plan.id)
        );
        let receipt = EditMutationReceipt {
            schema_version: 1,
            fingerprint: old.fingerprint,
            receipt_id: old.receipt_id,
            tool: "EditFiles".to_string(),
            status: "completed".to_string(),
            committed_at_unix_ms: now_unix_ms(),
            postconditions: vec![EditMutationPostcondition {
                path: "/workspace/file.rs".to_string(),
                sha256: "hash".to_string(),
            }],
            persisted: true,
            verification_pending: false,
            verification_id: old.verification.map(|plan| plan.id),
            legacy_verification_id: None,
            verification_state: Some(EditVerificationState::Passed),
            verification_execution: None,
            uncommitted_paths: Vec::new(),
            next_undo_id: None,
            undo_ids: Vec::new(),
        };

        let result = mutation_replay_result(&new, &receipt);
        let structured = result.structured_content.ok_or("missing structured replay")?;
        assert_eq!(structured["nextAction"]["tool"], "BashCommand");
        assert_eq!(structured["nextAction"]["arguments"]["action_json"]["type"], "verify");
        assert_eq!(structured["nextAction"]["arguments"]["action_json"]["command"], "cargo check");
        assert_eq!(structured["nextAction"]["arguments"]["wait_for_seconds"], 5.0);
        assert_eq!(structured["data"]["verification_status"], "pending");
        assert_eq!(structured["data"]["mutation_transition"], "replayed");
        Ok(())
    }

    #[test]
    fn changed_legacy_verification_rebuilds_a_resolvable_shadow(
    ) -> std::result::Result<(), &'static str> {
        let old_prepared = prepared("content", true);
        let old = mutation_metadata(&old_prepared).ok_or("old metadata")?;
        let mut new_prepared = prepared("content", true);
        new_prepared.verification = Some(EditVerification {
            command: "cargo check".to_string(),
            wait_for_seconds: Some(5.0),
        });
        new_prepared.original_arguments["verify_command"] = json!("cargo check");
        let new = mutation_metadata(&new_prepared).ok_or("new metadata")?;
        assert_eq!(old.fingerprint, new.fingerprint);
        assert_eq!(old.receipt_id, new.receipt_id);
        assert_ne!(old.legacy_fingerprint, new.legacy_fingerprint);

        let old_receipt = mutation_receipts(
            &old,
            "completed".to_string(),
            vec![EditMutationPostcondition {
                path: "/workspace/file.rs".to_string(),
                sha256: "hash".to_string(),
            }],
            false,
            Vec::new(),
            None,
            Vec::new(),
        )
        .into_iter()
        .next()
        .ok_or("old canonical receipt")?;
        let (canonical, pair) =
            canonicalize_replay_receipt_pair(&new, &old_receipt, false, Vec::new(), None);
        assert_eq!(canonical.verification_state, Some(EditVerificationState::Pending));
        let verification = new.verification.as_ref().ok_or("new verification")?;
        let shadow = pair.iter().find(|receipt| receipt.tool != "EditFiles").ok_or("shadow")?;
        assert_eq!(shadow.verification_id.as_deref(), Some(verification.id.as_str()));
        assert_eq!(
            shadow.legacy_verification_id.as_deref(),
            Some(verification.delivery_id.as_str())
        );
        let result = mutation_replay_result(&new, &canonical);
        let structured = result.structured_content.ok_or("structured replay")?;
        assert_eq!(structured["nextAction"]["tool"], "VerifyEdit");
        assert_eq!(
            structured["nextAction"]["arguments"]["verification_id"],
            verification.delivery_id
        );
        assert_eq!(structured["nextAction"]["arguments"]["command"], "cargo check");
        Ok(())
    }

    #[test]
    fn partial_receipt_replay_protects_committed_prefix_and_reads_only_suffix(
    ) -> std::result::Result<(), &'static str> {
        let mut prepared = modern_prepared("cargo test");
        prepared.verification = None;
        prepared.targets = ["/workspace/first.rs", "/workspace/second.rs"]
            .into_iter()
            .map(|path| {
                crate::tools::edit_files::CanonicalEditTarget::from_preflight(path.into())
                    .expect("canonical test target")
            })
            .collect();
        let metadata = mutation_metadata(&prepared).ok_or("missing metadata")?;
        let receipt = EditMutationReceipt {
            schema_version: 1,
            fingerprint: metadata.fingerprint.clone(),
            receipt_id: metadata.receipt_id.clone(),
            tool: "EditFiles".to_string(),
            status: "completed_with_issues".to_string(),
            committed_at_unix_ms: now_unix_ms(),
            postconditions: vec![EditMutationPostcondition {
                path: "/workspace/first.rs".to_string(),
                sha256: "first-hash".to_string(),
            }],
            persisted: true,
            verification_pending: false,
            verification_id: None,
            legacy_verification_id: None,
            verification_state: None,
            verification_execution: None,
            uncommitted_paths: vec!["/workspace/second.rs".to_string()],
            next_undo_id: None,
            undo_ids: vec![Some("undo-first".to_string())],
        };

        let result = mutation_replay_result(&metadata, &receipt);
        assert_ne!(result.is_error, Some(true));
        let structured = result.structured_content.ok_or("missing structured replay")?;
        assert_eq!(structured["status"], "completed_with_issues");
        assert_eq!(structured["errorCode"], "partial_commit");
        assert_eq!(structured["data"]["edit_applied"], true);
        assert_eq!(structured["data"]["committed_paths"], json!(["/workspace/first.rs"]));
        assert_eq!(structured["data"]["uncommitted_paths"], json!(["/workspace/second.rs"]));
        assert_eq!(structured["data"]["undo_ids"], json!(["undo-first"]));
        assert_eq!(
            structured["nextAction"]["arguments"]["file_paths"],
            json!(["/workspace/second.rs"])
        );
        assert!(!structured["nextAction"].to_string().contains("first.rs"));
        Ok(())
    }

    #[test]
    fn fixed_width_json_framing_distinguishes_ambiguous_value_boundaries() {
        let digest = |value: Value| {
            let mut hasher = Sha256::new();
            hash_json(&mut hasher, &value);
            hex_digest(hasher.finalize().as_slice())
        };
        assert_ne!(digest(json!(["ab", "c"])), digest(json!(["a", "bc"])));
        assert_ne!(digest(json!({"a": "bc"})), digest(json!({"ab": "c"})));
    }
}
