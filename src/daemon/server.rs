use std::collections::{HashMap, HashSet, VecDeque};
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{watch, Mutex, Notify, RwLock};

use super::protocol::{
    read_json_frame, write_json_frame, CancelActionParams, ConfigureSessionParams,
    ConfigureSessionResult, ConfigureSessionTransition, HelloResult, JournalRead,
    JournalReadParams, RpcError, RpcRequest, RpcResponse, RunActionParams, RunActionResult,
    SessionInfo, SessionParams, WireShellError, CANCELLABLE_ACTION_RESERVATIONS_CAPABILITY,
    COMPACT_ACTION_OUTPUT_CAPABILITY, GENERATION_BOUND_ACTIONS_CAPABILITY, MAX_FRAME_BYTES,
    PROTOCOL_MAJOR, PROTOCOL_MINOR, TYPED_ACTION_RESULT_CAPABILITY,
};
use crate::errors::{Result, WinxError};
use crate::runtime::{lock_session_store, ShellExecutionToken, ShellTarget};
use crate::state::bash_state::BashState;
use crate::state::pty::SharedPtyShell;
use crate::tools::bash_command::ShellDeliveryCursor;
use crate::types::normalize_thread_id;

type SharedState = Arc<Mutex<Option<BashState>>>;

struct TimedEntry<T> {
    value: T,
    last_seen: Instant,
}

type CachedActionResult = std::result::Result<RunActionResult, String>;

#[derive(Clone)]
enum ActionCompletion {
    InFlight(Arc<watch::Sender<Option<CachedActionResult>>>),
    Complete(Box<CachedActionResult>),
}

struct ActionOwnerGuard {
    completion: Arc<watch::Sender<Option<CachedActionResult>>>,
    armed: bool,
}

impl ActionOwnerGuard {
    fn new(completion: Arc<watch::Sender<Option<CachedActionResult>>>) -> Self {
        Self { completion, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ActionOwnerGuard {
    fn drop(&mut self) {
        if self.armed {
            self.completion.send_replace(Some(Err(
                "single-flight shell action owner disappeared before completion".to_string(),
            )));
        }
    }
}

struct ActionEntry {
    fingerprint: Vec<u8>,
    completion: ActionCompletion,
    last_seen: Instant,
}

struct DaemonSession {
    state: SharedState,
    /// Reset/configure takes the writer; actions, snapshots, cursors, and
    /// interrupts take readers. This binds execution-token validation to the
    /// exact shell incarnation on which the operation is performed.
    operation_barrier: RwLock<()>,
    completed: Mutex<HashMap<String, ActionEntry>>,
    journal: Mutex<OutputJournal>,
    observed: Mutex<HashMap<String, (u64, String)>>,
    background_ids: Mutex<HashSet<String>>,
    command_id: Mutex<Option<String>>,
    delivery_cursors: Mutex<HashMap<String, TimedEntry<Arc<Mutex<ShellDeliveryCursor>>>>>,
    launch_reservations: Mutex<HashMap<String, TimedEntry<Arc<AtomicBool>>>>,
    created_at_unix_ms: u64,
    last_activity_unix_ms: AtomicU64,
    last_command_at_unix_ms: AtomicU64,
    ever_ran_command: AtomicBool,
    drainer_started: AtomicBool,
    activity: Notify,
    session_epoch: Mutex<String>,
}

impl DaemonSession {
    fn new() -> Self {
        let now = unix_ms();
        Self {
            state: Arc::new(Mutex::new(None)),
            operation_barrier: RwLock::new(()),
            completed: Mutex::new(HashMap::new()),
            journal: Mutex::new(OutputJournal::default()),
            observed: Mutex::new(HashMap::new()),
            background_ids: Mutex::new(HashSet::new()),
            command_id: Mutex::new(None),
            delivery_cursors: Mutex::new(HashMap::new()),
            launch_reservations: Mutex::new(HashMap::new()),
            created_at_unix_ms: now,
            last_activity_unix_ms: AtomicU64::new(now),
            last_command_at_unix_ms: AtomicU64::new(0),
            ever_ran_command: AtomicBool::new(false),
            drainer_started: AtomicBool::new(false),
            activity: Notify::new(),
            session_epoch: Mutex::new(new_epoch()),
        }
    }

    fn note_activity(&self, command: bool) {
        let now = unix_ms();
        self.last_activity_unix_ms.store(now, Ordering::Release);
        if command {
            self.last_command_at_unix_ms.store(now, Ordering::Release);
            self.ever_ran_command.store(true, Ordering::Release);
        }
        self.activity.notify_one();
    }
}

fn new_epoch() -> String {
    format!("{:016x}", rand::random::<u64>())
}

const MAX_JOURNAL_BYTES: usize = 4 * 1024 * 1024;
const MAX_SESSION_CACHE_ENTRIES: usize = 256;
const MAX_IDENTIFIER_BYTES: usize = 256;
const SESSION_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const ACTION_OWNER_TIMEOUT: Duration = Duration::from_secs(11 * 60);
const DRAIN_CHANGED_INTERVAL: Duration = Duration::from_millis(20);
const DRAIN_ACTIVE_INTERVAL: Duration = Duration::from_millis(100);
const DRAIN_IDLE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Default)]
struct CaptureState {
    changed: bool,
    active: bool,
}

impl CaptureState {
    fn merge(&mut self, other: Self) {
        self.changed |= other.changed;
        self.active |= other.active;
    }

    fn next_delay(self) -> Duration {
        if self.changed {
            DRAIN_CHANGED_INTERVAL
        } else if self.active {
            DRAIN_ACTIVE_INTERVAL
        } else {
            DRAIN_IDLE_INTERVAL
        }
    }
}

#[derive(Default)]
struct OutputJournal {
    chunks: VecDeque<(u64, String)>,
    cursors: HashMap<String, TimedEntry<u64>>,
    next_seq: u64,
    bytes: usize,
}

fn prune_timed_entries<T>(entries: &mut HashMap<String, TimedEntry<T>>, now: Instant) {
    entries.retain(|_, entry| now.duration_since(entry.last_seen) <= SESSION_CACHE_TTL);
}

fn evict_oldest_if_full<T>(entries: &mut HashMap<String, TimedEntry<T>>, incoming: &str) {
    if entries.contains_key(incoming) || entries.len() < MAX_SESSION_CACHE_ENTRIES {
        return;
    }
    if let Some(oldest) =
        entries.iter().min_by_key(|(_, entry)| entry.last_seen).map(|(key, _)| key.clone())
    {
        entries.remove(&oldest);
    }
}

async fn launch_cancellation_flag(
    session: &DaemonSession,
    cancellation_key: &str,
) -> Result<Arc<AtomicBool>> {
    let now = Instant::now();
    let mut reservations = session.launch_reservations.lock().await;
    reservations.retain(|_, entry| {
        now.duration_since(entry.last_seen) <= SESSION_CACHE_TTL
            || Arc::strong_count(&entry.value) > 1
    });
    if !reservations.contains_key(cancellation_key)
        && reservations.len() >= MAX_SESSION_CACHE_ENTRIES
    {
        let oldest = reservations
            .iter()
            .filter(|(_, entry)| Arc::strong_count(&entry.value) == 1)
            .min_by_key(|(_, entry)| entry.last_seen)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            reservations.remove(&oldest);
        }
    }
    if !reservations.contains_key(cancellation_key)
        && reservations.len() >= MAX_SESSION_CACHE_ENTRIES
    {
        return Err(WinxError::ResourceAllocationError {
            message: format!(
                "all {MAX_SESSION_CACHE_ENTRIES} action cancellation reservations are active"
            ),
        });
    }
    let entry = reservations
        .entry(cancellation_key.to_string())
        .or_insert_with(|| TimedEntry { value: Arc::new(AtomicBool::new(false)), last_seen: now });
    entry.last_seen = now;
    Ok(Arc::clone(&entry.value))
}

