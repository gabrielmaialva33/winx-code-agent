use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UnixStream;
use tokio::sync::Mutex;

use super::protocol::{
    read_json_frame, write_json_frame, CancelActionParams, ConfigureSessionParams,
    ConfigureSessionResult, ConfigureSessionTransition, HelloResult, JournalRead,
    JournalReadParams, PruneParams, PruneResult, RpcRequest, RpcResponse, RunActionParams,
    RunActionResult, SessionInfo, SessionParams, WireShellError,
    CANCELLABLE_ACTION_RESERVATIONS_CAPABILITY, COMPACT_ACTION_OUTPUT_CAPABILITY,
    GENERATION_BOUND_ACTIONS_CAPABILITY, PROTOCOL_MAJOR, TYPED_ACTION_RESULT_CAPABILITY,
};
use crate::errors::{Result, WinxError};
use crate::runtime::{
    BashCommandRuntimeResult, ShellActionOptions, ShellExecutionToken, ShellRuntime,
    ShellRuntimeBoolFuture, ShellRuntimeConfigureFuture, ShellRuntimeDetailedFuture,
    ShellRuntimeFuture, ShellRuntimeUnitFuture, ShellSessionConfiguration, ShellSessionTransition,
};
use crate::state::bash_state::BashState;
use crate::tools::bash_command::BashCommandResult;
use crate::types::BashCommand;

/// Shell runtime backed by a `winxd` Unix-domain socket.
#[derive(Clone, Debug)]
pub struct DaemonShellRuntime {
    socket_path: PathBuf,
    consumer_id: String,
    negotiations: Arc<Mutex<NegotiationCache>>,
}

#[derive(Debug, Default)]
struct NegotiationCache {
    sessions: HashMap<String, CachedNegotiation>,
    gates: HashMap<String, NegotiationGate>,
    local_revisions: HashMap<String, LocalRevision>,
}

#[derive(Debug)]
struct CachedNegotiation {
    session: Arc<Mutex<NegotiatedSession>>,
    last_seen: Instant,
}

#[derive(Debug)]
struct NegotiationGate {
    gate: Arc<Mutex<()>>,
    last_seen: Instant,
}

#[derive(Debug)]
struct LocalRevision {
    revision: Arc<AtomicU64>,
    last_seen: Instant,
}

const MAX_NEGOTIATED_SESSIONS: usize = 64;
const NEGOTIATION_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug)]
struct NegotiatedSession {
    control_epoch: String,
    guardian: HelloResult,
    stream: UnixStream,
}

/// Control-plane client used by the CLI and reconnection tests.
#[derive(Clone, Debug)]
pub struct DaemonClient {
    socket_path: PathBuf,
}

impl DaemonClient {
    pub fn new(socket_path: impl AsRef<Path>) -> Self {
        Self { socket_path: socket_path.as_ref().to_path_buf() }
    }

    async fn connected_with_hello(&self) -> Result<(UnixStream, HelloResult)> {
        let mut stream = self.connect_raw().await?;
        let hello: HelloResult = DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "winx.hello",
            serde_json::json!({ "protocol_major": PROTOCOL_MAJOR }),
        )
        .await?;
        if hello.protocol_major != PROTOCOL_MAJOR {
            return Err(WinxError::ConfigurationError(format!(
                "winxd protocol major {} is incompatible with client {}",
                hello.protocol_major, PROTOCOL_MAJOR
            )));
        }
        Ok((stream, hello))
    }

    async fn connect_raw(&self) -> Result<UnixStream> {
        UnixStream::connect(&self.socket_path).await.map_err(|error| {
            WinxError::ShellInitializationError(format!(
                "cannot connect to winxd at {}: {error}",
                self.socket_path.display()
            ))
        })
    }

    async fn connected(&self) -> Result<UnixStream> {
        self.connected_with_hello().await.map(|(stream, _)| stream)
    }

    pub async fn hello(&self) -> Result<HelloResult> {
        self.connected_with_hello().await.map(|(_, hello)| hello)
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let mut stream = self.connected().await?;
        DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "session.list",
            serde_json::json!({}),
        )
        .await
    }

    pub async fn session_info(&self, thread_id: &str) -> Result<SessionInfo> {
        let mut stream = self.connected().await?;
        DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "session.info",
            serde_json::to_value(SessionParams {
                thread_id: thread_id.to_string(),
                expected_generation: None,
                expected_execution: None,
                expected_guardian_epoch: None,
            })
            .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await
    }

    pub async fn read_output(&self, thread_id: &str, consumer_id: &str) -> Result<JournalRead> {
        let mut stream = self.connected().await?;
        DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "session.read_output",
            serde_json::to_value(JournalReadParams {
                thread_id: thread_id.to_string(),
                consumer_id: consumer_id.to_string(),
            })
            .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await
    }

    pub async fn kill_session(&self, thread_id: &str) -> Result<bool> {
        let mut stream = self.connected().await?;
        DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "session.kill",
            serde_json::to_value(SessionParams {
                thread_id: thread_id.to_string(),
                expected_generation: None,
                expected_execution: None,
                expected_guardian_epoch: None,
            })
            .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await
    }

    pub async fn prune_sessions(&self, idle_seconds: Option<u64>) -> Result<PruneResult> {
        let mut stream = self.connected().await?;
        DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "session.prune",
            serde_json::to_value(PruneParams { idle_seconds })
                .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await
    }

    pub async fn interrupt_session(&self, thread_id: &str) -> Result<()> {
        let _ = self.interrupt_session_generation(thread_id, None).await?;
        Ok(())
    }

    pub async fn interrupt_session_generation(
        &self,
        thread_id: &str,
        expected_generation: Option<u64>,
    ) -> Result<bool> {
        self.interrupt_session_execution(
            thread_id,
            expected_generation.map(|generation| ShellExecutionToken {
                guardian_epoch: String::new(),
                session_epoch: String::new(),
                generation,
            }),
        )
        .await
    }

    async fn interrupt_session_execution(
        &self,
        thread_id: &str,
        expected_execution: Option<ShellExecutionToken>,
    ) -> Result<bool> {
        // Cancellation deliberately uses a fresh connection. A long-running
        // status request on the negotiated action channel must never delay it.
        let (mut stream, _) = self.connected_with_hello().await?;
        let hello: HelloResult = DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "session.negotiate",
            serde_json::to_value(SessionParams {
                thread_id: thread_id.to_string(),
                expected_generation: None,
                expected_execution: None,
                expected_guardian_epoch: None,
            })
            .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await?;
        if expected_execution.is_some()
            && !hello
                .capabilities
                .iter()
                .any(|capability| capability == GENERATION_BOUND_ACTIONS_CAPABILITY)
        {
            return Ok(false);
        }
        if let Some(expected) = expected_execution.as_ref() {
            if !expected.guardian_epoch.is_empty() && expected.guardian_epoch != hello.daemon_epoch
            {
                return Ok(false);
            }
        }
        let expected_generation = expected_execution.as_ref().map(|token| token.generation);
        DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "session.interrupt",
            serde_json::to_value(SessionParams {
                thread_id: thread_id.to_string(),
                expected_generation,
                expected_execution: expected_execution.filter(|token| {
                    !token.guardian_epoch.is_empty() && !token.session_epoch.is_empty()
                }),
                expected_guardian_epoch: Some(hello.daemon_epoch),
            })
            .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await
    }

    async fn cancel_pending_action(&self, thread_id: &str, cancellation_key: &str) -> Result<bool> {
        // Like interruption, prelaunch cancellation must not share the action
        // channel that may itself be queued behind another foreground command.
        let (mut stream, _) = self.connected_with_hello().await?;
        let hello: HelloResult = DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "session.negotiate",
            serde_json::to_value(SessionParams {
                thread_id: thread_id.to_string(),
                expected_generation: None,
                expected_execution: None,
                expected_guardian_epoch: None,
            })
            .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await?;
        if !hello
            .capabilities
            .iter()
            .any(|capability| capability == CANCELLABLE_ACTION_RESERVATIONS_CAPABILITY)
        {
            return Ok(false);
        }
        DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "shell.cancel_action",
            serde_json::to_value(CancelActionParams {
                thread_id: thread_id.to_string(),
                cancellation_key: cancellation_key.to_string(),
                expected_guardian_epoch: Some(hello.daemon_epoch),
            })
            .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await
    }
}

