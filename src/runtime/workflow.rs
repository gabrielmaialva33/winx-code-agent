//! Non-consuming native terminal observations and idempotent prompt receipts.

use serde::{Deserialize, Serialize};

use crate::state::turn::TurnExplanation;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowObservation {
    pub output: String,
    pub output_included: bool,
    pub screen: Vec<String>,
    pub activity_seq: u64,
    pub explanation: TurnExplanation,
    pub prompt_receipt: Option<PromptReceipt>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptDelivery {
    Delivered,
    Uncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptOutcome {
    Waiting,
    AwaitingInput,
    AwaitingApproval,
    Exited,
    TimedOut,
    GenerationChanged,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PromptReceipt {
    pub prompt_id: String,
    pub delivery: PromptDelivery,
    pub activity_observed: bool,
    pub outcome: PromptOutcome,
    pub generation: u64,
    pub baseline_activity_seq: u64,
}

/// Kept beside the PTY; never serialized or written to telemetry.
#[derive(Clone, Debug)]
pub(crate) struct PromptRecord {
    pub digest: [u8; 32],
    pub recognizer: String,
    pub receipt: PromptReceipt,
    pub activity: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Retained evidence after a background PTY is reaped. No process or writer survives.
#[derive(Clone, Debug)]
pub struct RetainedWorkflow {
    pub(crate) generation: u64,
    pub(crate) activity_seq: u64,
    pub(crate) screen: Vec<String>,
    pub(crate) osc_title: String,
    pub(crate) osc_progress: String,
    pub(crate) receipts: std::collections::HashMap<String, PromptRecord>,
}
