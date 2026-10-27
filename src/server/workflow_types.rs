use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::execution_history::OutputSource;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub(super) struct ExecutionRequest {
    pub thread_id: String,
    pub workspace_root: PathBuf,
    pub action: ExecutionAction,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ExecutionAction {
    List {
        #[serde(default)]
        limit: Option<usize>,
    },
    Get {
        execution_id: String,
    },
    Read {
        execution_id: String,
        #[serde(default)]
        source: OutputSource,
        #[serde(default)]
        cursor: Option<String>,
        #[serde(default)]
        page_bytes: Option<usize>,
    },
    Explain {
        execution_id: String,
        #[serde(default)]
        recognizer: Option<String>,
    },
    Wait {
        execution_ids: Vec<String>,
        #[serde(default)]
        after: Option<String>,
        #[serde(default)]
        events: Vec<ExecutionWatchEvent>,
        #[serde(default)]
        timeout_seconds: Option<f32>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ExecutionWatchEvent {
    State,
    Output,
    Exited,
    AwaitingInput,
    AwaitingApproval,
    Busy,
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentBackend {
    #[default]
    Native,
    Herdr,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentOperation {
    List,
    Get,
    Explain,
    Read,
    Prompt,
    Wait,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub(super) struct AgentRequest {
    pub thread_id: String,
    pub workspace_root: PathBuf,
    #[serde(default)]
    pub backend: AgentBackend,
    pub action: AgentOperation,
    /// Native execution ID or an explicitly configured Herdr target alias.
    #[serde(default)]
    pub target: Option<String>,
    /// Required for a new native prompt; reused IDs never resend input.
    #[serde(default)]
    pub prompt_id: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub recognizer: Option<String>,
    #[serde(default)]
    pub timeout_seconds: Option<f32>,
    #[serde(default)]
    pub source: OutputSource,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub page_bytes: Option<usize>,
    /// Herdr read line limit.
    #[serde(default)]
    pub lines: Option<u32>,
    /// Herdr lifecycle states; native waits use a receipt or input-ready lifecycle.
    #[serde(default)]
    pub until: Vec<crate::tools::herdr::HerdrStatus>,
}

#[derive(Clone, Debug)]
pub(super) struct WorkflowAuthority {
    pub principal_key: String,
    pub bash_allowed: bool,
}

impl Default for WorkflowAuthority {
    fn default() -> Self {
        Self { principal_key: "local".into(), bash_allowed: true }
    }
}