impl DaemonShellRuntime {
    pub fn new(socket_path: impl AsRef<Path>) -> Self {
        Self::with_consumer_id(socket_path, format!("adapter-{:016x}", rand::random::<u64>()))
    }

    pub fn with_consumer_id(socket_path: impl AsRef<Path>, consumer_id: impl Into<String>) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
            consumer_id: consumer_id.into(),
            negotiations: Arc::new(Mutex::new(NegotiationCache::default())),
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    async fn negotiated_session(&self, thread_id: &str) -> Result<Arc<Mutex<NegotiatedSession>>> {
        {
            let now = Instant::now();
            let mut cache = self.negotiations.lock().await;
            prune_negotiations(&mut cache, now);
            if let Some(entry) = cache.sessions.get_mut(thread_id) {
                entry.last_seen = now;
                return Ok(Arc::clone(&entry.session));
            }
        }

        let negotiation_gate = self.negotiation_gate(thread_id).await?;
        let _single_flight = negotiation_gate.lock().await;
        {
            let now = Instant::now();
            let mut cache = self.negotiations.lock().await;
            prune_negotiations(&mut cache, now);
            if let Some(entry) = cache.sessions.get_mut(thread_id) {
                entry.last_seen = now;
                return Ok(Arc::clone(&entry.session));
            }
        }

        // Do not hold the global cache mutex across socket I/O. Per-session
        // channels serialize their own requests after publication.
        let client = DaemonClient::new(&self.socket_path);
        let (mut stream, control) = client.connected_with_hello().await?;
        let hello: HelloResult = Self::request(
            &mut stream,
            rand::random::<u64>(),
            "session.negotiate",
            serde_json::to_value(SessionParams {
                thread_id: thread_id.to_string(),
                expected_generation: None,
                expected_execution: None,
                expected_guardian_epoch: None,
            })
            .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await?;
        if hello.protocol_major != PROTOCOL_MAJOR {
            return Err(WinxError::ConfigurationError(format!(
                "guardian protocol major {} is incompatible with adapter {}",
                hello.protocol_major, PROTOCOL_MAJOR
            )));
        }
        let session = Arc::new(Mutex::new(NegotiatedSession {
            control_epoch: control.daemon_epoch,
            guardian: hello,
            stream,
        }));
        let now = Instant::now();
        let mut cache = self.negotiations.lock().await;
        prune_negotiations(&mut cache, now);
        if let Some(existing) = cache.sessions.get_mut(thread_id) {
            existing.last_seen = now;
            return Ok(Arc::clone(&existing.session));
        }
        cache_negotiation(&mut cache, thread_id, Arc::clone(&session), now)?;
        Ok(session)
    }

    async fn local_revision(&self, thread_id: &str) -> Result<Arc<AtomicU64>> {
        let now = Instant::now();
        let mut cache = self.negotiations.lock().await;
        prune_negotiations(&mut cache, now);
        if let Some(entry) = cache.local_revisions.get_mut(thread_id) {
            entry.last_seen = now;
            return Ok(Arc::clone(&entry.revision));
        }
        if cache.local_revisions.len() >= MAX_NEGOTIATED_SESSIONS {
            let oldest = cache
                .local_revisions
                .iter()
                .filter(|(_, entry)| Arc::strong_count(&entry.revision) == 1)
                .min_by_key(|(_, entry)| entry.last_seen)
                .map(|(thread_id, _)| thread_id.clone());
            if let Some(oldest) = oldest {
                cache.local_revisions.remove(&oldest);
            }
        }
        if cache.local_revisions.len() >= MAX_NEGOTIATED_SESSIONS {
            return Err(WinxError::ResourceAllocationError {
                message: format!(
                    "all {MAX_NEGOTIATED_SESSIONS} daemon local session revisions are active"
                ),
            });
        }
        let revision = Arc::new(AtomicU64::new(0));
        cache.local_revisions.insert(
            thread_id.to_string(),
            LocalRevision { revision: Arc::clone(&revision), last_seen: now },
        );
        Ok(revision)
    }

    async fn local_revision_is_current(
        &self,
        thread_id: &str,
        expected: &Arc<AtomicU64>,
        value: u64,
    ) -> bool {
        let mut cache = self.negotiations.lock().await;
        let Some(current) = cache.local_revisions.get_mut(thread_id) else { return false };
        current.last_seen = Instant::now();
        Arc::ptr_eq(&current.revision, expected) && current.revision.load(Ordering::SeqCst) == value
    }

    async fn apply_snapshot_if_current(
        &self,
        bash_state: &Arc<Mutex<Option<BashState>>>,
        thread_id: &str,
        expected_revision: &Arc<AtomicU64>,
        revision_value: u64,
        snapshot: &crate::state::persistence::BashStateSnapshot,
    ) -> bool {
        let mut state = bash_state.lock().await;
        let Some(state) = state.as_mut() else { return false };
        if !self.local_revision_is_current(thread_id, expected_revision, revision_value).await {
            return false;
        }
        state.apply_snapshot(snapshot);
        true
    }

    async fn negotiation_gate(&self, thread_id: &str) -> Result<Arc<Mutex<()>>> {
        let now = Instant::now();
        let mut cache = self.negotiations.lock().await;
        prune_negotiations(&mut cache, now);
        if let Some(entry) = cache.gates.get_mut(thread_id) {
            entry.last_seen = now;
            return Ok(Arc::clone(&entry.gate));
        }
        if cache.gates.len() >= MAX_NEGOTIATED_SESSIONS {
            let oldest = cache
                .gates
                .iter()
                .filter(|(_, entry)| Arc::strong_count(&entry.gate) == 1)
                .min_by_key(|(_, entry)| entry.last_seen)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                cache.gates.remove(&oldest);
            }
        }
        if cache.gates.len() >= MAX_NEGOTIATED_SESSIONS {
            return Err(WinxError::ResourceAllocationError {
                message: format!(
                    "all {MAX_NEGOTIATED_SESSIONS} daemon negotiation slots are active"
                ),
            });
        }
        let gate = Arc::new(Mutex::new(()));
        cache.gates.insert(
            thread_id.to_string(),
            NegotiationGate { gate: Arc::clone(&gate), last_seen: now },
        );
        Ok(gate)
    }