fn validate_identifier(label: &str, value: &str) -> Result<()> {
    if value.len() > MAX_IDENTIFIER_BYTES {
        return Err(WinxError::InvalidInput(format!(
            "{label} exceeds the {MAX_IDENTIFIER_BYTES}-byte limit"
        )));
    }
    Ok(())
}

impl OutputJournal {
    fn append(&mut self, output: String) {
        if output.is_empty() {
            return;
        }
        self.next_seq = self.next_seq.saturating_add(1);
        self.bytes = self.bytes.saturating_add(output.len());
        self.chunks.push_back((self.next_seq, output));
        while self.bytes > MAX_JOURNAL_BYTES && self.chunks.len() > 1 {
            if let Some((_, removed)) = self.chunks.pop_front() {
                self.bytes = self.bytes.saturating_sub(removed.len());
            }
        }
    }

    fn read(&mut self, consumer_id: &str) -> JournalRead {
        let now = Instant::now();
        prune_timed_entries(&mut self.cursors, now);
        evict_oldest_if_full(&mut self.cursors, consumer_id);

        let oldest = self.chunks.front().map_or(self.next_seq.saturating_add(1), |(seq, _)| *seq);
        let cursor = self.cursors.get(consumer_id).map_or(0, |entry| entry.value);
        let gap = cursor.saturating_add(1) < oldest;
        let mut output = String::new();
        let mut next_seq = cursor;
        for (seq, chunk) in self.chunks.iter().filter(|(seq, _)| *seq > cursor) {
            output.push_str(chunk);
            next_seq = *seq;
        }
        self.cursors
            .insert(consumer_id.to_string(), TimedEntry { value: next_seq, last_seen: now });
        JournalRead { output, next_seq, gap }
    }
}

/// Long-lived JSON-RPC shell owner listening on a Unix-domain socket.
pub struct DaemonServer {
    listener: UnixListener,
    socket_path: PathBuf,
    sessions: Arc<Mutex<HashMap<String, Arc<DaemonSession>>>>,
    epoch: String,
}

impl DaemonServer {
    pub async fn bind(socket_path: impl AsRef<Path>) -> Result<Self> {
        let socket_path = socket_path.as_ref().to_path_buf();
        let parent = socket_path.parent().ok_or_else(|| {
            WinxError::ConfigurationError("daemon socket must have a parent directory".to_string())
        })?;
        tokio::fs::create_dir_all(parent).await?;
        tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).await?;

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
        let epoch = format!("{:016x}", rand::random::<u64>());
        Ok(Self { listener, socket_path, sessions: Arc::new(Mutex::new(HashMap::new())), epoch })
    }

    pub async fn serve(self) -> Result<()> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            if !same_uid(&stream)? {
                tracing::warn!("Rejected winxd connection from a different uid");
                continue;
            }
            let sessions = self.sessions.clone();
            let epoch = self.epoch.clone();
            tokio::spawn(async move {
                if let Err(error) = serve_connection(stream, sessions, epoch).await {
                    tracing::debug!("winxd client disconnected: {error}");
                }
            });
        }
    }
}

impl Drop for DaemonServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

