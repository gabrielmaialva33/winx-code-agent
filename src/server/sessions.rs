use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rmcp::service::{NotificationContext, RoleServer};
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::{SharedBashState, WinxService};
use crate::runtime::{EmbeddedShellRuntime, ShellRuntime};
use crate::state::bash_state::generate_thread_id;
use crate::state::task_state::TaskRegistry;
use crate::types::{Initialize, InitializeType, ModeName};

/// Upper bound on concurrently-live adapter sessions. Each embedded session owns
/// a PTY; daemon-backed sessions also release their guardian when evicted.
pub(super) const MAX_SESSIONS: usize = 32;

/// Per-`thread_id` shell sessions. Each `thread_id` gets its own
/// `BashState`/PTY, so concurrent threads (or HTTP clients sharing the service)
/// never execute in each other's shell. Tools that don't carry a `thread_id`
/// (legacy clients) fall back to the most recently active session.
#[derive(Default)]
pub(super) struct SessionRegistry {
    pub(super) slots: HashMap<String, SharedBashState>,
    /// Last-use timestamps for LRU eviction.
    pub(super) last_used: HashMap<String, Instant>,
    /// In-flight operation count per session. A session whose count is > 0 (it
    /// has a live [`SessionGuard`]) is never chosen as an LRU eviction victim.
    pub(super) in_flight: HashMap<String, SessionPin>,
    /// Most recently addressed session, used as the fallback for tool calls
    /// that omit a `thread_id`.
    pub(super) last_active: Option<String>,
}

/// Lock-free pin counter for one session. A live [`SessionGuard`] keeps the
/// count `> 0`, which marks the session as in-flight so LRU eviction skips it.
/// Clones share the same counter — the registry hands a clone to each
/// concurrent call. Keep this discipline in lockstep with the loom model.
#[derive(Clone)]
pub(super) struct SessionPin {
    count: Arc<AtomicUsize>,
}

impl Default for SessionPin {
    fn default() -> Self {
        Self { count: Arc::new(AtomicUsize::new(0)) }
    }
}

impl SessionPin {
    fn acquire(&self) -> SessionGuard {
        self.count.fetch_add(1, Ordering::SeqCst);
        SessionGuard { count: self.count.clone() }
    }

    fn is_pinned(&self) -> bool {
        self.count.load(Ordering::SeqCst) > 0
    }
}

/// RAII marker that a session has an operation in flight. Bumps the session's
/// in-flight counter on creation and releases it from synchronous `Drop`.
pub(super) struct SessionGuard {
    count: Arc<AtomicUsize>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::SeqCst);
    }
}

/// How an empty `thread_id` is resolved by the session registry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionIsolation {
    /// Local stdio transport — a single client. An empty `thread_id` falls back
    /// to the last-active session (legacy single-client convenience).
    Lenient,
    /// Remote HTTP transport — multiple clients behind one shared bearer token.
    /// The last-active fallback is disabled, so an empty `thread_id` from one
    /// client can never resolve to another client's shell; such calls get a
    /// dedicated anonymous slot instead.
    ///
    /// Residual: two clients that deliberately send the same explicit
    /// `thread_id` still share a shell. Closing that needs per-client identities.
    Strict,
}

impl Default for WinxService {
    fn default() -> Self {
        Self::new()
    }
}

impl WinxService {
    /// Create a new `WinxService` for local stdio transport.
    pub fn new() -> Self {
        Self::with_isolation(SessionIsolation::Lenient)
    }

    /// Create a service with an explicit session-isolation policy. The HTTP
    /// transport uses [`SessionIsolation::Strict`].
    pub fn with_isolation(isolation: SessionIsolation) -> Self {
        Self::with_runtime(isolation, Arc::new(EmbeddedShellRuntime))
    }

    /// Create a service with an explicit shell-runtime boundary.
    pub fn with_runtime(isolation: SessionIsolation, shell_runtime: Arc<dyn ShellRuntime>) -> Self {
        info!(?isolation, "Creating new WinxService instance");
        Self {
            sessions: Arc::new(Mutex::new(SessionRegistry::default())),
            tasks: Arc::new(Mutex::new(TaskRegistry::default())),
            root_bootstrap: Arc::new(Mutex::new(())),
            shell_runtime,
            mutations: super::mutations::MutationCoordinator::default(),
            version: crate::build_info::display_version().to_string(),
            isolation,
        }
    }

