use std::io::{Error as IoError, ErrorKind};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::state::persistence::BashStateSnapshot;
use crate::tools::bash_command::BashCommandState;
use crate::types::BashCommand;

pub const PROTOCOL_MAJOR: u16 = 1;
pub const PROTOCOL_MINOR: u16 = 5;
pub const TYPED_ACTION_RESULT_CAPABILITY: &str = "typed_action_result";
pub const COMPACT_ACTION_OUTPUT_CAPABILITY: &str = "compact_action_output";
pub const GENERATION_BOUND_ACTIONS_CAPABILITY: &str = "generation_bound_actions";
pub const CANCELLABLE_ACTION_RESERVATIONS_CAPABILITY: &str = "cancellable_action_reservations";
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct RpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct RpcError {
    pub code: i32,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HelloResult {
    pub protocol_major: u16,
    pub protocol_minor: u16,
    pub capabilities: Vec<String>,
    pub max_frame_bytes: usize,
    pub daemon_epoch: String,
    pub daemon_pid: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RunActionParams {
    pub snapshot: BashStateSnapshot,
    pub command: BashCommand,
    pub request_key: String,
    #[serde(default)]
    pub consumer_id: String,
    #[serde(default, skip_serializing_if = "crate::runtime::ShellActionOptions::is_default")]
    pub options: crate::runtime::ShellActionOptions,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConfigureSessionTransition {
    FirstCall,
    ModeChange,
    Reset,
    WorkspaceChange,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ConfigureSessionParams {
    pub snapshot: BashStateSnapshot,
    pub transition: ConfigureSessionTransition,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ConfigureSessionResult {
    pub attach_hint: Option<String>,
    /// Existing guardians return their authoritative state so a restarted adapter
    /// can attach without resetting the PTY. Absent for protocol-1.2 guardians.
    #[serde(default)]
    pub snapshot: Option<BashStateSnapshot>,
    #[serde(default)]
    pub attached_existing: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RunActionResult {
    pub output: Option<String>,
    /// Trailer-free runtime output added in protocol 1.5. New adapters safely
    /// fall back to `output` when attached to a protocol-1.4 guardian.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_token: Option<crate::runtime::ShellExecutionToken>,
    #[serde(default)]
    pub output_truncated: bool,
    /// Runtime-owned state added in protocol 1.4. Optional only so a new adapter
    /// can reject an older guardian with an actionable upgrade error instead of
    /// trying to reconstruct state from attacker-controlled output text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<BashCommandState>,
    pub snapshot: BashStateSnapshot,
    pub error: Option<WireShellError>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct JournalRead {
    pub output: String,
    pub next_seq: u64,
    pub gap: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct JournalReadParams {
    pub thread_id: String,
    pub consumer_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub thread_id: String,
    pub cwd: String,
    pub shell_pid: Option<u32>,
    pub command_id: Option<String>,
    pub running: bool,
    pub background_command_ids: Vec<String>,
    /// Guardian-owned lifecycle clock. Optional for compatibility with guardians
    /// created by releases before protocol 1.3.
    #[serde(default)]
    pub created_at_unix_ms: Option<u64>,
    #[serde(default)]
    pub last_activity_unix_ms: Option<u64>,
    #[serde(default)]
    pub last_command_at_unix_ms: Option<u64>,
    #[serde(default)]
    pub ever_ran_command: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SessionParams {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_execution: Option<crate::runtime::ShellExecutionToken>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_guardian_epoch: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CancelActionParams {
    pub thread_id: String,
    pub cancellation_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_guardian_epoch: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct PruneParams {
    #[serde(default)]
    pub idle_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PruneResult {
    pub removed_thread_ids: Vec<String>,
    pub skipped_active_thread_ids: Vec<String>,
    pub stale_socket_count: usize,
    pub unreachable_guardian_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub(crate) enum WireShellError {
    BashStateNotInitialized,
    ShellInitialization(String),
    CommandExecution(String),
    NoActiveCommand(String),
    BackgroundSessionNotFound(String),
    EmptyInteractiveInput(String),
    InteractiveTargetNotRunning(String),
    CommandAlreadyRunning { current_command: String, duration_seconds: f64 },
    CommandNotAllowed(String),
    ThreadIdMismatch(String),
    InvalidInput(String),
    Other(String),
}

pub(crate) async fn write_json_frame<W, T>(writer: &mut W, value: &T) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(value)
        .map_err(|error| IoError::new(ErrorKind::InvalidData, error.to_string()))?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(IoError::new(
            ErrorKind::InvalidData,
            format!("JSON-RPC frame exceeds {MAX_FRAME_BYTES} bytes"),
        ));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| IoError::new(ErrorKind::InvalidData, "JSON-RPC frame is too large"))?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await
}

pub(crate) async fn read_json_frame<R, T>(reader: &mut R) -> std::io::Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(IoError::new(
            ErrorKind::InvalidData,
            format!("invalid JSON-RPC frame length: {length}"),
        ));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload)
        .map_err(|error| IoError::new(ErrorKind::InvalidData, error.to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn guardian_1_4_params_default_new_options_and_do_not_forward_wait_policy() {
        let command: BashCommand = serde_json::from_value(serde_json::json!({
            "command": "printf compatible",
            "wait_policy": "until_complete",
            "thread_id": "guardian14"
        }))
        .expect("adapter command");
        let mut wire = serde_json::to_value(RunActionParams {
            snapshot: crate::state::bash_state::BashState::new().snapshot(),
            command,
            request_key: "request14".to_string(),
            consumer_id: "consumer14".to_string(),
            options: crate::runtime::ShellActionOptions::default(),
        })
        .expect("current params");
        let object = wire.as_object_mut().expect("params object");
        assert!(object.get("options").is_none());
        assert!(object["command"].get("wait_policy").is_none());

        let decoded: RunActionParams = serde_json::from_value(wire).expect("protocol 1.4 params");
        assert_eq!(decoded.options, crate::runtime::ShellActionOptions::default());
    }

    #[test]
    fn guardian_1_4_result_defaults_compact_and_generation_extensions() {
        let mut wire = serde_json::to_value(RunActionResult {
            output: Some("legacy output".to_string()),
            compact_output: Some("compact output".to_string()),
            command_generation: Some(7),
            execution_token: None,
            output_truncated: false,
            state: None,
            snapshot: crate::state::bash_state::BashState::new().snapshot(),
            error: None,
        })
        .expect("current result");
        let object = wire.as_object_mut().expect("result object");
        object.remove("compact_output");
        object.remove("command_generation");
        object.remove("execution_token");
        object.remove("output_truncated");

        let decoded: RunActionResult = serde_json::from_value(wire).expect("protocol 1.4 result");
        assert_eq!(decoded.output.as_deref(), Some("legacy output"));
        assert_eq!(decoded.compact_output, None);
        assert_eq!(decoded.command_generation, None);
        assert_eq!(decoded.execution_token, None);
        assert!(!decoded.output_truncated);
    }

    #[test]
    fn compact_wire_result_contains_one_output_payload() {
        let compact = "x".repeat(1024);
        let wire = serde_json::to_value(RunActionResult {
            output: None,
            compact_output: Some(compact.clone()),
            command_generation: Some(9),
            execution_token: None,
            output_truncated: false,
            state: None,
            snapshot: crate::state::bash_state::BashState::new().snapshot(),
            error: None,
        })
        .expect("compact result");
        assert!(wire["output"].is_null());
        assert_eq!(wire["compact_output"], compact);
        let encoded = serde_json::to_string(&wire).expect("wire bytes");
        assert_eq!(encoded.matches(&compact).count(), 1, "compact payload must not be duplicated");
    }
}
