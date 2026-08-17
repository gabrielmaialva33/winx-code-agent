use std::collections::{HashMap, HashSet, VecDeque};
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use super::protocol::{
    read_json_frame, write_json_frame, ConfigureSessionParams, ConfigureSessionResult,
    ConfigureSessionTransition, HelloResult, JournalRead, JournalReadParams, RpcError, RpcRequest,
    RpcResponse, RunActionParams, RunActionResult, SessionInfo, SessionParams, WireShellError,
    MAX_FRAME_BYTES, PROTOCOL_MAJOR, PROTOCOL_MINOR,
};
use crate::errors::{Result, WinxError};
use crate::runtime::{lock_session_store, ShellTarget};
use crate::state::bash_state::BashState;
use crate::state::pty::SharedPtyShell;
use crate::tools::bash_command::ShellDeliveryCursor;
use crate::types::normalize_thread_id;

type SharedState = Arc<Mutex<Option<BashState>>>;

struct DaemonSession {
    state: SharedState,
    completed: Mutex<HashMap<String, RunActionResult>>,
    journal: Mutex<OutputJournal>,
    observed: Mutex<HashMap<String, (u64, String)>>,
    background_ids: Mutex<HashSet<String>>,
    command_id: Mutex<Option<String>>,
    delivery_cursors: Mutex<HashMap<String, Arc<Mutex<ShellDeliveryCursor>>>>,
    drainer_started: AtomicBool,
}

impl DaemonSession {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            completed: Mutex::new(HashMap::new()),
            journal: Mutex::new(OutputJournal::default()),
            observed: Mutex::new(HashMap::new()),
            background_ids: Mutex::new(HashSet::new()),
            command_id: Mutex::new(None),
            delivery_cursors: Mutex::new(HashMap::new()),
            drainer_started: AtomicBool::new(false),
        }
    }
}

const MAX_JOURNAL_BYTES: usize = 4 * 1024 * 1024;