    /// Resolve the session slot for a `thread_id`, creating it if absent.
    /// Marks the slot as most-recently-used and evicts the LRU idle session when
    /// adding a new key would exceed [`MAX_SESSIONS`].
    pub(super) async fn session_for(&self, thread_id: &str) -> (SharedBashState, SessionGuard) {
        let thread_id = crate::types::normalize_thread_id(thread_id);
        let mut registry = self.sessions.lock().await;
        let key = if thread_id.is_empty() {
            match self.isolation {
                SessionIsolation::Lenient => {
                    registry.last_active.clone().unwrap_or_else(|| "default".to_string())
                }
                SessionIsolation::Strict => "anonymous".to_string(),
            }
        } else {
            thread_id.clone()
        };

        if !registry.slots.contains_key(&key) && registry.slots.len() >= MAX_SESSIONS {
            let victim = registry
                .last_used
                .iter()
                .filter(|(candidate, _)| **candidate != key)
                .filter(|(candidate, _)| {
                    registry.in_flight.get(candidate.as_str()).is_none_or(|pin| !pin.is_pinned())
                })
                .min_by_key(|(_, last_used)| **last_used)
                .map(|(candidate, _)| candidate.clone());

            if let Some(victim) = victim {
                // The daemon runtime owns a process outside this registry. Release
                // it while the registry lock prevents the same thread_id from being
                // recreated underneath the termination request.
                if let Err(error) = self.shell_runtime.terminate_session(&victim).await {
                    warn!(%error, "failed to terminate LRU shell session '{victim}'");
                }
                registry.slots.remove(&victim);
                registry.last_used.remove(&victim);
                registry.in_flight.remove(&victim);
                if registry.last_active.as_deref() == Some(victim.as_str()) {
                    registry.last_active = None;
                }
                warn!("Evicted LRU shell session '{victim}' (session cap {MAX_SESSIONS})");
            } else {
                warn!(
                    "All {MAX_SESSIONS} sessions busy; exceeding the cap rather than evicting an in-flight session"
                );
            }
        }

        let slot =
            registry.slots.entry(key.clone()).or_insert_with(|| Arc::new(Mutex::new(None))).clone();
        let pin = registry.in_flight.entry(key.clone()).or_default().clone();
        registry.last_used.insert(key.clone(), Instant::now());
        if !thread_id.is_empty() {
            registry.last_active = Some(key);
        }
        (slot, pin.acquire())
    }

    /// [`Self::session_for`] plus transparent rehydration: the entry point for
    /// every tool EXCEPT `Initialize`. After an adapter restart the registry is
    /// empty, but the per-thread snapshot survives on disk and (daemon runtime)
    /// the guardian may still own the live PTY — so a tool call carrying a
    /// known `thread_id` self-heals here instead of failing with
    /// `BashStateNotInitialized` and burning a model round-trip on re-Initialize.
    ///
    /// `Initialize` deliberately keeps the raw [`Self::session_for`]: its
    /// recovery semantics (downgrading `reset_shell`/`user_asked_mode_change`
    /// to `first_call`) depend on observing the missing live state.
    pub(super) async fn tool_session_for(
        &self,
        thread_id: &str,
    ) -> (SharedBashState, SessionGuard) {
        let (slot, guard) = self.session_for(thread_id).await;
        self.rehydrate_slot_if_persisted(&slot, thread_id).await;
        (slot, guard)
    }

    /// Rebuild a missing adapter-side `BashState` from its persisted snapshot
    /// and attach-or-create the runtime session behind it. No-op when the slot
    /// is live, the `thread_id` is empty/anonymous, or nothing was persisted.
    /// Failure leaves the slot empty so callers fail exactly as before.
    async fn rehydrate_slot_if_persisted(&self, slot: &SharedBashState, thread_id: &str) {
        use crate::runtime::ShellSessionTransition;

        let thread_id = crate::types::normalize_thread_id(thread_id);
        if thread_id.is_empty() {
            return;
        }
        let mut state_guard = slot.lock().await;
        if state_guard.is_some() {
            return;
        }
        let snapshot = match crate::state::persistence::load_bash_state(&thread_id) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return,
            Err(error) => {
                warn!(%error, thread_id, "failed to read persisted session snapshot");
                return;
            }
        };