    async fn live_negotiated_session(
        &self,
        thread_id: &str,
    ) -> Result<Arc<Mutex<NegotiatedSession>>> {
        loop {
            let session = self.negotiated_session(thread_id).await?;
            let (alive, control_epoch) = {
                let session_guard = session.lock().await;
                let mut probe = [0_u8; 1];
                match session_guard.stream.try_read(&mut probe) {
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        (true, session_guard.control_epoch.clone())
                    }
                    Ok(_) | Err(_) => (false, session_guard.control_epoch.clone()),
                }
            };
            if alive {
                return Ok(session);
            }
            tracing::debug!(%control_epoch, %thread_id, "invalidating closed winxd negotiation");
            self.invalidate_negotiation(thread_id, &session).await;
        }
    }

    async fn negotiated_guardian(&self, thread_id: &str) -> Result<HelloResult> {
        let session = self.live_negotiated_session(thread_id).await?;
        let hello = session.lock().await.guardian.clone();
        Ok(hello)
    }

    async fn invalidate_negotiation(
        &self,
        thread_id: &str,
        expected: &Arc<Mutex<NegotiatedSession>>,
    ) {
        let mut cache = self.negotiations.lock().await;
        if cache
            .sessions
            .get(thread_id)
            .is_some_and(|current| Arc::ptr_eq(&current.session, expected))
        {
            cache.sessions.remove(thread_id);
        }
    }

    async fn negotiated_request<T: serde::de::DeserializeOwned>(
        session: &Arc<Mutex<NegotiatedSession>>,
        id: u64,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T> {
        let mut session = session.lock().await;
        Self::request(&mut session.stream, id, method, params).await
    }

    async fn negotiated_action_request<T: serde::de::DeserializeOwned>(
        session: &Arc<Mutex<NegotiatedSession>>,
        id: u64,
        params: serde_json::Value,
        options: &ShellActionOptions,
    ) -> Result<T> {
        let mut session = session.lock().await;
        if options.is_launch_cancelled() {
            return Err(WinxError::CommandExecutionError(
                "task was cancelled before the shell action launch gate".to_string(),
            ));
        }
        Self::request(&mut session.stream, id, "shell.run_action", params).await
    }

    async fn request<T: serde::de::DeserializeOwned>(
        stream: &mut UnixStream,
        id: u64,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T> {
        write_json_frame(
            stream,
            &RpcRequest { jsonrpc: "2.0".to_string(), id, method: method.to_string(), params },
        )
        .await?;
        let response: RpcResponse = read_json_frame(stream).await?;
        if response.id != id || response.jsonrpc != "2.0" {
            return Err(WinxError::ParseError("mismatched JSON-RPC response".to_string()));
        }
        if let Some(error) = response.error {
            return Err(WinxError::CommandExecutionError(format!(
                "winxd JSON-RPC error {}: {}",
                error.code, error.message
            )));
        }
        serde_json::from_value(
            response.result.ok_or_else(|| {
                WinxError::ParseError("JSON-RPC response omitted result".to_string())
            })?,
        )
        .map_err(|error| WinxError::DeserializationError(error.to_string()))
    }

    async fn run_remote_detailed(
        &self,
        bash_state: &std::sync::Arc<tokio::sync::Mutex<Option<BashState>>>,
        command: BashCommand,
        requested_options: ShellActionOptions,
    ) -> Result<BashCommandRuntimeResult> {
        let thread_id = command.thread_id.clone();
        // Capture state and its adapter-local configuration revision under the
        // BashState lock. configure_remote uses the same lock order, so a
        // concurrent Initialize/Reset/Mode/Workspace transition cannot land
        // between these two values.
        let (snapshot, local_revision, captured_revision) = {
            let state = bash_state.lock().await;
            let state = state.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
            let revision = self.local_revision(&thread_id).await?;
            let value = revision.load(Ordering::SeqCst);
            (state.snapshot(), revision, value)
        };
        let mut session = self.live_negotiated_session(&thread_id).await?;
        let hello = session.lock().await.guardian.clone();
        let (options, mut generation_bound_actions) =
            action_options_for_guardian(&hello, requested_options.clone())?;
        let request_key = format!("{:016x}", rand::random::<u64>());
        let id = rand::random::<u64>();
        let encode_params = |options| {
            serde_json::to_value(RunActionParams {
                snapshot: snapshot.clone(),
                command: command.clone(),
                request_key: request_key.clone(),
                consumer_id: self.consumer_id.clone(),
                options,
            })
            .map_err(|error| WinxError::SerializationError(error.to_string()))
        };
        let params = encode_params(options.clone())?;
        let result: RunActionResult = match Self::negotiated_action_request(
            &session, id, params, &options,
        )
        .await
        {
            Ok(result) => result,
            Err(first_error) => {
                // A closed negotiated channel means the control epoch changed.
                // Re-negotiate before retrying, then enforce the new guardian's
                // capabilities. The stable request key makes a lost response
                // safe without executing the command twice.
                self.invalidate_negotiation(&thread_id, &session).await;
                session = self.live_negotiated_session(&thread_id).await?;
                let retry_hello = session.lock().await.guardian.clone();
                if retry_hello.daemon_epoch != hello.daemon_epoch {
                    return Err(WinxError::CommandExecutionError(format!(
                        "{first_error}; guardian epoch changed from {} to {}, so Winx will not retry an ambiguously delivered shell action",
                        hello.daemon_epoch, retry_hello.daemon_epoch
                    )));
                }
                let (retry_options, retry_generation_bound) =
                    action_options_for_guardian(&retry_hello, requested_options.clone())?;
                generation_bound_actions = retry_generation_bound;
                Self::negotiated_action_request(
                    &session,
                    id,
                    encode_params(retry_options)?,
                    &requested_options,
                )
                .await
                .map_err(|retry_error| {
                    WinxError::CommandExecutionError(format!(
                        "{first_error}; retry after guardian negotiation failed: {retry_error}"
                    ))
                })?
            }
        };

        self.apply_snapshot_if_current(
            bash_state,
            &thread_id,
            &local_revision,
            captured_revision,
            &result.snapshot,
        )
        .await;
        runtime_result_from_wire(result, generation_bound_actions)
    }

    async fn configure_remote(
        &self,
        bash_state: &mut BashState,
        transition: ShellSessionTransition,
    ) -> Result<ShellSessionConfiguration> {
        let transition = match transition {
            ShellSessionTransition::FirstCall => ConfigureSessionTransition::FirstCall,
            ShellSessionTransition::ModeChange => ConfigureSessionTransition::ModeChange,
            ShellSessionTransition::Reset => ConfigureSessionTransition::Reset,
            ShellSessionTransition::WorkspaceChange => ConfigureSessionTransition::WorkspaceChange,
        };
        let thread_id = bash_state.current_thread_id.clone();
        // The caller owns BashState for this entire future. Bump before any
        // remote I/O so every older action response is stale even when this
        // transition ultimately returns an error.
        self.local_revision(&thread_id).await?.fetch_add(1, Ordering::SeqCst);
        let session = self.live_negotiated_session(&thread_id).await?;
        let result: ConfigureSessionResult = Self::negotiated_request(
            &session,
            rand::random::<u64>(),
            "session.configure",
            serde_json::to_value(ConfigureSessionParams {
                snapshot: bash_state.snapshot(),
                transition,
            })
            .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await?;
        if let Some(snapshot) = result.snapshot {
            bash_state.apply_snapshot(&snapshot);
        }
        Ok(ShellSessionConfiguration {
            attach_hint: result.attach_hint,
            attached_existing: result.attached_existing,
        })
    }
}

fn prune_negotiations(cache: &mut NegotiationCache, now: Instant) {
    cache.sessions.retain(|_, entry| {
        now.duration_since(entry.last_seen) <= NEGOTIATION_TTL
            || Arc::strong_count(&entry.session) > 1
    });
    cache.gates.retain(|_, entry| {
        now.duration_since(entry.last_seen) <= NEGOTIATION_TTL || Arc::strong_count(&entry.gate) > 1
    });
    cache.local_revisions.retain(|_, entry| {
        now.duration_since(entry.last_seen) <= NEGOTIATION_TTL
            || Arc::strong_count(&entry.revision) > 1
    });
}

fn cache_negotiation(
    cache: &mut NegotiationCache,
    thread_id: &str,
    session: Arc<Mutex<NegotiatedSession>>,
    now: Instant,
) -> Result<()> {
    if cache.sessions.len() >= MAX_NEGOTIATED_SESSIONS {
        if let Some(oldest) = cache
            .sessions
            .iter()
            .filter(|(_, entry)| Arc::strong_count(&entry.session) == 1)
            .min_by_key(|(_, entry)| entry.last_seen)
            .map(|(thread_id, _)| thread_id.clone())
        {
            cache.sessions.remove(&oldest);
        }
    }
    if cache.sessions.len() >= MAX_NEGOTIATED_SESSIONS {
        return Err(WinxError::ResourceAllocationError {
            message: format!("all {MAX_NEGOTIATED_SESSIONS} daemon sessions are active"),
        });
    }
    cache.sessions.insert(thread_id.to_string(), CachedNegotiation { session, last_seen: now });
    Ok(())
}

impl ShellRuntime for DaemonShellRuntime {
    fn configure_session<'a>(
        &'a self,
        bash_state: &'a mut BashState,
        transition: ShellSessionTransition,
    ) -> ShellRuntimeConfigureFuture<'a> {
        Box::pin(self.configure_remote(bash_state, transition))
    }

    fn run_action<'a>(
        &'a self,
        bash_state: &'a std::sync::Arc<tokio::sync::Mutex<Option<BashState>>>,
        command: BashCommand,
    ) -> ShellRuntimeFuture<'a> {
        Box::pin(async move {
            Ok(self
                .run_remote_detailed(bash_state, command, ShellActionOptions::default())
                .await?
                .result)
        })
    }

    fn run_action_detailed<'a>(
        &'a self,
        bash_state: &'a std::sync::Arc<tokio::sync::Mutex<Option<BashState>>>,
        command: BashCommand,
        options: ShellActionOptions,
    ) -> ShellRuntimeDetailedFuture<'a> {
        Box::pin(self.run_remote_detailed(bash_state, command, options))
    }

    fn supports_generation_bound_actions(&self) -> ShellRuntimeBoolFuture<'_> {
        // The stable control daemon cannot answer this without a session. Keep
        // the legacy context-free probe conservative.
        Box::pin(async { Ok(false) })
    }

    fn supports_generation_bound_actions_for<'a>(
        &'a self,
        bash_state: &'a std::sync::Arc<tokio::sync::Mutex<Option<BashState>>>,
    ) -> ShellRuntimeBoolFuture<'a> {
        Box::pin(async move {
            let thread_id = bash_state
                .lock()
                .await
                .as_ref()
                .ok_or(WinxError::BashStateNotInitialized)?
                .current_thread_id
                .clone();
            let hello = self.negotiated_guardian(&thread_id).await?;
            Ok([GENERATION_BOUND_ACTIONS_CAPABILITY, CANCELLABLE_ACTION_RESERVATIONS_CAPABILITY]
                .iter()
                .all(|required| hello.capabilities.iter().any(|capability| capability == required)))
        })
    }

    fn interrupt<'a>(
        &'a self,
        bash_state: &'a std::sync::Arc<tokio::sync::Mutex<Option<BashState>>>,
    ) -> ShellRuntimeUnitFuture<'a> {
        Box::pin(async move {
            let thread_id = bash_state
                .lock()
                .await
                .as_ref()
                .ok_or(WinxError::BashStateNotInitialized)?
                .current_thread_id
                .clone();
            DaemonClient::new(&self.socket_path).interrupt_session(&thread_id).await
        })
    }

    fn interrupt_generation<'a>(
        &'a self,
        bash_state: &'a std::sync::Arc<tokio::sync::Mutex<Option<BashState>>>,
        expected_generation: Option<u64>,
    ) -> ShellRuntimeBoolFuture<'a> {
        Box::pin(async move {
            let thread_id = bash_state
                .lock()
                .await
                .as_ref()
                .ok_or(WinxError::BashStateNotInitialized)?
                .current_thread_id
                .clone();
            DaemonClient::new(&self.socket_path)
                .interrupt_session_generation(&thread_id, expected_generation)
                .await
        })
    }

    fn interrupt_execution<'a>(
        &'a self,
        bash_state: &'a std::sync::Arc<tokio::sync::Mutex<Option<BashState>>>,
        expected: Option<ShellExecutionToken>,
    ) -> ShellRuntimeBoolFuture<'a> {
        Box::pin(async move {
            let thread_id = bash_state
                .lock()
                .await
                .as_ref()
                .ok_or(WinxError::BashStateNotInitialized)?
                .current_thread_id
                .clone();
            DaemonClient::new(&self.socket_path)
                .interrupt_session_execution(&thread_id, expected)
                .await
        })
    }

    fn cancel_pending_action<'a>(
        &'a self,
        bash_state: &'a std::sync::Arc<tokio::sync::Mutex<Option<BashState>>>,
        cancellation_key: &'a str,
    ) -> ShellRuntimeBoolFuture<'a> {
        Box::pin(async move {
            let thread_id = bash_state
                .lock()
                .await
                .as_ref()
                .ok_or(WinxError::BashStateNotInitialized)?
                .current_thread_id
                .clone();
            DaemonClient::new(&self.socket_path)
                .cancel_pending_action(&thread_id, cancellation_key)
                .await
        })
    }

    fn terminate_session<'a>(&'a self, thread_id: &'a str) -> ShellRuntimeUnitFuture<'a> {
        Box::pin(async move {
            let _ = DaemonClient::new(&self.socket_path).kill_session(thread_id).await?;
            self.negotiations.lock().await.sessions.remove(thread_id);
            Ok(())
        })
    }
}