#[derive(Default)]
struct OutputJournal {
    chunks: VecDeque<(u64, String)>,
    cursors: HashMap<String, u64>,
    next_seq: u64,
    bytes: usize,
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
        let oldest = self.chunks.front().map_or(self.next_seq.saturating_add(1), |(seq, _)| *seq);
        let cursor = self.cursors.get(consumer_id).copied().unwrap_or(0);
        let gap = cursor.saturating_add(1) < oldest;
        let mut output = String::new();
        let mut next_seq = cursor;
        for (seq, chunk) in self.chunks.iter().filter(|(seq, _)| *seq > cursor) {
            output.push_str(chunk);
            next_seq = *seq;
        }
        self.cursors.insert(consumer_id.to_string(), next_seq);
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
        "winx.hello" => rpc_result(
            request.id,
            &HelloResult {
                protocol_major: PROTOCOL_MAJOR,
                protocol_minor: PROTOCOL_MINOR,
                capabilities: vec![
                    "session.configure".to_string(),
                    "shell.run_action".to_string(),
                    "session.list".to_string(),
                    "session.read_output".to_string(),
                    "session.kill".to_string(),
                    "multi_consumer_cursors".to_string(),
                    "idempotency".to_string(),
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
            match run_action(sessions, params).await {
                Ok(result) => rpc_result(request.id, &result),
                Err(error) => rpc_error(request.id, -32603, &error.to_string()),
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
            let thread_id = normalize_thread_id(&params.thread_id);
            let session = sessions.lock().await.get(&thread_id).cloned();
            match session {
                Some(session) => {
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
                Some(session) => match interrupt_session(&session).await {
                    Ok(()) => rpc_result(request.id, &true),
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

    let session = {
        let mut sessions = sessions.lock().await;
        match params.transition {
            ConfigureSessionTransition::FirstCall => {
                sessions.entry(thread_id).or_insert_with(|| Arc::new(DaemonSession::new())).clone()
            }
            _ => sessions.get(&thread_id).cloned().ok_or(WinxError::BashStateNotInitialized)?,
        }
    };
    ensure_drainer(&session);

    let attach_hint = {
        let mut guard = session.state.lock().await;
        match params.transition {
            ConfigureSessionTransition::FirstCall => {
                let mut state = BashState::new();
                state.apply_snapshot(&params.snapshot);
                if state.cwd.exists() {
                    state.init_pty_shell().await?;
                }
                *guard = Some(state);
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
            }
            ConfigureSessionTransition::WorkspaceChange => {
                let state = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
                state.apply_snapshot(&params.snapshot);
            }
        }
        let state = guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
        let shell = state.pty_shell.lock().await;
        shell.as_ref().and_then(|shell| shell.attach_hint.clone())
    };

    if matches!(
        params.transition,
        ConfigureSessionTransition::FirstCall | ConfigureSessionTransition::Reset
    ) {
        *session.command_id.lock().await = None;
        session.completed.lock().await.clear();
        session.delivery_cursors.lock().await.clear();
        session.observed.lock().await.remove("main");
    }
    capture_session_outputs(&session).await;
    Ok(ConfigureSessionResult { attach_hint })
}

async fn run_action(
    sessions: &Arc<Mutex<HashMap<String, Arc<DaemonSession>>>>,
    params: RunActionParams,
) -> Result<RunActionResult> {
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
    ensure_drainer(&session);

    if let Some(cached) = session.completed.lock().await.get(&params.request_key).cloned() {
        return Ok(cached);
    }

    {
        let mut guard = session.state.lock().await;
        if let Some(state) = guard.as_mut() {
            let daemon_cwd = state.cwd.clone();
            state.apply_snapshot(&params.snapshot);
            state.cwd = daemon_cwd;
        } else {
            let mut state = BashState::new();
            state.apply_snapshot(&params.snapshot);
            *guard = Some(state);
        }
    }

    if matches!(&params.command.action_json, crate::types::BashCommandAction::Command { .. }) {
        *session.command_id.lock().await = Some(params.request_key.clone());
    }
    let consumer_id = if params.consumer_id.is_empty() {
        "legacy-adapter".to_string()
    } else {
        params.consumer_id
    };
    let cursor_key = delivery_cursor_key(&consumer_id, &params.command.action_json);
    let cursor = {
        let mut cursors = session.delivery_cursors.lock().await;
        if cursors.len() >= 256 && !cursors.contains_key(&cursor_key) {
            if let Some(oldest) = cursors.keys().next().cloned() {
                cursors.remove(&oldest);
            }
        }
        cursors
            .entry(cursor_key)
            .or_insert_with(|| Arc::new(Mutex::new(ShellDeliveryCursor::default())))
            .clone()
    };
    let outcome = crate::tools::bash_command::handle_embedded_tool_call_with_cursor(
        &session.state,
        params.command,
        &cursor,
    )
    .await;
    if let Ok(output) = &outcome {
        if let Some(id) = output.lines().find_map(|line| line.strip_prefix("bg_command_id = ")) {
            session.background_ids.lock().await.insert(id.to_string());
        }
    }
    capture_session_outputs(&session).await;
    let snapshot =
        session.state.lock().await.as_ref().ok_or(WinxError::BashStateNotInitialized)?.snapshot();
    let result = match outcome {
        Ok(output) => RunActionResult { output: Some(output), snapshot, error: None },
        Err(error) => RunActionResult { output: None, snapshot, error: Some(to_wire_error(error)) },
    };

    let mut completed = session.completed.lock().await;
    if completed.len() >= 256 {
        completed.clear();
    }
    completed.insert(params.request_key, result.clone());
    Ok(result)
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
        return;
    }
    let session = session.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if Arc::strong_count(&session) == 1 {
                return;
            }
            capture_session_outputs(&session).await;
        }
    });
}

async fn capture_session_outputs(session: &Arc<DaemonSession>) {
    let (thread_id, main) = {
        let state = session.state.lock().await;
        let Some(state) = state.as_ref() else { return };
        (state.current_thread_id.clone(), state.pty_shell.clone())
    };
    capture_shell_output(session, "main", &main).await;

    let background_ids = session.background_ids.lock().await.clone();
    for id in background_ids {
        let shell = {
            let store = lock_session_store();
            store.resolve(&thread_id, &ShellTarget::Background(id.clone()))
        };
        if let Some(shell) = shell {
            capture_shell_output(session, &id, &shell).await;
        }
    }
}

async fn capture_shell_output(
    session: &Arc<DaemonSession>,
    terminal_id: &str,
    shell: &SharedPtyShell,
) {
    let (generation, snapshot) = {
        let mut guard = shell.lock().await;
        let Some(shell) = guard.as_mut() else { return };
        shell.poll_output_nonblocking();
        (shell.command_generation(), shell.output_snapshot())
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
    session.journal.lock().await.append(delta);
}

async fn session_info(session: &Arc<DaemonSession>) -> Option<SessionInfo> {
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
    Some(SessionInfo {
        thread_id,
        cwd,
        shell_pid,
        command_id: session.command_id.lock().await.clone(),
        running,
        background_command_ids,
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

async fn interrupt_session(session: &Arc<DaemonSession>) -> Result<()> {
    let shell = {
        let state = session.state.lock().await;
        state.as_ref().map(|state| state.pty_shell.clone())
    };
    let Some(shell) = shell else { return Ok(()) };
    {
        let mut guard = shell.lock().await;
        if let Some(shell) = guard.as_mut() {
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
            capture_session_outputs(session).await;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(WinxError::CommandExecutionError(
        "interrupted shell did not return to a prompt within 3 seconds".to_string(),
    ))
}

fn to_wire_error(error: WinxError) -> WireShellError {
    match error {
        WinxError::BashStateNotInitialized => WireShellError::BashStateNotInitialized,
        WinxError::ShellInitializationError(message) => {
            WireShellError::ShellInitialization(message)
        }
        WinxError::CommandExecutionError(message) => WireShellError::CommandExecution(message),
        WinxError::CommandNotAllowed(message) => WireShellError::CommandNotAllowed(message),
        WinxError::ThreadIdMismatch(message) => WireShellError::ThreadIdMismatch(message),
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
    // SAFETY: geteuid has no preconditions and reads process credentials only.
    Ok(credentials.uid() == unsafe { libc::geteuid() })
}

#[cfg(test)]
mod tests {
    use super::OutputJournal;

    #[test]
    fn late_consumer_is_told_when_journal_head_was_dropped() {
        let mut journal = OutputJournal::default();
        journal.append("a".repeat(3 * 1024 * 1024));
        journal.append("b".repeat(3 * 1024 * 1024));

        let read = journal.read("late");
        assert!(read.gap);
        assert!(read.output.starts_with('b'));
    }
}