async fn serve_connection(
    mut stream: UnixStream,
    sessions: Arc<Mutex<HashMap<String, Arc<DaemonSession>>>>,
    epoch: String,
) -> Result<()> {
    loop {
        let request: RpcRequest = match read_json_frame(&mut stream).await {
            Ok(request) => request,
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let response = dispatch(request, &sessions, &epoch).await;
        write_json_frame(&mut stream, &response).await?;
    }
}

#[allow(clippy::too_many_lines)]
async fn dispatch(
    request: RpcRequest,
    sessions: &Arc<Mutex<HashMap<String, Arc<DaemonSession>>>>,
    epoch: &str,
) -> RpcResponse {
    if request.jsonrpc != "2.0" {
        return rpc_error(request.id, -32600, "JSON-RPC version must be 2.0");
    }

    match request.method.as_str() {
        "winx.hello" | "session.negotiate" => rpc_result(
            request.id,
            &HelloResult {
                protocol_major: PROTOCOL_MAJOR,
                protocol_minor: PROTOCOL_MINOR,
                capabilities: vec![
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
                    "multi_consumer_cursors".to_string(),
                    "idempotency".to_string(),
                    "session_activity_timestamps".to_string(),
                    "attach_or_create".to_string(),
                ],
                max_frame_bytes: MAX_FRAME_BYTES,
                daemon_epoch: epoch.to_string(),
                daemon_pid: std::process::id(),
            },
        ),
        "session.configure" => {
            let params: ConfigureSessionParams = match serde_json::from_value(request.params) {
                Ok(params) => params,
                Err(error) => return rpc_error(request.id, -32602, &error.to_string()),
            };
            match configure_session(sessions, params).await {
                Ok(result) => rpc_result(request.id, &result),
                Err(error) => rpc_error(request.id, -32603, &error.to_string()),
            }
        }
        "shell.run_action" => {
            let params: RunActionParams = match serde_json::from_value(request.params) {
                Ok(params) => params,
                Err(error) => return rpc_error(request.id, -32602, &error.to_string()),
            };
            match run_action(sessions, params, epoch).await {
                Ok(result) => rpc_result(request.id, &result),
                Err(error) => rpc_error(request.id, -32603, &error.to_string()),
            }
        }
        "shell.cancel_action" => {
            let params: CancelActionParams = match serde_json::from_value(request.params) {
                Ok(params) => params,
                Err(error) => return rpc_error(request.id, -32602, &error.to_string()),
            };
            if params.expected_guardian_epoch.as_deref().is_some_and(|expected| expected != epoch) {
                return rpc_result(request.id, &false);
            }
            if let Err(error) = validate_identifier("cancellation_key", &params.cancellation_key) {
                return rpc_error(request.id, -32602, &error.to_string());
            }
            let session =
                sessions.lock().await.get(&normalize_thread_id(&params.thread_id)).cloned();
            match session {
                Some(session) => {
                    match launch_cancellation_flag(&session, &params.cancellation_key).await {
                        Ok(flag) => {
                            flag.store(true, Ordering::SeqCst);
                            rpc_result(request.id, &true)
                        }
                        Err(error) => rpc_error(request.id, -32603, &error.to_string()),
                    }
                }
                None => rpc_result(request.id, &false),
            }
        }
        "session.list" => {
            let entries = sessions.lock().await.values().cloned().collect::<Vec<_>>();
            let mut infos = Vec::with_capacity(entries.len());
            for session in entries {
                if let Some(info) = session_info(&session).await {
                    infos.push(info);
                }
            }
            infos.sort_by(|left, right| left.thread_id.cmp(&right.thread_id));
            rpc_result(request.id, &infos)
        }
        "session.info" => {
            let params: SessionParams = match serde_json::from_value(request.params) {
                Ok(params) => params,
                Err(error) => return rpc_error(request.id, -32602, &error.to_string()),
            };
            let session =
                sessions.lock().await.get(&normalize_thread_id(&params.thread_id)).cloned();
            match session {
                Some(session) => match session_info(&session).await {
                    Some(info) => rpc_result(request.id, &info),
                    None => rpc_error(request.id, -32004, "session not initialized"),
                },
                None => rpc_error(request.id, -32004, "session not found"),
            }
        }
        "session.read_output" => {
            let params: JournalReadParams = match serde_json::from_value(request.params) {
                Ok(params) => params,
                Err(error) => return rpc_error(request.id, -32602, &error.to_string()),
            };
            if let Err(error) = validate_identifier("consumer_id", &params.consumer_id) {
                return rpc_error(request.id, -32602, &error.to_string());
            }
            let thread_id = normalize_thread_id(&params.thread_id);
            let session = sessions.lock().await.get(&thread_id).cloned();
            match session {
                Some(session) => {
                    session.note_activity(false);
                    capture_session_outputs(&session).await;
                    let read = session.journal.lock().await.read(&params.consumer_id);
                    rpc_result(request.id, &read)
                }
                None => rpc_error(request.id, -32004, "session not found"),
            }
        }
        "session.kill" => {
            let params: SessionParams = match serde_json::from_value(request.params) {
                Ok(params) => params,
                Err(error) => return rpc_error(request.id, -32602, &error.to_string()),
            };
            let session = sessions.lock().await.remove(&normalize_thread_id(&params.thread_id));
            match session {
                Some(session) => {
                    let _operation = session.operation_barrier.write().await;
                    let background_ids = session.background_ids.lock().await.clone();
                    {
                        let mut store = lock_session_store();
                        for id in background_ids {
                            store.remove_shell(&id);
                        }
                    }
                    *session.state.lock().await = None;
                    rpc_result(request.id, &true)
                }
                None => rpc_result(request.id, &false),
            }
        }
        "session.interrupt" => {
            let params: SessionParams = match serde_json::from_value(request.params) {
                Ok(params) => params,
                Err(error) => return rpc_error(request.id, -32602, &error.to_string()),
            };
            let session =
                sessions.lock().await.get(&normalize_thread_id(&params.thread_id)).cloned();
            match session {
                Some(session) => match interrupt_session(&session, &params, epoch).await {
                    Ok(interrupted) => rpc_result(request.id, &interrupted),
                    Err(error) => rpc_error(request.id, -32005, &error.to_string()),
                },
                None => rpc_error(request.id, -32004, "session not found"),
            }
        }
        _ => rpc_error(request.id, -32601, "method not found"),
    }
}

async fn configure_session(
    sessions: &Arc<Mutex<HashMap<String, Arc<DaemonSession>>>>,
    params: ConfigureSessionParams,
) -> Result<ConfigureSessionResult> {
    let thread_id = normalize_thread_id(&params.snapshot.chat_id);
    if thread_id.is_empty() {
        return Err(WinxError::ThreadIdMismatch(
            "daemon session configuration requires an explicit thread_id".to_string(),
        ));
    }

    let (session, entry_created) = {
        let mut sessions = sessions.lock().await;
        match params.transition {
            ConfigureSessionTransition::FirstCall => match sessions.entry(thread_id) {
                std::collections::hash_map::Entry::Occupied(entry) => (entry.get().clone(), false),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let session = Arc::new(DaemonSession::new());
                    entry.insert(session.clone());
                    (session, true)
                }
            },
            _ => (
                sessions.get(&thread_id).cloned().ok_or(WinxError::BashStateNotInitialized)?,
                false,
            ),
        }
    };
    ensure_drainer(&session);
    session.note_activity(false);
    let _operation = session.operation_barrier.write().await;

    let (attach_hint, snapshot, attached_existing) = {
        let mut guard = session.state.lock().await;
        let mut attached_existing = false;
        match params.transition {
            ConfigureSessionTransition::FirstCall => {
                if entry_created || guard.is_none() {
                    let mut state = BashState::new();
                    state.apply_snapshot(&params.snapshot);
                    if state.cwd.exists() {
                        state.init_pty_shell().await?;
                    }
                    *guard = Some(state);
                } else {
                    // Attach-or-create: preserve the guardian-owned PTY, cwd, mode,
                    // output journal, and command state across adapter reconnects.
                    attached_existing = true;
                }
            }
            ConfigureSessionTransition::ModeChange => {
                let state = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
                let daemon_cwd = state.cwd.clone();
                state.apply_snapshot(&params.snapshot);
                state.cwd = daemon_cwd;
            }
            ConfigureSessionTransition::Reset => {
                let state = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
                let daemon_cwd = state.cwd.clone();
                state.apply_snapshot(&params.snapshot);
                state.cwd = daemon_cwd;
                state.init_pty_shell().await?;
                *session.session_epoch.lock().await = new_epoch();
            }
            ConfigureSessionTransition::WorkspaceChange => {
                let state = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
                state.apply_snapshot(&params.snapshot);
            }
        }
        let state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        let attach_hint = {
            let shell = state.pty_shell.lock().await;
            shell.as_ref().and_then(|shell| shell.attach_hint.clone())
        };
        (attach_hint, state.snapshot(), attached_existing)
    };

    if matches!(params.transition, ConfigureSessionTransition::Reset)
        || (matches!(params.transition, ConfigureSessionTransition::FirstCall)
            && !attached_existing)
    {
        *session.command_id.lock().await = None;
        // Idempotency entries intentionally survive reset. A response-lost
        // owner or waiter must still observe the original operation result;
        // its execution token identifies the old incarnation unambiguously.
        session.delivery_cursors.lock().await.clear();
        session.observed.lock().await.remove("main");
    }
    capture_session_outputs_locked(&session).await;
    session.activity.notify_one();
    Ok(ConfigureSessionResult { attach_hint, snapshot: Some(snapshot), attached_existing })
}

