use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::net::{UnixListener, UnixStream};

use super::lifecycle::{GuardianLifecycle, GuardianLimits};
use super::protocol::{
    read_json_frame, write_json_frame, ConfigureSessionParams, HelloResult, JournalReadParams,
    PruneParams, RpcError, RpcRequest, RpcResponse, RunActionParams, SessionInfo, SessionParams,
    MAX_FRAME_BYTES, PROTOCOL_MAJOR, PROTOCOL_MINOR,
};
use crate::errors::{Result, WinxError};
use crate::types::normalize_thread_id;

/// Stable control plane. Each logical session is owned by a separate guardian
/// process, so restarting this process does not close any PTY master.
pub struct ControlServer {
    listener: UnixListener,
    socket_path: PathBuf,
    lifecycle: Arc<GuardianLifecycle>,
    epoch: String,
}

impl ControlServer {
    pub async fn bind(socket_path: impl AsRef<Path>) -> Result<Self> {
        let socket_path = socket_path.as_ref().to_path_buf();
        let parent = socket_path.parent().ok_or_else(|| {
            WinxError::ConfigurationError("daemon socket must have a parent directory".to_string())
        })?;
        tokio::fs::create_dir_all(parent).await?;
        tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).await?;
        let guardian_dir = parent.join("guardians");
        tokio::fs::create_dir_all(&guardian_dir).await?;
        tokio::fs::set_permissions(&guardian_dir, std::fs::Permissions::from_mode(0o700)).await?;

