use std::collections::HashMap;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use super::lifecycle::{GuardianLifecycle, GuardianLimits};
use super::protocol::{
    read_json_frame, write_json_frame, CancelActionParams, ConfigureSessionParams, HelloResult,
    JournalReadParams, PruneParams, RpcError, RpcRequest, RpcResponse, RunActionParams,
    SessionInfo, SessionParams, CANCELLABLE_ACTION_RESERVATIONS_CAPABILITY,
    COMPACT_ACTION_OUTPUT_CAPABILITY, GENERATION_BOUND_ACTIONS_CAPABILITY, MAX_FRAME_BYTES,
    PROTOCOL_MAJOR, PROTOCOL_MINOR, TYPED_ACTION_RESULT_CAPABILITY,
};
use crate::errors::{Result, WinxError};
use crate::types::normalize_thread_id;

/// Stable control plane. Each logical session is owned by a separate guardian
/// process, so restarting this process does not close any PTY master.
pub struct ControlServer {
    listener: UnixListener,
    socket_path: PathBuf,
    lifecycle: Arc<GuardianLifecycle>,
    guardian_capabilities: Arc<Mutex<HashMap<PathBuf, CachedGuardian>>>,
    guardian_negotiation_gates: Arc<Mutex<HashMap<PathBuf, GuardianNegotiationGate>>>,
    epoch: String,
}