#[allow(clippy::too_many_lines)] // session setup, execution, capture, and idempotent caching
async fn run_action(
    sessions: &Arc<Mutex<HashMap<String, Arc<DaemonSession>>>>,
    mut params: RunActionParams,
    guardian_epoch: &str,
) -> Result<RunActionResult> {
    validate_identifier("request_key", &params.request_key)?;
    validate_identifier("consumer_id", &params.consumer_id)?;
    if let Some(key) = params.options.cancellation_key.as_deref() {
        validate_identifier("cancellation_key", key)?;
    }
    let thread_id = normalize_thread_id(&params.command.thread_id);
    if thread_id.is_empty() {
        return Err(WinxError::ThreadIdMismatch(
            "daemon requests require an explicit thread_id".to_string(),
        ));
    }

    let session = {
        let mut sessions = sessions.lock().await;
        sessions.entry(thread_id.clone()).or_insert_with(|| Arc::new(DaemonSession::new())).clone()
    };
    if let Some(key) = params.options.cancellation_key.as_deref() {
        params.options.launch_cancelled = Some(launch_cancellation_flag(&session, key).await?);
    }
    let fingerprint = canonical_action_fingerprint(&params)?;

    let request_key = params.request_key.clone();
    let (completion, owner) = {
        let mut completed = session.completed.lock().await;
        let now = Instant::now();
        prune_action_entries(&mut completed, now);
        if let Some(entry) = completed.get_mut(&request_key) {
            if entry.fingerprint != fingerprint {
                return Err(WinxError::InvalidInput(
                    "request_key was reused with different shell action parameters".to_string(),
                ));
            }
            entry.last_seen = now;
            match &entry.completion {
                ActionCompletion::Complete(result) => {
                    return cached_action_result((**result).clone())
                }
                ActionCompletion::InFlight(sender) => (Arc::clone(sender), false),
            }
        } else {
            if !evict_oldest_action_if_full(&mut completed) {
                return Err(WinxError::ResourceAllocationError {
                    message: format!(
                        "all {MAX_SESSION_CACHE_ENTRIES} shell action reservations are in flight; retry after one completes"
                    ),
                });
            }
            let completion = Arc::new(watch::channel(None).0);
            completed.insert(
                request_key.clone(),
                ActionEntry {
                    fingerprint,
                    completion: ActionCompletion::InFlight(Arc::clone(&completion)),
                    last_seen: now,
                },
            );
            (completion, true)
        }
    };

    if owner {
        let owner_session = Arc::clone(&session);
        let owner_epoch = guardian_epoch.to_string();
        let owner_completion = Arc::clone(&completion);
        tokio::spawn(async move {
            let mut owner_guard = ActionOwnerGuard::new(Arc::clone(&owner_completion));
            let outcome = match tokio::time::timeout(
                ACTION_OWNER_TIMEOUT,
                execute_action(&owner_session, params, &owner_epoch),
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(_) => Err(WinxError::CommandTimeout {
                    command: "guardian shell action".to_string(),
                    timeout_seconds: ACTION_OWNER_TIMEOUT.as_secs(),
                }),
            };
            let cached = outcome.map_err(|error| error.to_string());
            complete_action(&owner_session, &request_key, cached, &owner_completion).await;
            owner_guard.disarm();
        });
    }

    wait_for_action_completion(completion.subscribe()).await
}

fn cached_action_result(cached: CachedActionResult) -> Result<RunActionResult> {
    cached.map_err(WinxError::CommandExecutionError)
}

async fn wait_for_action_completion(
    mut completion: watch::Receiver<Option<CachedActionResult>>,
) -> Result<RunActionResult> {
    loop {
        if let Some(result) = completion.borrow().clone() {
            return cached_action_result(result);
        }
        if completion.changed().await.is_err() {
            return Err(WinxError::CommandExecutionError(
                "single-flight shell action owner disappeared".to_string(),
            ));
        }
    }
}

async fn complete_action(
    session: &DaemonSession,
    request_key: &str,
    cached: CachedActionResult,
    completion: &watch::Sender<Option<CachedActionResult>>,
) {
    {
        let mut completed = session.completed.lock().await;
        if let Some(entry) = completed.get_mut(request_key) {
            entry.completion = ActionCompletion::Complete(Box::new(cached.clone()));
            entry.last_seen = Instant::now();
        }
    }
    completion.send_replace(Some(cached));
}

fn canonical_action_fingerprint(params: &RunActionParams) -> Result<Vec<u8>> {
    let value = serde_json::to_value((
        &params.snapshot,
        &params.command,
        &params.consumer_id,
        &params.options,
    ))
    .map_err(|error| WinxError::SerializationError(error.to_string()))?;
    serde_json::to_vec(&canonical_json(value))
        .map_err(|error| WinxError::SerializationError(error.to_string()))
}

fn canonical_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonical_json).collect())
        }
        serde_json::Value::Object(values) => {
            let mut entries = values.into_iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            serde_json::Value::Object(
                entries.into_iter().map(|(key, value)| (key, canonical_json(value))).collect(),
            )
        }
        scalar => scalar,
    }
}