fn action_options_for_guardian(
    hello: &HelloResult,
    mut options: ShellActionOptions,
) -> Result<(ShellActionOptions, bool)> {
    let has = |capability: &str| hello.capabilities.iter().any(|candidate| candidate == capability);
    if !has(TYPED_ACTION_RESULT_CAPABILITY) {
        return Err(WinxError::ConfigurationError(format!(
            "the running guardian does not advertise {TYPED_ACTION_RESULT_CAPABILITY}; terminate this durable session and initialize it again before executing BashCommand"
        )));
    }
    let generation_bound_actions = has(GENERATION_BOUND_ACTIONS_CAPABILITY);
    if (options.expected_generation.is_some() || options.require_generation_binding)
        && !generation_bound_actions
    {
        return Err(WinxError::InvalidInput(
            "the running guardian does not support generation-bound shell actions".to_string(),
        ));
    }
    if let Some(expected) = options.expected_execution.as_ref() {
        if expected.guardian_epoch != hello.daemon_epoch {
            return Err(WinxError::InvalidInput(
                "execution token belongs to a different guardian epoch".to_string(),
            ));
        }
        options.expected_generation = Some(expected.generation);
    }
    if options.expected_generation.is_some() || options.require_generation_binding {
        options.expected_guardian_epoch = Some(hello.daemon_epoch.clone());
    }
    if options.cancellation_key.is_some() && !has(CANCELLABLE_ACTION_RESERVATIONS_CAPABILITY) {
        return Err(WinxError::InvalidInput(
            "the running guardian does not support cancellable action reservations".to_string(),
        ));
    }
    if !has(COMPACT_ACTION_OUTPUT_CAPABILITY) {
        options.compact_output = false;
    }
    Ok((options, generation_bound_actions))
}

