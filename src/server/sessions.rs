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
            version: env!("CARGO_PKG_VERSION").to_string(),
            isolation,
        }
    }

    /// Resolve the session slot for a `thread_id`, creating it if absent.
    /// Marks the slot as most-recently-used and evicts the LRU idle session when
    /// adding a new key would exceed [`MAX_SESSIONS`].
    pub(super) async fn session_for(&self, thread_id: &str) -> (SharedBashState, SessionGuard) {
        let mut registry = self.sessions.lock().await;
        let key = if thread_id.is_empty() {
            match self.isolation {
                SessionIsolation::Lenient => {
                    registry.last_active.clone().unwrap_or_else(|| "default".to_string())
                }
                SessionIsolation::Strict => "anonymous".to_string(),
            }
        } else {
            thread_id.to_string()
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

    /// The most recently active session slot, without creating one.
    pub(super) async fn active_slot(&self) -> Option<SharedBashState> {
        let registry = self.sessions.lock().await;
        registry.last_active.as_ref().and_then(|key| registry.slots.get(key).cloned())
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