#[allow(clippy::too_many_lines)]
async fn execute_action(
    session: &Arc<DaemonSession>,
    params: RunActionParams,
    guardian_epoch: &str,
) -> Result<RunActionResult> {
    let _operation = session.operation_barrier.read().await;
    if params
        .options
        .expected_guardian_epoch
        .as_deref()
        .is_some_and(|expected| expected != guardian_epoch)
        || params
            .options
            .expected_execution
            .as_ref()
            .is_some_and(|expected| expected.guardian_epoch != guardian_epoch)
    {
        return Err(WinxError::InvalidInput(
            "execution precondition refers to a different guardian epoch".to_string(),
        ));
    }
    let session_epoch = session.session_epoch.lock().await.clone();
    if params.options.expected_execution.as_ref().is_some_and(|expected| {
        expected.session_epoch != session_epoch
            || params
                .options
                .expected_generation
                .is_some_and(|generation| generation != expected.generation)
    }) {
        return Err(WinxError::InvalidInput(
            "execution precondition refers to a different session incarnation".to_string(),
        ));
    }
    let is_command =
        matches!(&params.command.action_json, crate::types::BashCommandAction::Command { .. });
    ensure_drainer(session);
    session.note_activity(is_command);

    {
        let mut guard = session.state.lock().await;
        let state = if let Some(state) = guard.as_mut() {
            let daemon_cwd = state.cwd.clone();
            state.apply_snapshot(&params.snapshot);
            state.cwd = daemon_cwd;
            state
        } else {
            let mut state = BashState::new();
            state.apply_snapshot(&params.snapshot);
            guard.insert(state)
        };
        let needs_shell = state.pty_shell.lock().await.is_none();
        if needs_shell && state.cwd.exists() {
            state.init_pty_shell().await?;
        }
    }
    let pty_incarnation_before = {
        let guard = session.state.lock().await;
        let state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        let incarnation =
            state.pty_shell.lock().await.as_ref().map(crate::state::pty::PtyShell::incarnation);
        incarnation
    };

    if is_command {
        *session.command_id.lock().await = Some(params.request_key.clone());
    }
    let consumer_id = if params.consumer_id.is_empty() {
        "legacy-adapter".to_string()
    } else {
        params.consumer_id
    };
    let cursor_key = delivery_cursor_key(&consumer_id, &params.command.action_json);
    let cursor = {
        let now = Instant::now();
        let mut cursors = session.delivery_cursors.lock().await;
        prune_timed_entries(&mut cursors, now);
        evict_oldest_if_full(&mut cursors, &cursor_key);
        let entry = cursors.entry(cursor_key).or_insert_with(|| TimedEntry {
            value: Arc::new(Mutex::new(ShellDeliveryCursor::default())),
            last_seen: now,
        });
        entry.last_seen = now;
        entry.value.clone()
    };
    let outcome = crate::tools::bash_command::handle_embedded_tool_call_with_cursor_detailed(
        &session.state,
        params.command,
        &cursor,
        params.options,
    )
    .await;
    let pty_incarnation_after = {
        let guard = session.state.lock().await;
        let state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        let incarnation =
            state.pty_shell.lock().await.as_ref().map(crate::state::pty::PtyShell::incarnation);
        incarnation
    };
    let session_epoch = if pty_incarnation_before == pty_incarnation_after {
        session_epoch
    } else {
        let next = new_epoch();
        *session.session_epoch.lock().await = next.clone();
        next
    };
    if let Ok(result) = &outcome {
        if let Some(id) = result.result.state.background_id.as_ref() {
            session.background_ids.lock().await.insert(id.clone());
        }
    }
    capture_session_outputs_locked(session).await;
    let snapshot =
        session.state.lock().await.as_ref().ok_or(WinxError::BashStateNotInitialized)?.snapshot();
    let result = match outcome {
        Ok(result) => {
            let command_generation = result.command_generation;
            let execution_token = command_generation.map(|generation| ShellExecutionToken {
                guardian_epoch: guardian_epoch.to_string(),
                session_epoch,
                generation,
            });
            // Compact and legacy are mutually exclusive on the wire. Compact
            // rendering leaves the stable legacy string empty at runtime.
            let (output, compact_output) = match result.compact_output {
                Some(compact) => (None, Some(compact)),
                None => (Some(result.result.output), None),
            };
            RunActionResult {
                output,
                compact_output,
                command_generation,
                execution_token,
                output_truncated: result.output_truncated,
                state: Some(result.result.state),
                snapshot,
                error: None,
            }
        }
        Err(error) => RunActionResult {
            output: None,
            compact_output: None,
            command_generation: None,
            execution_token: None,
            output_truncated: false,
            state: None,
            snapshot,
            error: Some(to_wire_error(error)),
        },
    };

    session.activity.notify_one();
    Ok(result)
}

fn prune_action_entries(entries: &mut HashMap<String, ActionEntry>, now: Instant) {
    entries.retain(|_, entry| {
        !action_entry_is_terminal(entry) || now.duration_since(entry.last_seen) <= SESSION_CACHE_TTL
    });
}

fn action_entry_is_terminal(entry: &ActionEntry) -> bool {
    match &entry.completion {
        ActionCompletion::Complete(_) => true,
        ActionCompletion::InFlight(completion) => completion.borrow().is_some(),
    }
}

fn evict_oldest_action_if_full(entries: &mut HashMap<String, ActionEntry>) -> bool {
    if entries.len() < MAX_SESSION_CACHE_ENTRIES {
        return true;
    }
    if let Some(oldest) = entries
        .iter()
        .filter(|(_, entry)| action_entry_is_terminal(entry))
        .min_by_key(|(_, entry)| entry.last_seen)
        .map(|(key, _)| key.clone())
    {
        entries.remove(&oldest);
    }
    entries.len() < MAX_SESSION_CACHE_ENTRIES
}

fn delivery_cursor_key(consumer_id: &str, action: &crate::types::BashCommandAction) -> String {
    let background_id = match action {
        crate::types::BashCommandAction::StatusCheck { bg_command_id, .. }
        | crate::types::BashCommandAction::SendText { bg_command_id, .. }
        | crate::types::BashCommandAction::SendSpecials { bg_command_id, .. }
        | crate::types::BashCommandAction::SendAscii { bg_command_id, .. }
        | crate::types::BashCommandAction::Screen { bg_command_id, .. }
        | crate::types::BashCommandAction::WaitForTurn { bg_command_id, .. } => {
            bg_command_id.as_deref()
        }
        crate::types::BashCommandAction::Command { .. } => None,
    };
    match background_id {
        Some(id) => format!("{consumer_id}:background:{id}"),
        None => format!("{consumer_id}:main"),
    }
}

fn ensure_drainer(session: &Arc<DaemonSession>) {
    if session.drainer_started.swap(true, Ordering::AcqRel) {
        session.activity.notify_one();
        return;
    }
    let session = session.clone();
    tokio::spawn(async move {
        loop {
            if Arc::strong_count(&session) == 1 {
                return;
            }
            let capture = capture_session_outputs(&session).await;
            tokio::select! {
                () = tokio::time::sleep(capture.next_delay()) => {}
                () = session.activity.notified() => {}
            }
        }
    });
}

async fn capture_session_outputs(session: &Arc<DaemonSession>) -> CaptureState {
    let _operation = session.operation_barrier.read().await;
    capture_session_outputs_locked(session).await
}

async fn capture_session_outputs_locked(session: &Arc<DaemonSession>) -> CaptureState {
    let (thread_id, main) = {
        let state = session.state.lock().await;
        let Some(state) = state.as_ref() else { return CaptureState::default() };
        (state.current_thread_id.clone(), state.pty_shell.clone())
    };
    let mut capture = capture_shell_output(session, "main", &main).await;

    let background_ids = session.background_ids.lock().await.clone();
    for id in background_ids {
        let shell = {
            let store = lock_session_store();
            store.resolve(&thread_id, &ShellTarget::Background(id.clone()))
        };
        if let Some(shell) = shell {
            capture.merge(capture_shell_output(session, &id, &shell).await);
        }
    }
    capture
}