fn runtime_result_from_wire(
    result: RunActionResult,
    generation_bound_actions: bool,
) -> Result<BashCommandRuntimeResult> {
    match (
        result.output,
        result.compact_output,
        result.state,
        result.error,
        result.dropped_output_file,
    ) {
        (output, compact_output, Some(state), None, dropped_output_file)
            if output.is_some() || compact_output.is_some() => {
            let (output, compact_output) = match (output, compact_output) {
                (Some(output), compact) => (output, compact),
                (None, Some(compact)) => (String::new(), Some(compact)),
                (None, None) => unreachable!(),
            };
            Ok(BashCommandRuntimeResult {
                result: BashCommandResult { output, state },
                compact_output,
                command_generation: result.command_generation,
                execution_token: result.execution_token,
                generation_bound_actions,
                dropped_output_file,
                output_truncated: result.output_truncated,
            })
        }
        (_, _, _, Some(error), _) => Err(from_wire_error(error)),
        (Some(_), _, None, None, _) => Err(WinxError::ConfigurationError(
            "the session guardian predates typed BashCommand results; terminate this durable session, then initialize it again so the current guardian binary is created. Winx will not reconstruct orchestration state from terminal text."
                .to_string(),
        )),
        _ => Err(WinxError::ParseError(
            "winxd action response had neither a typed result nor an error".to_string(),
        )),
    }
}