        let mut state = crate::state::bash_state::BashState::new();
        state.apply_snapshot(&snapshot);
        // The registry key is authoritative for which session this is.
        state.current_thread_id.clone_from(&thread_id);
        if !state.workspace_root.exists() {
            warn!(
                thread_id,
                workspace_root = %state.workspace_root.display(),
                "not rehydrating session: persisted workspace no longer exists"
            );
            return;
        }

        // FirstCall is attach-or-create in both runtimes: the daemon guardian
        // that survived the adapter restart hands back its authoritative
        // snapshot with the live PTY untouched; a dead guardian (or the
        // embedded runtime) gets a fresh PTY at the persisted cwd/mode.
        match self
            .shell_runtime
            .configure_session(&mut state, ShellSessionTransition::FirstCall)
            .await
        {
            Ok(configured) => {
                info!(
                    thread_id,
                    attached_existing = configured.attached_existing,
                    "rehydrated session from persisted state after adapter restart"
                );
                if !configured.attached_existing {
                    state.recovery_note = Some(lost_shell_recovery_note(&thread_id, &state));
                }
                *state_guard = Some(state);
            }
            Err(error) => {
                warn!(%error, thread_id, "failed to rehydrate persisted session");
            }
        }
    }

    /// The most recently active session slot, optionally confined to one HTTP
    /// principal's internal thread-id prefix.
    pub(super) async fn active_slot(
        &self,
        session_prefix: Option<&str>,
    ) -> Option<SharedBashState> {
        let registry = self.sessions.lock().await;
        if let Some(prefix) = session_prefix {
            return registry
                .last_used
                .iter()
                .filter(|(key, _)| key.starts_with(prefix))
                .max_by_key(|(_, last_used)| **last_used)
                .and_then(|(key, _)| registry.slots.get(key).cloned());
        }
        registry.last_active.as_ref().and_then(|key| registry.slots.get(key).cloned())
    }

    /// Read the immutable project identity of an already initialized adapter
    /// session without creating a slot or changing LRU state. Remote coherence
    /// checks use this before any tool can touch the shell or filesystem.
    pub(super) async fn bound_workspace(&self, thread_id: &str) -> Option<std::path::PathBuf> {
        let thread_id = crate::types::normalize_thread_id(thread_id);
        if thread_id.is_empty() {
            return None;
        }
        let slot = {
            let registry = self.sessions.lock().await;
            registry.slots.get(&thread_id).cloned()
        }?;
        let state = slot.lock().await;
        state.as_ref().filter(|state| state.initialized).map(|state| state.workspace_root.clone())
    }

    pub(super) async fn has_initialized_session(&self) -> bool {
        let slots = {
            let registry = self.sessions.lock().await;
            registry.slots.values().cloned().collect::<Vec<_>>()
        };
        for slot in slots {
            if slot.lock().await.as_ref().is_some_and(|state| state.initialized) {
                return true;
            }
        }
        false
    }

    /// Bootstrap a local single-client session from the client's first MCP Root.
    /// The negotiated MCP protocol still exposes Roots, although `rmcp` marks this
    /// compatibility API as deprecated in anticipation of the later SEP-2577 draft.
    #[allow(deprecated)]
    pub(super) async fn initialize_from_client_roots(
        &self,
        context: NotificationContext<RoleServer>,
    ) {
        if self.isolation != SessionIsolation::Lenient {
            return;
        }
        let _bootstrap_guard = self.root_bootstrap.lock().await;
        if self.has_initialized_session().await {
            return;
        }
        let supports_roots =
            context.peer.peer_info().is_some_and(|info| info.capabilities.roots.is_some());
        if !supports_roots {
            return;
        }
        let roots = match context.peer.list_roots().await {
            Ok(result) => result.roots,
            Err(error) => {
                warn!(%error, "client advertised Roots but roots/list failed");
                return;
            }
        };
        let cwd = std::env::current_dir().ok();
        let mut paths =
            roots.iter().filter_map(|root| root_uri_to_path(&root.uri)).collect::<Vec<_>>();
        let selected = cwd
            .as_ref()
            .and_then(|cwd| paths.iter().position(|path| cwd.starts_with(path)))
            .map(|index| paths.remove(index))
            .or_else(|| paths.into_iter().next());
        let Some(workspace) = selected else {
            warn!("client returned no usable local file Roots; Initialize remains required");
            return;
        };

        let initialize = Initialize {
            init_type: InitializeType::FirstCall,
            any_workspace_path: workspace.to_string_lossy().into_owned(),
            initial_files_to_read: Vec::new(),
            task_id_to_resume: String::new(),
            mode_name: ModeName::Wcgw,
            thread_id: generate_thread_id(),
            code_writer_config: None,
        };
        let arguments = match serde_json::to_value(initialize) {
            Ok(arguments) => arguments,
            Err(error) => {
                warn!(%error, "failed to serialize Roots bootstrap request");
                return;
            }
        };
        match self.handle_initialize(Some(arguments)).await {
            Ok(_) => info!(workspace = %workspace.display(), "initialized Winx from MCP Roots"),
            Err(error) => warn!(%error.message, "failed to initialize Winx from MCP Roots"),
        }
    }
}