        if tokio::fs::try_exists(&socket_path).await? {
            if UnixStream::connect(&socket_path).await.is_ok() {
                return Err(WinxError::ConfigurationError(format!(
                    "a winxd instance is already listening at {}",
                    socket_path.display()
                )));
            }
            tokio::fs::remove_file(&socket_path).await?;
        }
        let listener = UnixListener::bind(&socket_path)?;
        tokio::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)).await?;
        let lifecycle = Arc::new(GuardianLifecycle::new(
            guardian_dir,
            guardian_binary()?,
            GuardianLimits::from_env()?,
        ));

        Ok(Self {
            listener,
            socket_path,
            lifecycle,
            epoch: format!("{:016x}", rand::random::<u64>()),
        })
    }

    pub async fn serve(self) -> Result<()> {
        self.lifecycle.clone().spawn_sweeper();
        loop {
            let (stream, _) = self.listener.accept().await?;
            if !same_uid(&stream)? {
                tracing::warn!("Rejected winxd control connection from a different uid");
                continue;
            }
            let lifecycle = self.lifecycle.clone();
            let epoch = self.epoch.clone();
            tokio::spawn(async move {
                if let Err(error) = serve_connection(stream, lifecycle, epoch).await {
                    tracing::debug!("winxd control client disconnected: {error}");
                }
            });
        }
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

async fn serve_connection(
    mut stream: UnixStream,
    lifecycle: Arc<GuardianLifecycle>,
    epoch: String,
) -> Result<()> {
    loop {
        let request: RpcRequest = match read_json_frame(&mut stream).await {
            Ok(request) => request,
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let response = dispatch(request, &lifecycle, &epoch).await;
        write_json_frame(&mut stream, &response).await?;
    }
}

#[allow(clippy::too_many_lines)]
async fn dispatch(request: RpcRequest, lifecycle: &GuardianLifecycle, epoch: &str) -> RpcResponse {
    if request.jsonrpc != "2.0" {
        return rpc_error(request.id, -32600, "JSON-RPC version must be 2.0");
    }
    if request.method == "winx.hello" {
        return rpc_result(
            request.id,
            &HelloResult {
                protocol_major: PROTOCOL_MAJOR,
                protocol_minor: PROTOCOL_MINOR,
                capabilities: vec![
                    "per_session_guardians".to_string(),
                    "planned_control_restart".to_string(),
                    "guardian_quota".to_string(),
                    "guardian_idle_ttl".to_string(),
                    "guardian_activity_clock".to_string(),
                    "unused_session_ttl".to_string(),
                    "quota_reclaims_unused".to_string(),
                    "session.configure".to_string(),
                    "shell.run_action".to_string(),
                    "session.list".to_string(),
                    "session.read_output".to_string(),
                    "session.kill".to_string(),
                    "session.prune".to_string(),
                    "multi_consumer_cursors".to_string(),
                    "idempotency".to_string(),
                ],
                max_frame_bytes: MAX_FRAME_BYTES,
                daemon_epoch: epoch.to_string(),
                daemon_pid: std::process::id(),
            },
        );
    }

    if request.method == "session.list" {
        return match list_sessions(lifecycle.guardian_dir()).await {
            Ok(sessions) => rpc_result(request.id, &sessions),
            Err(error) => rpc_error(request.id, -32000, &error.to_string()),
        };
    }

    if request.method == "session.prune" {
        let params = match decode::<PruneParams>(request.params) {
            Ok(params) => params,
            Err(error) => return rpc_error(request.id, -32602, &error.to_string()),
        };
        return match lifecycle.prune(params.idle_seconds).await {
            Ok(result) => rpc_result(request.id, &result),
            Err(error) => rpc_error(request.id, -32003, &error.to_string()),
        };
    }

    let thread_id = match request_thread_id(&request) {
        Ok(thread_id) if !thread_id.is_empty() => thread_id,
        Ok(_) => return rpc_error(request.id, -32602, "explicit thread_id is required"),
        Err(error) => return rpc_error(request.id, -32602, &error.to_string()),
    };
    let guardian_socket = lifecycle.socket_for(&thread_id);
    if matches!(request.method.as_str(), "session.configure" | "shell.run_action") {
        if let Err(error) = lifecycle.ensure_guardian(&thread_id, &guardian_socket).await {
            return rpc_error(request.id, -32001, &error.to_string());
        }
    }
    if request.method == "session.kill" {
        match tokio::fs::try_exists(&guardian_socket).await {
            Ok(true) => {}
            Ok(false) => return rpc_result(request.id, &false),
            Err(error) => return rpc_error(request.id, -32002, &error.to_string()),
        }
    }

    let kill_after = request.method == "session.kill";
    let request_id = request.id;
    let (response, hello) = match relay(&guardian_socket, request).await {
        Ok(result) => result,
        Err(error) => return rpc_error(request_id, -32002, &error.to_string()),
    };

    if kill_after && response.error.is_none() {
        if let Err(error) = lifecycle.finish_kill(&guardian_socket, hello.daemon_pid).await {
            return rpc_error(
                request_id,
                -32003,
                &format!("session state was removed, but guardian cleanup failed: {error}"),
            );
        }
    } else {
        lifecycle.note_activity(&guardian_socket, &thread_id, hello.daemon_pid).await;
    }
    response
}

fn request_thread_id(request: &RpcRequest) -> Result<String> {
    let thread_id = match request.method.as_str() {
        "session.configure" => {
            decode::<ConfigureSessionParams>(request.params.clone())?.snapshot.chat_id
        }
        "shell.run_action" => decode::<RunActionParams>(request.params.clone())?.command.thread_id,
        "session.read_output" => decode::<JournalReadParams>(request.params.clone())?.thread_id,
        "session.info" | "session.kill" | "session.interrupt" => {
            decode::<SessionParams>(request.params.clone())?.thread_id
        }
        _ => {
            return Err(WinxError::InvalidInput(format!(
                "unsupported control method {}",
                request.method
            )))
        }
    };
    Ok(normalize_thread_id(&thread_id))
}

async fn relay(socket: &Path, request: RpcRequest) -> Result<(RpcResponse, HelloResult)> {
    let mut stream = UnixStream::connect(socket).await.map_err(|error| {
        WinxError::ShellInitializationError(format!(
            "guardian at {} is unavailable: {error}",
            socket.display()
        ))
    })?;
    let hello_id = rand::random::<u64>();
    write_json_frame(
        &mut stream,
        &RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: hello_id,
            method: "winx.hello".to_string(),
            params: serde_json::json!({ "protocol_major": PROTOCOL_MAJOR }),
        },
    )
    .await?;
    let hello_response: RpcResponse = read_json_frame(&mut stream).await?;
    let hello: HelloResult = decode(hello_response.result.ok_or_else(|| {
        WinxError::ConfigurationError("guardian handshake omitted result".to_string())
    })?)?;
    if hello.protocol_major != PROTOCOL_MAJOR {
        return Err(WinxError::ConfigurationError(format!(
            "guardian protocol major {} is incompatible with control {}",
            hello.protocol_major, PROTOCOL_MAJOR
        )));
    }
    write_json_frame(&mut stream, &request).await?;
    let response = read_json_frame(&mut stream).await?;
    Ok((response, hello))
}

async fn list_sessions(guardian_dir: &Path) -> Result<Vec<SessionInfo>> {
    let mut sessions = Vec::new();
    let mut entries = tokio::fs::read_dir(guardian_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("sock") {
            continue;
        }
        let request = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: rand::random::<u64>(),
            method: "session.list".to_string(),
            params: serde_json::json!({}),
        };
        let Ok((response, _)) = relay(&path, request).await else { continue };
        let Some(result) = response.result else { continue };
        if let Ok(mut guardian_sessions) = serde_json::from_value::<Vec<SessionInfo>>(result) {
            sessions.append(&mut guardian_sessions);
        }
    }
    sessions.sort_by(|left, right| left.thread_id.cmp(&right.thread_id));
    Ok(sessions)
}

fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|error| WinxError::DeserializationError(error.to_string()))
}

fn guardian_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("WINX_GUARDIAN_BIN") {
        return Ok(PathBuf::from(path));
    }
    let executable = std::env::current_exe()?;
    let sibling = executable.with_file_name("winx-guardian");
    if sibling.is_file() {
        Ok(sibling)
    } else {
        Err(WinxError::ConfigurationError(format!(
            "winx-guardian not found beside {} (set WINX_GUARDIAN_BIN)",
            executable.display()
        )))
    }
}

fn rpc_result<T: serde::Serialize>(id: u64, result: &T) -> RpcResponse {
    match serde_json::to_value(result) {
        Ok(result) => {
            RpcResponse { jsonrpc: "2.0".to_string(), id, result: Some(result), error: None }
        }
        Err(error) => rpc_error(id, -32603, &error.to_string()),
    }
}

fn rpc_error(id: u64, code: i32, message: &str) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: None,
        error: Some(RpcError { code, message: message.to_string() }),
    }
}

fn same_uid(stream: &UnixStream) -> Result<bool> {
    let credentials = stream.peer_cred()?;
    Ok(credentials.uid() == crate::os::unix::effective_uid())
}
