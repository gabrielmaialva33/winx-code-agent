//! Embedded session index used by the shell runtime.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex as StdMutex, Weak};

use crate::errors::Result;
use crate::state::pty::{PtyShell, SharedPtyShell};
use crate::tools::background_shell::{BackgroundShellManager, ExitedShellInfo};

/// Stable address of a terminal within one logical session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShellTarget {
    Main,
    Background(String),
}

/// Single index for main and background terminals in the embedded runtime.
///
/// Main shells remain owned by `BashState`; weak references keep this index from
/// extending their lifetime. Background ownership stays in the existing manager
/// until the daemon runtime takes ownership in a later migration step.
#[derive(Debug, Default)]
pub struct SessionStore {
    mains: HashMap<String, Weak<tokio::sync::Mutex<Option<PtyShell>>>>,
    operation_barriers: HashMap<String, Weak<tokio::sync::RwLock<()>>>,
    backgrounds: BackgroundShellManager,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind_main(&mut self, thread_id: &str, shell: &SharedPtyShell) {
        self.mains.retain(|_, shell| shell.strong_count() > 0);
        self.operation_barriers.retain(|_, barrier| barrier.strong_count() > 0);
        self.mains.insert(thread_id.to_string(), Arc::downgrade(shell));
    }

    /// Session-wide ordering barrier. Shell actions and interrupts hold a read
    /// guard while reset/configure holds a write guard, so an incarnation can
    /// never change between execution-token validation and the PTY operation.
    pub(crate) fn operation_barrier(&mut self, thread_id: &str) -> Arc<tokio::sync::RwLock<()>> {
        if let Some(barrier) =
            self.operation_barriers.get(thread_id).and_then(std::sync::Weak::upgrade)
        {
            return barrier;
        }
        let barrier = Arc::new(tokio::sync::RwLock::new(()));
        self.operation_barriers.insert(thread_id.to_string(), Arc::downgrade(&barrier));
        barrier
    }

    pub fn resolve(&self, thread_id: &str, target: &ShellTarget) -> Option<SharedPtyShell> {
        match target {
            ShellTarget::Main => self.mains.get(thread_id).and_then(Weak::upgrade),
            ShellTarget::Background(id) => self.backgrounds.get_shell(thread_id, id),
        }
    }

    pub(crate) fn register_shell(&mut self, thread_id: &str, shell: PtyShell) -> Result<String> {
        self.backgrounds.register_shell(thread_id, shell)
    }

    pub(crate) fn get_shell(&self, thread_id: &str, bg_command_id: &str) -> Option<SharedPtyShell> {
        self.backgrounds.get_shell(thread_id, bg_command_id)
    }

    pub(crate) fn remove_shell(&mut self, bg_command_id: &str) -> bool {
        self.backgrounds.remove_shell(bg_command_id)
    }

    pub(crate) fn prune_finished_shells(&mut self) {
        self.backgrounds.prune_finished_shells();
    }

    pub(crate) fn peek_tombstone(
        &self,
        thread_id: &str,
        bg_command_id: &str,
    ) -> Option<ExitedShellInfo> {
        self.backgrounds.peek_tombstone(thread_id, bg_command_id)
    }

    pub(crate) fn get_running_info(&mut self, thread_id: &str) -> String {
        self.backgrounds.get_running_info(thread_id)
    }
}

static EMBEDDED_SESSION_STORE: LazyLock<StdMutex<SessionStore>> =
    LazyLock::new(|| StdMutex::new(SessionStore::new()));

/// Lock the embedded session index, recovering from poisoning like the legacy
/// background manager did.
pub(crate) fn lock_session_store() -> std::sync::MutexGuard<'static, SessionStore> {
    EMBEDDED_SESSION_STORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