#[derive(Clone)]
struct CachedGuardian {
    hello: HelloResult,
    last_seen: Instant,
    socket_identity: Option<SocketIdentity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

struct GuardianNegotiationGate {
    gate: Arc<Mutex<()>>,
    last_seen: Instant,
}

const MAX_GUARDIAN_NEGOTIATIONS: usize = 256;
const GUARDIAN_NEGOTIATION_TTL: Duration = Duration::from_secs(5 * 60);

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
            guardian_capabilities: Arc::new(Mutex::new(HashMap::new())),
            guardian_negotiation_gates: Arc::new(Mutex::new(HashMap::new())),
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
            let guardian_capabilities = self.guardian_capabilities.clone();
            let guardian_negotiation_gates = self.guardian_negotiation_gates.clone();
            let epoch = self.epoch.clone();
            tokio::spawn(async move {
                if let Err(error) = serve_connection(
                    stream,
                    lifecycle,
                    guardian_capabilities,
                    guardian_negotiation_gates,
                    epoch,
                )
                .await
                {
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
    guardian_capabilities: Arc<Mutex<HashMap<PathBuf, CachedGuardian>>>,
    guardian_negotiation_gates: Arc<Mutex<HashMap<PathBuf, GuardianNegotiationGate>>>,
    epoch: String,
) -> Result<()> {
    loop {
        let request: RpcRequest = match read_json_frame(&mut stream).await {
            Ok(request) => request,
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let response = dispatch(
            request,
            &lifecycle,
            &guardian_capabilities,
            &guardian_negotiation_gates,
            &epoch,
        )
        .await;
        write_json_frame(&mut stream, &response).await?;
    }
}

#[allow(clippy::too_many_lines)]
async fn dispatch(
    mut request: RpcRequest,
    lifecycle: &GuardianLifecycle,
    guardian_capabilities: &Mutex<HashMap<PathBuf, CachedGuardian>>,
    guardian_negotiation_gates: &Mutex<HashMap<PathBuf, GuardianNegotiationGate>>,
    epoch: &str,
) -> RpcResponse {
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
                    "session.negotiate".to_string(),
                    "shell.run_action".to_string(),
                    TYPED_ACTION_RESULT_CAPABILITY.to_string(),
                    COMPACT_ACTION_OUTPUT_CAPABILITY.to_string(),
                    GENERATION_BOUND_ACTIONS_CAPABILITY.to_string(),
                    CANCELLABLE_ACTION_RESERVATIONS_CAPABILITY.to_string(),
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
            Ok(result) => {
                let mut cache = guardian_capabilities.lock().await;
                for thread_id in &result.removed_thread_ids {
                    cache.remove(&lifecycle.socket_for(thread_id));
                }
                rpc_result(request.id, &result)
            }
            Err(error) => rpc_error(request.id, -32003, &error.to_string()),
        };
    }

    let thread_id = match request_thread_id(&request) {
        Ok(thread_id) if !thread_id.is_empty() => thread_id,
        Ok(_) => return rpc_error(request.id, -32602, "explicit thread_id is required"),
        Err(error) => return rpc_error(request.id, -32602, &error.to_string()),
    };
    let guardian_socket = lifecycle.socket_for(&thread_id);
    if request.method == "session.negotiate" {
        return match negotiated_guardian(
            lifecycle,
            guardian_capabilities,
            guardian_negotiation_gates,
            &thread_id,
            &guardian_socket,
            true,
        )
        .await
        {
            Ok(hello) => rpc_result(request.id, &hello),
            Err(error) => rpc_error(request.id, -32001, &error.to_string()),
        };
    }
    if request.method == "session.kill" {
        match tokio::fs::try_exists(&guardian_socket).await {
            Ok(true) => {}
            Ok(false) => return rpc_result(request.id, &false),
            Err(error) => return rpc_error(request.id, -32002, &error.to_string()),
        }
    }

    let create_if_missing =
        matches!(request.method.as_str(), "session.configure" | "shell.run_action");
    let mut hello = match negotiated_guardian(
        lifecycle,
        guardian_capabilities,
        guardian_negotiation_gates,
        &thread_id,
        &guardian_socket,
        create_if_missing,
    )
    .await
    {
        Ok(hello) => hello,
        Err(error) => return rpc_error(request.id, -32001, &error.to_string()),
    };
    let live_precondition = match request_requires_live_guardian(&request) {
        Ok(required) => required,
        Err(error) => return rpc_error(request.id, -32602, &error.to_string()),
    };
    if !live_precondition {
        if let Err(error) = normalize_guardian_request(&mut request, &hello) {
            return rpc_error(request.id, -32602, &error.to_string());
        }
    }
    let kill_after = request.method == "session.kill";
    let request_id = request.id;
    let response = if live_precondition {
        match relay(&guardian_socket, request.clone()).await {
            Ok((response, effective)) => {
                hello = effective.clone();
                cache_guardian(guardian_capabilities, guardian_socket.clone(), effective).await;
                response
            }
            Err(error) => {
                guardian_capabilities.lock().await.remove(&guardian_socket);
                return rpc_error(request_id, -32602, &error.to_string());
            }
        }
    } else {
        match relay_negotiated(&guardian_socket, request.clone(), &hello).await {
            Ok(result) => result,
            Err(first_error) => {
                guardian_capabilities.lock().await.remove(&guardian_socket);
                let refreshed = match negotiated_guardian(
                    lifecycle,
                    guardian_capabilities,
                    guardian_negotiation_gates,
                    &thread_id,
                    &guardian_socket,
                    create_if_missing,
                )
                .await
                {
                    Ok(hello) => hello,
                    Err(error) => {
                        return rpc_error(
                            request_id,
                            -32002,
                            &format!("{first_error}; guardian renegotiation failed: {error}"),
                        )
                    }
                };
                if request.method == "shell.run_action"
                    && refreshed.daemon_epoch != hello.daemon_epoch
                {
                    return rpc_error(
                    request_id,
                    -32002,
                    &format!(
                        "{first_error}; guardian epoch changed, so winxd will not retry an ambiguously delivered shell action"
                    ),
                );
                }
                if let Err(error) = normalize_guardian_request(&mut request, &refreshed) {
                    return rpc_error(request_id, -32602, &error.to_string());
                }
                match relay_negotiated(&guardian_socket, request, &refreshed).await {
                    Ok(result) => {
                        hello = refreshed;
                        result
                    }
                    Err(error) => return rpc_error(request_id, -32002, &error.to_string()),
                }
            }
        }
    };

    if kill_after && response.error.is_none() {
        guardian_capabilities.lock().await.remove(&guardian_socket);
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
        "shell.cancel_action" => decode::<CancelActionParams>(request.params.clone())?.thread_id,
        "session.read_output" => decode::<JournalReadParams>(request.params.clone())?.thread_id,
        "session.negotiate" | "session.info" | "session.kill" | "session.interrupt" => {
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

async fn negotiated_guardian(
    lifecycle: &GuardianLifecycle,
    cache: &Mutex<HashMap<PathBuf, CachedGuardian>>,
    gates: &Mutex<HashMap<PathBuf, GuardianNegotiationGate>>,
    thread_id: &str,
    socket: &Path,
    create_if_missing: bool,
) -> Result<HelloResult> {
    if let Some(hello) = cached_guardian(cache, socket).await {
        return Ok(hello);
    }
    let negotiation_gate = guardian_negotiation_gate(gates, socket).await?;
    let _single_flight = negotiation_gate.lock().await;
    if let Some(hello) = cached_guardian(cache, socket).await {
        return Ok(hello);
    }
    if !create_if_missing && !tokio::fs::try_exists(socket).await? {
        return Err(WinxError::ShellInitializationError(format!(
            "guardian at {} is unavailable",
            socket.display()
        )));
    }
    let hello = lifecycle.ensure_guardian(thread_id, socket).await?;
    cache_guardian(cache, socket.to_path_buf(), hello.clone()).await;
    Ok(hello)
}

async fn cached_guardian(
    cache: &Mutex<HashMap<PathBuf, CachedGuardian>>,
    socket: &Path,
) -> Option<HelloResult> {
    let socket_identity = socket_identity(socket).await;
    let now = Instant::now();
    let mut cache = cache.lock().await;
    prune_guardian_cache(&mut cache, now);
    if let Some(entry) = cache
        .get_mut(socket)
        .filter(|entry| socket_identity.is_some() && entry.socket_identity == socket_identity)
    {
        entry.last_seen = now;
        return Some(entry.hello.clone());
    }
    cache.remove(socket);
    None
}

fn prune_guardian_cache(cache: &mut HashMap<PathBuf, CachedGuardian>, now: Instant) {
    cache.retain(|_, entry| now.duration_since(entry.last_seen) <= GUARDIAN_NEGOTIATION_TTL);
}

async fn guardian_negotiation_gate(
    gates: &Mutex<HashMap<PathBuf, GuardianNegotiationGate>>,
    socket: &Path,
) -> Result<Arc<Mutex<()>>> {
    let now = Instant::now();
    let mut gates = gates.lock().await;
    gates.retain(|_, entry| {
        now.duration_since(entry.last_seen) <= GUARDIAN_NEGOTIATION_TTL
            || Arc::strong_count(&entry.gate) > 1
    });
    if let Some(entry) = gates.get_mut(socket) {
        entry.last_seen = now;
        return Ok(Arc::clone(&entry.gate));
    }
    if gates.len() >= MAX_GUARDIAN_NEGOTIATIONS {
        let oldest = gates
            .iter()
            .filter(|(_, entry)| Arc::strong_count(&entry.gate) == 1)
            .min_by_key(|(_, entry)| entry.last_seen)
            .map(|(path, _)| path.clone());
        if let Some(oldest) = oldest {
            gates.remove(&oldest);
        }
    }
    if gates.len() >= MAX_GUARDIAN_NEGOTIATIONS {
        return Err(WinxError::ResourceAllocationError {
            message: format!(
                "all {MAX_GUARDIAN_NEGOTIATIONS} guardian negotiation slots are active"
            ),
        });
    }
    let gate = Arc::new(Mutex::new(()));
    gates.insert(
        socket.to_path_buf(),
        GuardianNegotiationGate { gate: Arc::clone(&gate), last_seen: now },
    );
    Ok(gate)
}

async fn cache_guardian(
    cache: &Mutex<HashMap<PathBuf, CachedGuardian>>,
    socket: PathBuf,
    hello: HelloResult,
) {
    // Read filesystem identity before taking the global cache mutex. Besides
    // avoiding I/O under the lock, this makes an unlinked/recreated guardian
    // socket invalidate the old epoch immediately instead of waiting for TTL.
    let socket_identity = socket_identity(&socket).await;
    let now = Instant::now();
    let mut cache = cache.lock().await;
    prune_guardian_cache(&mut cache, now);
    if !cache.contains_key(&socket) && cache.len() >= MAX_GUARDIAN_NEGOTIATIONS {
        if let Some(oldest) =
            cache.iter().min_by_key(|(_, entry)| entry.last_seen).map(|(path, _)| path.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(socket, CachedGuardian { hello, last_seen: now, socket_identity });
}

async fn socket_identity(socket: &Path) -> Option<SocketIdentity> {
    tokio::fs::metadata(socket)
        .await
        .ok()
        .map(|metadata| SocketIdentity { device: metadata.dev(), inode: metadata.ino() })
}

fn normalize_guardian_request(request: &mut RpcRequest, hello: &HelloResult) -> Result<()> {
    let has = |capability: &str| hello.capabilities.iter().any(|candidate| candidate == capability);
    if request.method == "shell.run_action" {
        let mut params = decode::<RunActionParams>(request.params.clone())?;
        if !has(TYPED_ACTION_RESULT_CAPABILITY) {
            return Err(WinxError::ConfigurationError(format!(
                "guardian pid {} does not advertise {TYPED_ACTION_RESULT_CAPABILITY}; terminate this durable session and initialize it again before running another BashCommand",
                hello.daemon_pid
            )));
        }
        if params
            .options
            .expected_guardian_epoch
            .as_deref()
            .is_some_and(|expected| expected != hello.daemon_epoch)
            || params
                .options
                .expected_execution
                .as_ref()
                .is_some_and(|expected| expected.guardian_epoch != hello.daemon_epoch)
        {
            return Err(WinxError::InvalidInput(
                "shell action precondition refers to a different effective guardian epoch"
                    .to_string(),
            ));
        }
        if (params.options.expected_generation.is_some()
            || params.options.expected_execution.is_some()
            || params.options.require_generation_binding)
            && !has(GENERATION_BOUND_ACTIONS_CAPABILITY)
        {
            return Err(WinxError::InvalidInput(
                "the running guardian does not support generation-bound shell actions".to_string(),
            ));
        }
        if params.options.cancellation_key.is_some()
            && !has(CANCELLABLE_ACTION_RESERVATIONS_CAPABILITY)
        {
            return Err(WinxError::InvalidInput(
                "the running guardian does not support cancellable action reservations".to_string(),
            ));
        }
        if !has(COMPACT_ACTION_OUTPUT_CAPABILITY) {
            params.options.compact_output = false;
        }
        request.params = serde_json::to_value(params)
            .map_err(|error| WinxError::SerializationError(error.to_string()))?;
    } else if request.method == "session.interrupt" {
        let params = decode::<SessionParams>(request.params.clone())?;
        if params
            .expected_guardian_epoch
            .as_deref()
            .is_some_and(|expected| expected != hello.daemon_epoch)
            || params
                .expected_execution
                .as_ref()
                .is_some_and(|expected| expected.guardian_epoch != hello.daemon_epoch)
        {
            return Err(WinxError::InvalidInput(
                "interrupt precondition refers to a different effective guardian epoch".to_string(),
            ));
        }
        if (params.expected_generation.is_some() || params.expected_execution.is_some())
            && !has(GENERATION_BOUND_ACTIONS_CAPABILITY)
        {
            return Err(WinxError::InvalidInput(
                "the running guardian does not support generation-bound interruption".to_string(),
            ));
        }
    } else if request.method == "shell.cancel_action" {
        let params = decode::<CancelActionParams>(request.params.clone())?;
        if !has(CANCELLABLE_ACTION_RESERVATIONS_CAPABILITY) {
            return Err(WinxError::InvalidInput(
                "the running guardian does not support cancellable action reservations".to_string(),
            ));
        }
        if params
            .expected_guardian_epoch
            .as_deref()
            .is_some_and(|expected| expected != hello.daemon_epoch)
        {
            return Err(WinxError::InvalidInput(
                "cancel precondition refers to a different effective guardian epoch".to_string(),
            ));
        }
    }
    Ok(())
}

fn request_requires_live_guardian(request: &RpcRequest) -> Result<bool> {
    match request.method.as_str() {
        "shell.run_action" => {
            let params = decode::<RunActionParams>(request.params.clone())?;
            Ok(params.options.require_generation_binding
                || params.options.expected_generation.is_some()
                || params.options.expected_execution.is_some()
                || params.options.expected_guardian_epoch.is_some()
                || params.options.cancellation_key.is_some())
        }
        "session.interrupt" => {
            let params = decode::<SessionParams>(request.params.clone())?;
            Ok(params.expected_generation.is_some()
                || params.expected_execution.is_some()
                || params.expected_guardian_epoch.is_some())
        }
        "shell.cancel_action" => Ok(true),
        _ => Ok(false),
    }
}

async fn relay(socket: &Path, mut request: RpcRequest) -> Result<(RpcResponse, HelloResult)> {
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
    if request.method == "shell.run_action"
        && !hello.capabilities.iter().any(|capability| capability == TYPED_ACTION_RESULT_CAPABILITY)
    {
        return Err(WinxError::ConfigurationError(format!(
            "guardian pid {} does not advertise {TYPED_ACTION_RESULT_CAPABILITY}; terminate this durable session and initialize it again before running another BashCommand",
            hello.daemon_pid
        )));
    }
    normalize_guardian_request(&mut request, &hello)?;
    write_json_frame(&mut stream, &request).await?;
    let response: RpcResponse = read_json_frame(&mut stream).await?;
    Ok((response, hello))
}

async fn relay_negotiated(
    socket: &Path,
    request: RpcRequest,
    hello: &HelloResult,
) -> Result<RpcResponse> {
    let mut stream = UnixStream::connect(socket).await.map_err(|error| {
        WinxError::ShellInitializationError(format!(
            "guardian at {} is unavailable: {error}",
            socket.display()
        ))
    })?;
    write_json_frame(&mut stream, &request).await?;
    let response: RpcResponse = read_json_frame(&mut stream).await?;
    if response.id != request.id || response.jsonrpc != "2.0" {
        return Err(WinxError::ParseError(format!(
            "guardian pid {} returned a mismatched JSON-RPC response",
            hello.daemon_pid
        )));
    }
    Ok(response)
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::runtime::ShellActionOptions;
    use crate::state::bash_state::BashState;
    use crate::types::BashCommand;

    fn guardian_1_4() -> HelloResult {
        HelloResult {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: 4,
            capabilities: vec![TYPED_ACTION_RESULT_CAPABILITY.to_string()],
            max_frame_bytes: MAX_FRAME_BYTES,
            daemon_epoch: "guardian-14".to_string(),
            daemon_pid: 14,
        }
    }

    #[test]
    fn guardian_1_4_normalization_preserves_adapter_wait_policy_semantics() {
        let command: BashCommand = serde_json::from_value(serde_json::json!({
            "command": "printf compatible",
            "wait_policy": "until_complete",
            "thread_id": "guardian14"
        }))
        .expect("adapter command");
        let mut request = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 14,
            method: "shell.run_action".to_string(),
            params: serde_json::to_value(RunActionParams {
                snapshot: BashState::new().snapshot(),
                command,
                request_key: "request14".to_string(),
                consumer_id: "consumer14".to_string(),
                options: ShellActionOptions {
                    compact_output: true,
                    ..ShellActionOptions::default()
                },
            })
            .expect("run params"),
        };

        normalize_guardian_request(&mut request, &guardian_1_4()).expect("normalization");
        let wire = request.params.as_object().expect("params object");
        assert!(wire.get("options").is_none());
        assert!(wire["command"].get("wait_policy").is_none());
    }

    #[test]
    fn guardian_1_4_rejects_generation_bound_action_before_relay() {
        let command: BashCommand = serde_json::from_value(serde_json::json!({
            "command": "printf compatible",
            "thread_id": "guardian14"
        }))
        .expect("adapter command");
        let mut request = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 15,
            method: "shell.run_action".to_string(),
            params: serde_json::to_value(RunActionParams {
                snapshot: BashState::new().snapshot(),
                command,
                request_key: "request15".to_string(),
                consumer_id: "consumer15".to_string(),
                options: ShellActionOptions {
                    expected_generation: Some(7),
                    ..ShellActionOptions::default()
                },
            })
            .expect("run params"),
        };
        assert!(matches!(
            normalize_guardian_request(&mut request, &guardian_1_4()),
            Err(WinxError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn stale_1_5_precondition_sends_zero_action_frames_to_1_4_guardian() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let socket = temp.path().join("guardian.sock");
        let listener = UnixListener::bind(&socket).expect("mock guardian");
        let guardian = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("control connection");
            let hello: RpcRequest = read_json_frame(&mut stream).await.expect("hello frame");
            assert_eq!(hello.method, "winx.hello");
            write_json_frame(&mut stream, &rpc_result(hello.id, &guardian_1_4()))
                .await
                .expect("hello response");
            tokio::time::timeout(
                Duration::from_millis(100),
                read_json_frame::<_, RpcRequest>(&mut stream),
            )
            .await
            .is_ok_and(|frame| frame.is_ok())
        });
        let command: BashCommand = serde_json::from_value(serde_json::json!({
            "command": "touch must-not-run",
            "thread_id": "stale-guardian"
        }))
        .expect("command");
        let request = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 16,
            method: "shell.run_action".to_string(),
            params: serde_json::to_value(RunActionParams {
                snapshot: BashState::new().snapshot(),
                command,
                request_key: "stale-request".to_string(),
                consumer_id: "stale-consumer".to_string(),
                options: ShellActionOptions {
                    require_generation_binding: true,
                    expected_guardian_epoch: Some("guardian-15".to_string()),
                    ..ShellActionOptions::default()
                },
            })
            .expect("params"),
        };

        assert!(relay(&socket, request).await.is_err());
        assert!(!guardian.await.expect("guardian task"));
    }

    #[tokio::test]
    async fn guardian_negotiation_cache_is_bounded_under_churn() {
        let cache = Mutex::new(HashMap::new());
        for index in 0..(MAX_GUARDIAN_NEGOTIATIONS + 50) {
            let mut hello = guardian_1_4();
            hello.daemon_epoch = format!("epoch-{index}");
            cache_guardian(&cache, PathBuf::from(format!("guardian-{index}.sock")), hello).await;
        }
        assert_eq!(cache.lock().await.len(), MAX_GUARDIAN_NEGOTIATIONS);
    }

    #[tokio::test]
    async fn concurrent_cold_guardian_negotiation_uses_one_gate() {
        let gates = Mutex::new(HashMap::new());
        let socket = PathBuf::from("same-guardian.sock");
        let (first, second) = tokio::join!(
            guardian_negotiation_gate(&gates, &socket),
            guardian_negotiation_gate(&gates, &socket)
        );
        assert!(Arc::ptr_eq(
            &first.expect("first negotiation gate"),
            &second.expect("concurrent negotiation gate")
        ));
        assert_eq!(gates.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn guardian_negotiation_gates_apply_backpressure_without_evicting_active_slots() {
        let gates = Mutex::new(HashMap::new());
        let mut active = Vec::with_capacity(MAX_GUARDIAN_NEGOTIATIONS);
        for index in 0..MAX_GUARDIAN_NEGOTIATIONS {
            active.push(
                guardian_negotiation_gate(
                    &gates,
                    &PathBuf::from(format!("active-guardian-{index}.sock")),
                )
                .await
                .expect("active gate"),
            );
        }

        let overflow =
            guardian_negotiation_gate(&gates, &PathBuf::from("overflow-guardian.sock")).await;
        assert!(matches!(overflow, Err(WinxError::ResourceAllocationError { .. })));
        assert_eq!(gates.lock().await.len(), MAX_GUARDIAN_NEGOTIATIONS);

        drop(active.pop());
        guardian_negotiation_gate(&gates, &PathBuf::from("replacement-guardian.sock"))
            .await
            .expect("inactive slot can be replaced");
        assert_eq!(gates.lock().await.len(), MAX_GUARDIAN_NEGOTIATIONS);
    }

    #[tokio::test]
    async fn recreated_guardian_socket_invalidates_negotiation_cache() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let socket = temp.path().join("guardian.sock");
        let first_listener = UnixListener::bind(&socket).expect("first guardian socket");
        let cache = Mutex::new(HashMap::new());
        let hello = guardian_1_4();
        cache_guardian(&cache, socket.clone(), hello.clone()).await;

        assert_eq!(
            cached_guardian(&cache, &socket).await.expect("cached first guardian").daemon_epoch,
            hello.daemon_epoch
        );

        drop(first_listener);
        tokio::fs::remove_file(&socket).await.expect("remove first socket");
        let _second_listener = UnixListener::bind(&socket).expect("recreated guardian socket");

        assert!(cached_guardian(&cache, &socket).await.is_none());
        assert!(cache.lock().await.is_empty());
    }
}