fn from_wire_error(error: WireShellError) -> WinxError {
    match error {
        WireShellError::BashStateNotInitialized => WinxError::BashStateNotInitialized,
        WireShellError::ShellInitialization(message) => {
            WinxError::ShellInitializationError(message)
        }
        WireShellError::CommandExecution(message) | WireShellError::Other(message) => {
            WinxError::CommandExecutionError(message)
        }
        WireShellError::NoActiveCommand(message) => WinxError::NoActiveCommand(message),
        WireShellError::BackgroundSessionNotFound(message) => {
            WinxError::BackgroundSessionNotFound(message)
        }
        WireShellError::EmptyInteractiveInput(action) => {
            WinxError::EmptyInteractiveInput { action }
        }
        WireShellError::InteractiveTargetNotRunning(message) => {
            WinxError::InteractiveTargetNotRunning(message)
        }
        WireShellError::CommandAlreadyRunning { current_command, duration_seconds } => {
            WinxError::CommandAlreadyRunning { current_command, duration_seconds }
        }
        WireShellError::CommandNotAllowed(message) => WinxError::CommandNotAllowed(message),
        WireShellError::ThreadIdMismatch(message) => WinxError::ThreadIdMismatch(message),
        WireShellError::InvalidInput(message) => WinxError::InvalidInput(message),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::daemon::protocol::{RpcResponse, TYPED_ACTION_RESULT_CAPABILITY};

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // full control/guardian wire exchange is intentional
    async fn generation_capability_comes_from_effective_guardian_and_is_cached() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let socket = temp.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind mock control");
        let connections = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&connections);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept adapter");
            observed.fetch_add(1, Ordering::SeqCst);
            let hello: RpcRequest = read_json_frame(&mut stream).await.expect("control hello");
            assert_eq!(hello.method, "winx.hello");
            write_json_frame(
                &mut stream,
                &RpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: hello.id,
                    result: Some(
                        serde_json::to_value(HelloResult {
                            protocol_major: PROTOCOL_MAJOR,
                            protocol_minor: 5,
                            capabilities: vec![GENERATION_BOUND_ACTIONS_CAPABILITY.to_string()],
                            max_frame_bytes: super::super::protocol::MAX_FRAME_BYTES,
                            daemon_epoch: "control-15".to_string(),
                            daemon_pid: 15,
                            process_role: None,
                            build: None,
                        })
                        .expect("control hello value"),
                    ),
                    error: None,
                },
            )
            .await
            .expect("write control hello");

            let negotiation: RpcRequest =
                read_json_frame(&mut stream).await.expect("session negotiation");
            assert_eq!(negotiation.method, "session.negotiate");
            write_json_frame(
                &mut stream,
                &RpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: negotiation.id,
                    result: Some(
                        serde_json::to_value(HelloResult {
                            protocol_major: PROTOCOL_MAJOR,
                            protocol_minor: 4,
                            capabilities: vec![TYPED_ACTION_RESULT_CAPABILITY.to_string()],
                            max_frame_bytes: super::super::protocol::MAX_FRAME_BYTES,
                            daemon_epoch: "guardian-14".to_string(),
                            daemon_pid: 14,
                            process_role: None,
                            build: None,
                        })
                        .expect("guardian hello value"),
                    ),
                    error: None,
                },
            )
            .await
            .expect("write guardian hello");

            let action: RpcRequest =
                read_json_frame(&mut stream).await.expect("hot action request");
            assert_eq!(action.method, "shell.run_action", "hot path must not send winx.hello");
            assert!(
                action.params.get("options").is_none(),
                "guardian 1.4 must receive default protocol-1.4 options"
            );
            let params: RunActionParams =
                serde_json::from_value(action.params).expect("run action params");
            let cwd = params.snapshot.cwd.clone();
            write_json_frame(
                &mut stream,
                &RpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: action.id,
                    result: Some(
                        serde_json::to_value(RunActionResult {
                            output: Some("guardian-14-output".to_string()),
                            compact_output: None,
                            command_generation: None,
                            execution_token: None,
                            dropped_output_file: None,
                            output_truncated: false,
                            state: Some(crate::tools::bash_command::BashCommandState {
                                process_status:
                                    crate::tools::bash_command::BashProcessStatus::Exited,
                                background_id: None,
                                running_for_seconds: None,
                                exit_code: Some(0),
                                cwd: cwd.into(),
                                turn_state: None,
                            }),
                            snapshot: params.snapshot,
                            error: None,
                        })
                        .expect("action result value"),
                    ),
                    error: None,
                },
            )
            .await
            .expect("write action result");
        });

        let runtime = DaemonShellRuntime::new(&socket);
        let mut state = BashState::new();
        state.current_thread_id = "legacy-guardian".to_string();
        let state = Arc::new(Mutex::new(Some(state)));
        let (first_probe, concurrent_probe) = tokio::join!(
            runtime.supports_generation_bound_actions_for(&state),
            runtime.supports_generation_bound_actions_for(&state)
        );
        assert!(!first_probe.expect("first capability probe"));
        assert!(!concurrent_probe.expect("concurrent capability probe"));
        assert!(!tokio::time::timeout(
            std::time::Duration::from_millis(100),
            runtime.supports_generation_bound_actions_for(&state),
        )
        .await
        .expect("cached probe must not reconnect")
        .expect("cached capability"));

        let outcome = runtime
            .run_remote_detailed(
                &state,
                BashCommand {
                    action_json: crate::types::BashCommandAction::Command {
                        command: "printf guardian-14-output".to_string(),
                        is_background: false,
                        allow_multi: false,
                    },
                    wait_for_seconds: Some(0.0),
                    thread_id: "legacy-guardian".to_string(),
                },
                ShellActionOptions { compact_output: true, ..ShellActionOptions::default() },
            )
            .await
            .expect("guardian 1.4 action");
        assert_eq!(outcome.result.output, "guardian-14-output");
        assert_eq!(outcome.compact_output, None);
        server.await.expect("mock server task");
        assert_eq!(connections.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // two complete negotiated epochs are intentional
    async fn ambiguous_action_is_not_retried_after_guardian_epoch_changes() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let socket = temp.path().join("control-restart.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind mock control");
        let action_count = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&action_count);
        let server = tokio::spawn(async move {
            for (control_epoch, guardian_epoch) in
                [("control-one", "guardian-one"), ("control-two", "guardian-two")]
            {
                let (mut stream, _) = listener.accept().await.expect("accept adapter");
                let control_request: RpcRequest =
                    read_json_frame(&mut stream).await.expect("control hello");
                assert_eq!(control_request.method, "winx.hello");
                write_json_frame(
                    &mut stream,
                    &RpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: control_request.id,
                        result: Some(
                            serde_json::to_value(HelloResult {
                                protocol_major: PROTOCOL_MAJOR,
                                protocol_minor: 5,
                                capabilities: vec![],
                                max_frame_bytes: super::super::protocol::MAX_FRAME_BYTES,
                                daemon_epoch: control_epoch.to_string(),
                                daemon_pid: 15,
                                process_role: None,
                                build: None,
                            })
                            .expect("control hello value"),
                        ),
                        error: None,
                    },
                )
                .await
                .expect("control hello response");
                let negotiation: RpcRequest =
                    read_json_frame(&mut stream).await.expect("session negotiation");
                write_json_frame(
                    &mut stream,
                    &RpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: negotiation.id,
                        result: Some(
                            serde_json::to_value(HelloResult {
                                protocol_major: PROTOCOL_MAJOR,
                                protocol_minor: 5,
                                capabilities: vec![
                                    TYPED_ACTION_RESULT_CAPABILITY.to_string(),
                                    GENERATION_BOUND_ACTIONS_CAPABILITY.to_string(),
                                ],
                                max_frame_bytes: super::super::protocol::MAX_FRAME_BYTES,
                                daemon_epoch: guardian_epoch.to_string(),
                                daemon_pid: 16,
                                process_role: None,
                                build: None,
                            })
                            .expect("guardian hello value"),
                        ),
                        error: None,
                    },
                )
                .await
                .expect("guardian hello response");

                if guardian_epoch == "guardian-one" {
                    let action: RpcRequest =
                        read_json_frame(&mut stream).await.expect("first action");
                    assert_eq!(action.method, "shell.run_action");
                    observed.fetch_add(1, Ordering::SeqCst);
                    // Dropping without a response makes delivery ambiguous.
                } else {
                    let repeated = tokio::time::timeout(
                        std::time::Duration::from_millis(100),
                        read_json_frame::<_, RpcRequest>(&mut stream),
                    )
                    .await;
                    assert!(repeated.is_err(), "action was resent to a different guardian epoch");
                }
            }
        });

        let runtime = DaemonShellRuntime::new(&socket);
        let mut state = BashState::new();
        state.current_thread_id = "ambiguous-action".to_string();
        let state = Arc::new(Mutex::new(Some(state)));
        let result = runtime
            .run_remote_detailed(
                &state,
                BashCommand {
                    action_json: crate::types::BashCommandAction::Command {
                        command: "printf must-run-once".to_string(),
                        is_background: false,
                        allow_multi: false,
                    },
                    wait_for_seconds: Some(0.0),
                    thread_id: "ambiguous-action".to_string(),
                },
                ShellActionOptions::default(),
            )
            .await;
        assert!(
            matches!(result, Err(WinxError::CommandExecutionError(ref message)) if message.contains("will not retry")),
            "{result:?}"
        );
        server.await.expect("mock server task");
        assert_eq!(action_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn adapter_negotiation_cache_is_bounded_under_session_churn() {
        let mut cache = NegotiationCache::default();
        for index in 0..(MAX_NEGOTIATED_SESSIONS + 50) {
            let (stream, peer) = UnixStream::pair().expect("unix stream pair");
            drop(peer);
            let session = Arc::new(Mutex::new(NegotiatedSession {
                control_epoch: "control".to_string(),
                guardian: HelloResult {
                    protocol_major: PROTOCOL_MAJOR,
                    protocol_minor: 5,
                    capabilities: vec![TYPED_ACTION_RESULT_CAPABILITY.to_string()],
                    max_frame_bytes: super::super::protocol::MAX_FRAME_BYTES,
                    daemon_epoch: format!("guardian-{index}"),
                    daemon_pid: u32::try_from(index).unwrap_or(u32::MAX),
                    process_role: None,
                    build: None,
                },
                stream,
            }));
            cache_negotiation(&mut cache, &format!("thread-{index}"), session, Instant::now())
                .expect("inactive cache entry should be evictable");
        }
        assert_eq!(cache.sessions.len(), MAX_NEGOTIATED_SESSIONS);
    }

    #[tokio::test]
    async fn adapter_negotiation_cache_never_evicts_active_sessions() {
        let mut cache = NegotiationCache::default();
        let mut active = Vec::new();
        for index in 0..MAX_NEGOTIATED_SESSIONS {
            let (stream, peer) = UnixStream::pair().expect("unix stream pair");
            drop(peer);
            let session = Arc::new(Mutex::new(NegotiatedSession {
                control_epoch: "control".to_string(),
                guardian: HelloResult {
                    protocol_major: PROTOCOL_MAJOR,
                    protocol_minor: 5,
                    capabilities: vec![TYPED_ACTION_RESULT_CAPABILITY.to_string()],
                    max_frame_bytes: super::super::protocol::MAX_FRAME_BYTES,
                    daemon_epoch: format!("active-{index}"),
                    daemon_pid: u32::try_from(index).unwrap_or(u32::MAX),
                    process_role: None,
                    build: None,
                },
                stream,
            }));
            cache_negotiation(
                &mut cache,
                &format!("active-{index}"),
                Arc::clone(&session),
                Instant::now(),
            )
            .expect("active slot");
            active.push(session);
        }
        let (stream, peer) = UnixStream::pair().expect("overflow pair");
        drop(peer);
        let overflow = Arc::new(Mutex::new(NegotiatedSession {
            control_epoch: "control".to_string(),
            guardian: HelloResult {
                protocol_major: PROTOCOL_MAJOR,
                protocol_minor: 5,
                capabilities: vec![],
                max_frame_bytes: super::super::protocol::MAX_FRAME_BYTES,
                daemon_epoch: "overflow".to_string(),
                daemon_pid: 999,
                process_role: None,
                build: None,
            },
            stream,
        }));
        let error =
            cache_negotiation(&mut cache, "overflow", Arc::clone(&overflow), Instant::now())
                .expect_err("all active sessions must apply backpressure");
        assert!(matches!(error, WinxError::ResourceAllocationError { .. }));
        assert_eq!(cache.sessions.len(), MAX_NEGOTIATED_SESSIONS);

        drop(active.pop());
        cache_negotiation(&mut cache, "overflow", overflow, Instant::now())
            .expect("an inactive session can be evicted");
        assert_eq!(cache.sessions.len(), MAX_NEGOTIATED_SESSIONS);
    }

    #[tokio::test]
    async fn stale_daemon_action_snapshot_cannot_overwrite_local_reconfiguration() {
        let runtime = DaemonShellRuntime::new("unused.sock");
        let mut initial = BashState::new();
        initial.current_thread_id = "local-revision".to_string();
        initial.cwd = PathBuf::from("/old-action-cwd");
        let stale_snapshot = initial.snapshot();
        let state = Arc::new(Mutex::new(Some(initial)));

        let revision = runtime.local_revision("local-revision").await.expect("revision");
        let captured = revision.load(Ordering::SeqCst);
        let mut configure_guard = state.lock().await;
        let apply_runtime = runtime.clone();
        let apply_state = Arc::clone(&state);
        let apply_revision = Arc::clone(&revision);
        let stale_apply = tokio::spawn(async move {
            apply_runtime
                .apply_snapshot_if_current(
                    &apply_state,
                    "local-revision",
                    &apply_revision,
                    captured,
                    &stale_snapshot,
                )
                .await
        });
        tokio::task::yield_now().await;

        // configure_remote performs this bump while its caller holds the same
        // BashState lock. The stale action writeback is already concurrent and
        // waiting for that lock, so it must revalidate after configuration.
        revision.fetch_add(1, Ordering::SeqCst);
        configure_guard.as_mut().expect("state").cwd = PathBuf::from("/new-config-cwd");
        drop(configure_guard);

        assert!(!stale_apply.await.expect("stale writeback task"));
        assert_eq!(
            state.lock().await.as_ref().expect("state").cwd,
            PathBuf::from("/new-config-cwd")
        );
    }
}