async fn capture_shell_output(
    session: &Arc<DaemonSession>,
    terminal_id: &str,
    shell: &SharedPtyShell,
) -> CaptureState {
    let (generation, snapshot, active) = {
        let mut guard = shell.lock().await;
        let Some(shell) = guard.as_mut() else { return CaptureState::default() };
        shell.poll_output_nonblocking();
        (shell.command_generation(), shell.output_snapshot(), shell.command_running)
    };
    let delta = {
        let mut observed = session.observed.lock().await;
        let (previous_generation, previous) =
            observed.entry(terminal_id.to_string()).or_insert_with(|| (generation, String::new()));
        if *previous_generation != generation {
            *previous_generation = generation;
            previous.clear();
        }
        let delta = snapshot
            .strip_prefix(previous.as_str())
            .map_or_else(|| snapshot.clone(), str::to_string);
        previous.clone_from(&snapshot);
        delta
    };
    let changed = !delta.is_empty();
    if changed {
        session.last_activity_unix_ms.store(unix_ms(), Ordering::Release);
        session.journal.lock().await.append(delta);
    }
    CaptureState { changed, active }
}

async fn session_info(session: &Arc<DaemonSession>) -> Option<SessionInfo> {
    let _operation = session.operation_barrier.read().await;
    let (thread_id, cwd, main) = {
        let state = session.state.lock().await;
        let state = state.as_ref()?;
        (
            state.current_thread_id.clone(),
            state.cwd.to_string_lossy().into_owned(),
            state.pty_shell.clone(),
        )
    };
    let (shell_pid, running) = {
        let guard = main.lock().await;
        guard.as_ref().map_or((None, false), |shell| (shell.process_id(), shell.command_running))
    };
    let background_command_ids = active_background_command_ids(session, &thread_id).await;
    let last_command_at_unix_ms = session.last_command_at_unix_ms.load(Ordering::Acquire);
    Some(SessionInfo {
        thread_id,
        cwd,
        shell_pid,
        command_id: session.command_id.lock().await.clone(),
        running,
        background_command_ids,
        created_at_unix_ms: Some(session.created_at_unix_ms),
        last_activity_unix_ms: Some(session.last_activity_unix_ms.load(Ordering::Acquire)),
        last_command_at_unix_ms: (last_command_at_unix_ms > 0).then_some(last_command_at_unix_ms),
        ever_ran_command: session.ever_ran_command.load(Ordering::Acquire),
    })
}

async fn active_background_command_ids(
    session: &Arc<DaemonSession>,
    thread_id: &str,
) -> Vec<String> {
    let candidates = session.background_ids.lock().await.clone();
    let mut active = Vec::new();
    for id in candidates {
        let shell = {
            let store = lock_session_store();
            store.resolve(thread_id, &ShellTarget::Background(id.clone()))
        };
        let Some(shell) = shell else { continue };
        // Capture the final queued bytes before deciding that a completed shell is
        // no longer active. Otherwise pruning the id here could make the journal
        // drainer stop one poll too early.
        capture_shell_output(session, &id, &shell).await;
        let running = {
            let guard = shell.lock().await;
            guard.as_ref().is_some_and(|shell| shell.command_running)
        };
        if running {
            active.push(id);
        }
    }

    let active_set = active.iter().cloned().collect::<HashSet<_>>();
    session.background_ids.lock().await.retain(|id| active_set.contains(id));
    active.sort();
    active
}

