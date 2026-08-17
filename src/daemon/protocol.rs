use std::io::{Error as IoError, ErrorKind};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::state::persistence::BashStateSnapshot;
use crate::types::BashCommand;

pub const PROTOCOL_MAJOR: u16 = 1;
pub const PROTOCOL_MINOR: u16 = 2;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct RunActionParams {
    pub snapshot: BashStateSnapshot,
    pub command: BashCommand,
    pub request_key: String,
    #[serde(default)]
    pub consumer_id: String,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RunActionResult {
    pub output: Option<String>,
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
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SessionParams {
    pub thread_id: String,
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