/// The show-once note for a session whose live shell did NOT survive the
/// restart (a fresh PTY was created). When an interactive agent was running
/// in the lost PTY, include how to resume its conversation.
fn lost_shell_recovery_note(
    thread_id: &str,
    state: &crate::state::bash_state::BashState,
) -> String {
    let base = format!(
        "Recovered session {thread_id} from its persisted snapshot after a restart: mode, \
         permissions, and edit receipts were restored, and a fresh shell was started at {}.",
        state.cwd.display()
    );
    match crate::state::agent_resume::load_agent_session(thread_id) {
        Ok(Some(record)) => format!(
            "{base} The interactive `{}` session running before the restart did not survive — \
             resume its conversation with `{}` (cwd {}).",
            record.agent,
            record.resume_command(),
            record.cwd
        ),
        Ok(None) => base,
        Err(error) => {
            warn!(%error, thread_id, "failed to read agent-session record");
            base
        }
    }
}

pub(super) fn root_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    let encoded = if let Some(path) = encoded.strip_prefix("localhost/") {
        format!("/{path}")
    } else if encoded.starts_with('/') {
        encoded.to_string()
    } else {
        // Refuse non-local file authorities (file://host/path).
        return None;
    };
    let decoded = percent_encoding::percent_decode_str(&encoded).decode_utf8().ok()?;
    let path = std::path::PathBuf::from(decoded.as_ref());
    path.is_absolute().then_some(path)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod rehydration_tests {
    use std::sync::Arc;

    use super::SessionIsolation;
    use crate::errors::WinxError;
    use crate::runtime::{
        ShellRuntime, ShellRuntimeConfigureFuture, ShellRuntimeFuture, ShellRuntimeUnitFuture,
        ShellSessionConfiguration, ShellSessionTransition,
    };
    use crate::server::WinxService;
    use crate::state::agent_resume::{save_agent_session, AgentSessionRecord};
    use crate::state::bash_state::BashState;
    use crate::state::persistence::{delete_bash_state, save_bash_state};

    /// Runtime double: attach-or-create succeeds without a real PTY and
    /// reports whether the "live" session survived.
    struct StubRuntime {
        attached_existing: bool,
    }

    impl ShellRuntime for StubRuntime {
        fn configure_session<'a>(
            &'a self,
            _bash_state: &'a mut BashState,
            _transition: ShellSessionTransition,
        ) -> ShellRuntimeConfigureFuture<'a> {
            let attached_existing = self.attached_existing;
            Box::pin(async move {
                Ok(ShellSessionConfiguration { attach_hint: None, attached_existing })
            })
        }

        fn run_action<'a>(
            &'a self,
            _bash_state: &'a Arc<tokio::sync::Mutex<Option<BashState>>>,
            _command: crate::types::BashCommand,
        ) -> ShellRuntimeFuture<'a> {
            Box::pin(async { Err(WinxError::CommandExecutionError("stub".to_string())) })
        }

        fn interrupt<'a>(
            &'a self,
            _bash_state: &'a Arc<tokio::sync::Mutex<Option<BashState>>>,
        ) -> ShellRuntimeUnitFuture<'a> {
            Box::pin(async { Ok(()) })
        }

        fn terminate_session<'a>(&'a self, _thread_id: &'a str) -> ShellRuntimeUnitFuture<'a> {
            Box::pin(async { Ok(()) })
        }
    }

    fn service(attached_existing: bool) -> WinxService {
        WinxService::with_runtime(
            SessionIsolation::Strict,
            Arc::new(StubRuntime { attached_existing }),
        )
    }

    fn unique_thread_id(tag: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        // Alphanumeric + underscore only, so `normalize_thread_id` keeps the
        // id verbatim and the on-disk snapshot file matches.
        format!("winxtest_rehydrate_{tag}_{}_{nanos}", std::process::id())
    }

    fn persist_snapshot(thread_id: &str, workspace: &std::path::Path) {
        let mut state = BashState::new();
        state.current_thread_id = thread_id.to_string();
        state.workspace_root = workspace.to_path_buf();
        state.cwd = workspace.to_path_buf();
        state.initialized = true;
        save_bash_state(thread_id, &state.snapshot()).expect("persist snapshot");
    }

    #[tokio::test]
    async fn tool_call_rehydrates_a_persisted_session_transparently() {
        let workspace = tempfile::tempdir().expect("workspace");
        let thread_id = unique_thread_id("attach");
        persist_snapshot(&thread_id, workspace.path());

        // Fresh service = adapter restart: empty registry, snapshot on disk.
        let (slot, _guard) = service(true).tool_session_for(&thread_id).await;
        {
            let state = slot.lock().await;
            let state = state.as_ref().expect("session should rehydrate from disk");
            assert!(state.initialized);
            assert_eq!(state.current_thread_id, thread_id);
            assert_eq!(state.workspace_root, workspace.path());
            // The live session survived (guardian attach) — nothing to report.
            assert!(state.recovery_note.is_none());
        }
        delete_bash_state(&thread_id).ok();
    }

    #[tokio::test]
    async fn lost_shell_rehydration_surfaces_the_agent_resume_hint() {
        let workspace = tempfile::tempdir().expect("workspace");
        let thread_id = unique_thread_id("resume");
        persist_snapshot(&thread_id, workspace.path());
        save_agent_session(
            &thread_id,
            &AgentSessionRecord {
                agent: "claude".to_string(),
                command: "claude".to_string(),
                cwd: workspace.path().to_string_lossy().into_owned(),
                launched_at_unix_ms: 1,
            },
        )
        .expect("persist agent record");

        // attached_existing=false = the PTY did not survive (reboot / dead
        // guardian): the note must carry the resume command.
        let (slot, _guard) = service(false).tool_session_for(&thread_id).await;
        {
            let state = slot.lock().await;
            let note = state
                .as_ref()
                .expect("session should rehydrate")
                .recovery_note
                .clone()
                .expect("lost shell must set a recovery note");
            assert!(note.contains("claude --continue"), "note missing resume hint: {note}");
        }
        delete_bash_state(&thread_id).ok();
        crate::state::agent_resume::clear_agent_session(&thread_id).ok();
    }

    #[tokio::test]
    async fn without_a_persisted_snapshot_the_slot_stays_empty() {
        let thread_id = unique_thread_id("missing");
        let (slot, _guard) = service(true).tool_session_for(&thread_id).await;
        assert!(slot.lock().await.is_none(), "no snapshot on disk must not fabricate state");
    }

    #[tokio::test]
    async fn empty_thread_id_never_rehydrates() {
        let (slot, _guard) = service(true).tool_session_for("").await;
        assert!(slot.lock().await.is_none());
    }
}