async fn interrupt_session(
    session: &Arc<DaemonSession>,
    params: &SessionParams,
    guardian_epoch: &str,
) -> Result<bool> {
    session.note_activity(false);
    let _operation = session.operation_barrier.read().await;
    if params.expected_guardian_epoch.as_deref().is_some_and(|expected| expected != guardian_epoch)
        || params
            .expected_execution
            .as_ref()
            .is_some_and(|expected| expected.guardian_epoch != guardian_epoch)
    {
        return Ok(false);
    }
    let session_epoch = session.session_epoch.lock().await;
    if params.expected_execution.as_ref().is_some_and(|expected| {
        expected.session_epoch != *session_epoch
            || params
                .expected_generation
                .is_some_and(|generation| generation != expected.generation)
    }) {
        return Ok(false);
    }
    drop(session_epoch);
    let shell = {
        let state = session.state.lock().await;
        state.as_ref().map(|state| state.pty_shell.clone())
    };
    let Some(shell) = shell else { return Ok(false) };
    {
        let mut guard = shell.lock().await;
        if let Some(shell) = guard.as_mut() {
            if params.expected_generation.is_some_and(|expected| {
                shell.command_generation() != expected || !shell.command_running
            }) {
                return Ok(false);
            }
            shell.send_interrupt().map_err(|error| {
                WinxError::CommandExecutionError(format!("failed to interrupt shell: {error}"))
            })?;
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        let recovered = {
            let mut guard = shell.lock().await;
            match guard.as_mut() {
                Some(shell) => shell.poll_output_nonblocking() || !shell.command_running,
                None => true,
            }
        };
        if recovered {
            capture_session_outputs_locked(session).await;
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(WinxError::CommandExecutionError(
        "interrupted shell did not return to a prompt within 3 seconds".to_string(),
    ))
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn to_wire_error(error: WinxError) -> WireShellError {
    match error {
        WinxError::BashStateNotInitialized => WireShellError::BashStateNotInitialized,
        WinxError::ShellInitializationError(message) => {
            WireShellError::ShellInitialization(message)
        }
        WinxError::CommandExecutionError(message) => WireShellError::CommandExecution(message),
        WinxError::NoActiveCommand(message) => WireShellError::NoActiveCommand(message),
        WinxError::BackgroundSessionNotFound(message) => {
            WireShellError::BackgroundSessionNotFound(message)
        }
        WinxError::EmptyInteractiveInput { action } => {
            WireShellError::EmptyInteractiveInput(action)
        }
        WinxError::InteractiveTargetNotRunning(message) => {
            WireShellError::InteractiveTargetNotRunning(message)
        }
        WinxError::CommandAlreadyRunning { current_command, duration_seconds } => {
            WireShellError::CommandAlreadyRunning { current_command, duration_seconds }
        }
        WinxError::CommandNotAllowed(message) => WireShellError::CommandNotAllowed(message),
        WinxError::ThreadIdMismatch(message) => WireShellError::ThreadIdMismatch(message),
        WinxError::InvalidInput(message) | WinxError::ArgumentParseError(message) => {
            WireShellError::InvalidInput(message)
        }
        other => WireShellError::Other(other.to_string()),
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
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use tokio::sync::{watch, Mutex};

    use super::{
        canonical_action_fingerprint, configure_session, interrupt_session,
        launch_cancellation_flag, run_action, validate_identifier, ActionCompletion, ActionEntry,
        ActionOwnerGuard, CaptureState, DaemonSession, OutputJournal, MAX_SESSION_CACHE_ENTRIES,
    };
    use crate::daemon::protocol::{
        ConfigureSessionParams, ConfigureSessionTransition, RunActionParams, SessionParams,
    };
    use crate::errors::WinxError;
    use crate::runtime::ShellActionOptions;
    use crate::state::bash_state::BashState;
    use crate::types::BashCommand;

    fn action_params(state: &BashState, command: &str, request_key: &str) -> RunActionParams {
        let command: BashCommand = serde_json::from_value(serde_json::json!({
            "action_json": {
                "type": "command",
                "command": command,
                "is_background": false,
                "allow_multi": true
            },
            "thread_id": state.current_thread_id,
            "wait_for_seconds": 2.0
        }))
        .expect("valid command");
        RunActionParams {
            snapshot: state.snapshot(),
            command,
            request_key: request_key.to_string(),
            consumer_id: "single-flight-test".to_string(),
            options: ShellActionOptions::default(),
        }
    }

    #[test]
    fn late_consumer_is_told_when_journal_head_was_dropped() {
        let mut journal = OutputJournal::default();
        journal.append("a".repeat(3 * 1024 * 1024));
        journal.append("b".repeat(3 * 1024 * 1024));

        let read = journal.read("late");
        assert!(read.gap);
        assert!(read.output.starts_with('b'));
    }

    #[test]
    fn journal_consumer_cursors_are_lru_bounded() {
        let mut journal = OutputJournal::default();
        journal.append("output".to_string());
        for index in 0..(MAX_SESSION_CACHE_ENTRIES + 100) {
            let _ = journal.read(&format!("consumer-{index}"));
        }
        assert_eq!(journal.cursors.len(), MAX_SESSION_CACHE_ENTRIES);
    }

    #[test]
    fn daemon_identifiers_have_a_small_memory_bound() {
        assert!(validate_identifier("consumer_id", &"x".repeat(256)).is_ok());
        assert!(validate_identifier("consumer_id", &"x".repeat(257)).is_err());
    }

    #[test]
    fn drainer_backoff_tracks_output_and_command_activity() {
        assert!(
            CaptureState { changed: true, active: false }.next_delay()
                < CaptureState { changed: false, active: true }.next_delay()
        );
        assert!(
            CaptureState { changed: false, active: true }.next_delay()
                < CaptureState { changed: false, active: false }.next_delay()
        );
    }

    #[tokio::test]
    async fn request_key_is_single_flight_and_replays_lost_response() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let marker = temp.path().join("single-flight.marker");
        let mut state = BashState::new();
        state.cwd = temp.path().to_path_buf();
        state.workspace_root = temp.path().to_path_buf();
        state.current_thread_id = "single_flight".to_string();
        let command = format!("printf x >> {}; sleep 0.1", marker.display());
        let params = action_params(&state, &command, "same-request");
        let sessions = Arc::new(Mutex::new(HashMap::<String, Arc<DaemonSession>>::new()));

        let (first, concurrent) = tokio::join!(
            run_action(&sessions, params.clone(), "guardian-a"),
            run_action(&sessions, params.clone(), "guardian-a")
        );
        assert!(first.expect("owner result").error.is_none());
        assert!(concurrent.expect("shared result").error.is_none());
        let replay = run_action(&sessions, params, "guardian-a").await.expect("cached replay");
        assert!(replay.error.is_none());
        assert_eq!(std::fs::read_to_string(marker).expect("marker"), "x");
    }

    #[tokio::test]
    async fn request_key_collision_rejects_different_parameters() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let mut state = BashState::new();
        state.cwd = temp.path().to_path_buf();
        state.workspace_root = temp.path().to_path_buf();
        state.current_thread_id = "request_collision".to_string();
        let sessions = Arc::new(Mutex::new(HashMap::<String, Arc<DaemonSession>>::new()));
        run_action(&sessions, action_params(&state, "printf first", "collision"), "guardian-a")
            .await
            .expect("first action");

        let error = run_action(
            &sessions,
            action_params(&state, "printf second", "collision"),
            "guardian-a",
        )
        .await
        .expect_err("collision must fail");
        assert!(error.to_string().contains("different shell action parameters"));
    }

    #[test]
    fn action_fingerprint_is_canonical_across_map_insertion_order() {
        let mut first = BashState::new();
        first.current_thread_id = "canonical".to_string();
        first.whitelist_for_overwrite.insert(
            "z".to_string(),
            crate::state::bash_state::FileWhitelistData::new("z".into(), vec![(1, 1)], 1),
        );
        first.whitelist_for_overwrite.insert(
            "a".to_string(),
            crate::state::bash_state::FileWhitelistData::new("a".into(), vec![(1, 1)], 1),
        );
        let mut second = BashState::new();
        second.current_thread_id = "canonical".to_string();
        second.whitelist_for_overwrite.insert(
            "a".to_string(),
            crate::state::bash_state::FileWhitelistData::new("a".into(), vec![(1, 1)], 1),
        );
        second.whitelist_for_overwrite.insert(
            "z".to_string(),
            crate::state::bash_state::FileWhitelistData::new("z".into(), vec![(1, 1)], 1),
        );

        assert_eq!(
            canonical_action_fingerprint(&action_params(&first, "printf stable", "key"))
                .expect("first fingerprint"),
            canonical_action_fingerprint(&action_params(&second, "printf stable", "key"))
                .expect("second fingerprint")
        );
    }

    #[tokio::test]
    async fn action_cache_applies_backpressure_when_every_entry_is_in_flight() {
        let state = BashState::new();
        let session = Arc::new(DaemonSession::new());
        {
            let mut completed = session.completed.lock().await;
            for index in 0..MAX_SESSION_CACHE_ENTRIES {
                completed.insert(
                    format!("working-{index}"),
                    ActionEntry {
                        fingerprint: vec![u8::try_from(index % 255).unwrap_or_default()],
                        completion: ActionCompletion::InFlight(Arc::new(watch::channel(None).0)),
                        last_seen: Instant::now(),
                    },
                );
            }
        }
        let sessions =
            Arc::new(Mutex::new(HashMap::from([(state.current_thread_id.clone(), session)])));
        let error = run_action(
            &sessions,
            action_params(&state, "printf blocked", "overflow"),
            "guardian-a",
        )
        .await
        .expect_err("all-in-flight cache must reject new reservations");
        assert!(matches!(error, WinxError::ResourceAllocationError { .. }));
    }

    #[tokio::test]
    async fn cancellation_reservations_never_evict_active_flags() {
        let session = DaemonSession::new();
        let mut active = Vec::new();
        for index in 0..MAX_SESSION_CACHE_ENTRIES {
            active.push(
                launch_cancellation_flag(&session, &format!("active-{index}"))
                    .await
                    .expect("active cancellation reservation"),
            );
        }
        let error = launch_cancellation_flag(&session, "overflow")
            .await
            .expect_err("all active cancellation flags must apply backpressure");
        assert!(matches!(error, WinxError::ResourceAllocationError { .. }));
        assert_eq!(session.launch_reservations.lock().await.len(), MAX_SESSION_CACHE_ENTRIES);

        drop(active.pop());
        launch_cancellation_flag(&session, "overflow")
            .await
            .expect("inactive cancellation flag should be evictable");
        assert_eq!(session.launch_reservations.lock().await.len(), MAX_SESSION_CACHE_ENTRIES);
    }

    #[tokio::test]
    async fn automatic_pty_reset_advances_guardian_incarnation() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let mut state = BashState::new();
        state.cwd = temp.path().to_path_buf();
        state.workspace_root = temp.path().to_path_buf();
        state.current_thread_id = "automatic_pty_reset".to_string();
        let sessions = Arc::new(Mutex::new(HashMap::new()));

        let first = run_action(
            &sessions,
            action_params(&state, "printf first-incarnation", "before-auto-reset"),
            "guardian-a",
        )
        .await
        .expect("first action");
        let prior_token = first.execution_token.expect("first execution token");

        let mut forced = action_params(&state, "printf reset-incarnation", "forced-auto-reset");
        forced.options.force_clear_to_run_failure = true;
        let reset = run_action(&sessions, forced, "guardian-a").await.expect("reset action");
        let current = reset.execution_token.expect("reset execution token");
        assert_eq!(prior_token.generation, current.generation);
        assert_ne!(prior_token.session_epoch, current.session_epoch);

        let mut stale_status = action_params(&state, "printf unused", "stale-after-auto-reset");
        stale_status.command = serde_json::from_value(serde_json::json!({
            "action_json": {"type": "status_check", "status_check": true},
            "thread_id": "automatic_pty_reset",
            "wait_for_seconds": 0.0
        }))
        .expect("status command");
        stale_status.options.expected_generation = Some(prior_token.generation);
        stale_status.options.expected_execution = Some(prior_token);
        assert!(run_action(&sessions, stale_status, "guardian-a").await.is_err());
    }

    #[tokio::test]
    async fn abandoned_action_owner_notifies_waiters() {
        let completion = Arc::new(watch::channel(None).0);
        let receiver = completion.subscribe();
        drop(ActionOwnerGuard::new(completion));
        let error = super::wait_for_action_completion(receiver)
            .await
            .expect_err("abandoned owner must fail its waiters");
        assert!(error.to_string().contains("owner disappeared"));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One deterministic reset/action/interrupt schedule.
    async fn reset_is_atomic_with_action_status_and_interrupt_across_generation_collision() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let mut state = BashState::new();
        state.cwd = temp.path().to_path_buf();
        state.workspace_root = temp.path().to_path_buf();
        state.current_thread_id = "atomic_reset".to_string();
        let session = Arc::new(DaemonSession::new());
        let sessions = Arc::new(Mutex::new(HashMap::from([(
            state.current_thread_id.clone(),
            Arc::clone(&session),
        )])));
        let held = session.operation_barrier.write().await;
        let owner_sessions = Arc::clone(&sessions);
        let first_params =
            action_params(&state, "sh -c 'sleep 0.25; printf old-incarnation'", "old-action");
        let first =
            tokio::spawn(
                async move { run_action(&owner_sessions, first_params, "guardian-a").await },
            );

        while !session.completed.lock().await.contains_key("old-action") {
            tokio::task::yield_now().await;
        }
        // Let the owner reach the barrier while the test-held writer makes the
        // queue ordering deterministic, then enqueue reset behind it.
        tokio::time::sleep(Duration::from_millis(10)).await;
        let reset_sessions = Arc::clone(&sessions);
        let reset_snapshot = state.snapshot();
        let mut reset = tokio::spawn(async move {
            configure_session(
                &reset_sessions,
                ConfigureSessionParams {
                    snapshot: reset_snapshot,
                    transition: ConfigureSessionTransition::Reset,
                },
            )
            .await
        });
        drop(held);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut reset).await.is_err(),
            "reset crossed an in-flight action's session barrier"
        );

        let first = tokio::time::timeout(Duration::from_secs(5), first)
            .await
            .expect("first action timeout")
            .expect("first join")
            .expect("first result");
        let stale_execution = first.execution_token.expect("old execution token");
        tokio::time::timeout(Duration::from_secs(5), reset)
            .await
            .expect("reset timeout")
            .expect("reset join")
            .expect("reset result");

        let mut new_action =
            action_params(&state, "sh -c 'sleep 0.15; printf new-incarnation'", "new-action");
        new_action.command.wait_for_seconds = Some(0.001);
        let current = tokio::time::timeout(
            Duration::from_secs(5),
            run_action(&sessions, new_action, "guardian-a"),
        )
        .await
        .expect("new action timeout")
        .expect("new action");
        let current_token = current.execution_token.expect("current execution token");
        assert_eq!(stale_execution.generation, current_token.generation);
        assert_ne!(stale_execution.session_epoch, current_token.session_epoch);

        let mut stale_status = action_params(&state, "printf unused", "stale-status");
        stale_status.command = serde_json::from_value(serde_json::json!({
            "action_json": {"type": "status_check", "status_check": true},
            "thread_id": "atomic_reset",
            "wait_for_seconds": 0.01
        }))
        .expect("status command");
        stale_status.options.expected_generation = Some(stale_execution.generation);
        stale_status.options.expected_execution = Some(stale_execution.clone());
        assert!(tokio::time::timeout(
            Duration::from_secs(5),
            run_action(&sessions, stale_status, "guardian-a"),
        )
        .await
        .expect("stale status timeout")
        .is_err());

        assert!(!interrupt_session(
            &session,
            &SessionParams {
                thread_id: "atomic_reset".to_string(),
                expected_generation: Some(stale_execution.generation),
                expected_execution: Some(stale_execution),
                expected_guardian_epoch: Some("guardian-a".to_string()),
            },
            "guardian-a",
        )
        .await
        .expect("stale interrupt"));

        let mut current_status = action_params(&state, "printf unused", "current-status");
        current_status.command = serde_json::from_value(serde_json::json!({
            "action_json": {"type": "status_check", "status_check": true},
            "thread_id": "atomic_reset",
            "wait_for_seconds": 1.0
        }))
        .expect("current status command");
        current_status.options.expected_generation = Some(current_token.generation);
        current_status.options.expected_execution = Some(current_token);
        let drained = tokio::time::timeout(
            Duration::from_secs(5),
            run_action(&sessions, current_status, "guardian-a"),
        )
        .await
        .expect("current status timeout")
        .expect("current status");
        assert!(
            drained.output.as_deref().is_some_and(|output| output.contains("new-incarnation")),
            "stale operations consumed or interrupted the new incarnation: {drained:?}"
        );
    }
}
