//! Background shell session lifecycle and shared manager state.

use rand::RngExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::info;

use crate::errors::{Result, WinxError};
use crate::state::pty::{PtyShell, SharedPtyShell};

/// Snapshot of a background shell that has exited but whose final output has not
/// yet been consumed by the caller. We keep it around so the next call (typically
/// a `status_check`) can return the trailing output before the entry is gone.
#[derive(Debug, Clone)]
pub struct ExitedShellInfo {
    pub owner_thread_id: String,
    pub last_command: String,
    pub final_output: Arc<str>,
    pub exit_code: Option<i32>,
    pub cwd: PathBuf,
    pub output_truncated: bool,
    pub scratch_path: Option<PathBuf>,
    pub exited_at: Instant,
}

#[derive(Debug)]
struct BackgroundShellEntry {
    owner_thread_id: String,
    shell: SharedPtyShell,
}

/// Manages background shell sessions - matches WCGW Python's `background_shells` dict
#[derive(Debug, Default)]
pub struct BackgroundShellManager {
    shells: HashMap<String, BackgroundShellEntry>,
    /// Recently exited shells that still owe their final output to the caller.
    /// Entries are consumed the first time the caller queries the id, then dropped.
    tombstones: HashMap<String, ExitedShellInfo>,
}

impl BackgroundShellManager {
    /// Tombstones older than this are garbage-collected on the next prune pass.
    const TOMBSTONE_TTL: Duration = Duration::from_secs(300);
    const MAX_SHELLS: usize = 32;
    const MAX_SHELLS_PER_THREAD: usize = 8;
    const MAX_TOMBSTONES: usize = 32;
    const MAX_TOMBSTONES_PER_THREAD: usize = 8;

    /// Create a new background shell manager
    pub fn new() -> Self {
        Self { shells: HashMap::new(), tombstones: HashMap::new() }
    }

    /// Start a new background shell and return its command ID
    /// Register an already-built shell and return its id.
    ///
    /// The caller builds the `PtyShell` BEFORE taking the manager lock —
    /// `PtyShell::new` forks+execs and does a ~300ms blocking prompt init, which
    /// must not run under the global `std::Mutex` (it would serialize every other
    /// background-shell op behind one slow spawn).
    pub fn register_shell(&mut self, owner_thread_id: &str, shell: PtyShell) -> Result<String> {
        self.prune_finished_shells();
        self.ensure_capacity(owner_thread_id)?;

        let cid = format!("{:016x}", rand::rng().random::<u64>());
        self.shells.insert(
            cid.clone(),
            BackgroundShellEntry {
                owner_thread_id: owner_thread_id.to_string(),
                shell: Arc::new(Mutex::new(Some(shell))),
            },
        );
        info!("Started background shell with id: {}", cid);
        Ok(cid)
    }

    fn ensure_capacity(&self, owner_thread_id: &str) -> Result<()> {
        let owned =
            self.shells.values().filter(|entry| entry.owner_thread_id == owner_thread_id).count();
        if owned >= Self::MAX_SHELLS_PER_THREAD {
            return Err(WinxError::CommandExecutionError(format!(
                "Background shell limit reached for this thread ({}). Reuse or stop an existing shell.",
                Self::MAX_SHELLS_PER_THREAD
            )));
        }
        if self.shells.len() >= Self::MAX_SHELLS {
            return Err(WinxError::CommandExecutionError(format!(
                "Global background shell limit reached ({}). Wait for an existing shell to exit.",
                Self::MAX_SHELLS
            )));
        }
        Ok(())
    }

    /// Get a background shell by its command ID
    pub fn get_shell(&self, owner_thread_id: &str, bg_command_id: &str) -> Option<SharedPtyShell> {
        self.shells
            .get(bg_command_id)
            .filter(|entry| entry.owner_thread_id == owner_thread_id)
            .map(|entry| entry.shell.clone())
    }

    /// Remove and cleanup a background shell
    pub fn remove_shell(&mut self, bg_command_id: &str) -> bool {
        if let Some(entry) = self.shells.remove(bg_command_id) {
            if let Ok(mut guard) = entry.shell.try_lock() {
                *guard = None;
            }
            info!("Removed background shell: {}", bg_command_id);
            true
        } else {
            false
        }
    }

    pub(crate) fn prune_finished_shells(&mut self) {
        // GC old tombstones first.
        let now = Instant::now();
        self.tombstones.retain(|_, info| now.duration_since(info.exited_at) < Self::TOMBSTONE_TTL);

        let mut finished: Vec<(String, Option<ExitedShellInfo>)> = Vec::new();

        for (id, entry) in &self.shells {
            // A routed status/screen/wait call owns another Arc while it reads this
            // shell. Never turn the shell into a tombstone underneath that caller:
            // it would observe `None` and return an empty final response while the
            // real output sat in the newly-created tombstone.
            if Arc::strong_count(&entry.shell) > 1 {
                continue;
            }
            let Ok(mut guard) = entry.shell.try_lock() else {
                continue;
            };

            let Some(shell) = guard.as_mut() else {
                finished.push((id.clone(), None));
                continue;
            };

            if !shell.is_alive() {
                let tombstone = Self::exited_shell_info(&entry.owner_thread_id, shell, now);
                finished.push((id.clone(), Some(tombstone)));
                continue;
            }

            // Never prune shells that haven't received a command yet.
            // The global BG_SHELL_MANAGER is shared across parallel tests; a freshly
            // spawned shell would otherwise be evicted between start_new_shell and
            // the first send_command, leading to "Failed to get background shell".
            if shell.last_command.is_empty() {
                continue;
            }

            if shell.command_running {
                // Non-blocking: draining here must NOT hold the global manager
                // lock across a 100ms blocking read (that serializes every other
                // background-shell op behind a single slow PTY).
                shell.poll_output_nonblocking();
            }

            if !shell.command_running {
                let tombstone = Self::exited_shell_info(&entry.owner_thread_id, shell, now);
                finished.push((id.clone(), Some(tombstone)));
            }
        }

        for (id, tombstone) in finished {
            self.remove_shell(&id);
            if let Some(info) = tombstone {
                self.insert_tombstone(id, info);
            }
        }
    }

    fn exited_shell_info(
        owner_thread_id: &str,
        shell: &mut PtyShell,
        now: Instant,
    ) -> ExitedShellInfo {
        ExitedShellInfo {
            owner_thread_id: owner_thread_id.to_string(),
            last_command: shell.last_command.clone(),
            final_output: Arc::from(std::mem::take(&mut shell.output_buffer)),
            exit_code: shell.last_exit_code,
            cwd: shell.current_cwd().to_path_buf(),
            output_truncated: shell.output_truncated,
            scratch_path: shell.scratch_path().map(Path::to_path_buf),
            exited_at: now,
        }
    }

    fn insert_tombstone(&mut self, id: String, info: ExitedShellInfo) {
        let owner = info.owner_thread_id.clone();
        self.tombstones.insert(id, info);

        while self.tombstones.values().filter(|entry| entry.owner_thread_id == owner).count()
            > Self::MAX_TOMBSTONES_PER_THREAD
        {
            let oldest = self
                .tombstones
                .iter()
                .filter(|(_, entry)| entry.owner_thread_id == owner)
                .min_by_key(|(_, entry)| entry.exited_at)
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                self.tombstones.remove(&id);
            }
        }

        while self.tombstones.len() > Self::MAX_TOMBSTONES {
            let oldest = self
                .tombstones
                .iter()
                .min_by_key(|(_, entry)| entry.exited_at)
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                self.tombstones.remove(&id);
            }
        }
    }

    /// Look up the tombstone for a recently-exited shell, if any.
    ///
    /// The entry stays in the map until the TTL expires (see
    /// `prune_finished_shells`), so repeated `status_check` calls on the same
    /// `bg_command_id` keep returning the cached final output instead of
    /// flipping to "shell not found" after the first read.
    pub fn peek_tombstone(
        &self,
        owner_thread_id: &str,
        bg_command_id: &str,
    ) -> Option<ExitedShellInfo> {
        self.tombstones
            .get(bg_command_id)
            .filter(|entry| entry.owner_thread_id == owner_thread_id)
            .cloned()
    }

    /// Get info about all running background shells - matches WCGW Python `get_bg_running_commandsinfo`
    pub fn get_running_info(&mut self, owner_thread_id: &str) -> String {
        self.prune_finished_shells();

        if !self.shells.values().any(|entry| entry.owner_thread_id == owner_thread_id) {
            return "No command running in background.\n".to_string();
        }

        let mut running = Vec::new();
        for (id, entry) in &self.shells {
            if entry.owner_thread_id != owner_thread_id {
                continue;
            }
            if let Ok(guard) = entry.shell.try_lock() {
                if let Some(bash) = guard.as_ref() {
                    if bash.command_running {
                        running
                            .push(format!("Command: {}, bg_command_id: {}", bash.last_command, id));
                    }
                }
            } else {
                running.push(format!("Command: <busy>, bg_command_id: {id}"));
            }
        }

        if running.is_empty() {
            "No command running in background.\n".to_string()
        } else {
            format!("Following background commands are attached:\n{}\n", running.join("\n"))
        }
    }
}

// Global background shell manager (thread-safe) - matches WCGW Python's BashState.background_shells
lazy_static::lazy_static! {
    static ref BG_SHELL_MANAGER: StdMutex<BackgroundShellManager> = StdMutex::new(BackgroundShellManager::new());
}

/// Lock the global background-shell manager, recovering from poisoning.
///
/// A panic while holding this lock (e.g. in the rendering path during a prune)
/// must NOT permanently brick all background-shell functionality for the rest of
/// the server's lifetime. The manager's data stays consistent across a panic, so
/// recovering the inner guard is safe.
pub(crate) fn lock_bg_manager() -> std::sync::MutexGuard<'static, BackgroundShellManager> {
    BG_SHELL_MANAGER.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::{BackgroundShellEntry, BackgroundShellManager, ExitedShellInfo};
    use crate::errors::WinxError;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::sync::Mutex;

    fn empty_entry(owner_thread_id: &str) -> BackgroundShellEntry {
        BackgroundShellEntry {
            owner_thread_id: owner_thread_id.to_string(),
            shell: Arc::new(Mutex::new(None)),
        }
    }

    fn tombstone(owner_thread_id: &str) -> ExitedShellInfo {
        ExitedShellInfo {
            owner_thread_id: owner_thread_id.to_string(),
            last_command: "true".to_string(),
            final_output: Arc::from("done"),
            exit_code: Some(0),
            cwd: PathBuf::from("/tmp"),
            output_truncated: false,
            scratch_path: None,
            exited_at: Instant::now(),
        }
    }

    #[test]
    fn background_capacity_is_bounded_per_thread_and_globally() {
        let mut manager = BackgroundShellManager::new();
        for index in 0..BackgroundShellManager::MAX_SHELLS_PER_THREAD {
            manager.shells.insert(format!("owned-{index}"), empty_entry("owner"));
        }
        assert!(matches!(
            manager.ensure_capacity("owner"),
            Err(WinxError::CommandExecutionError(message)) if message.contains("this thread")
        ));
        assert!(manager.ensure_capacity("another-owner").is_ok());

        for index in manager.shells.len()..BackgroundShellManager::MAX_SHELLS {
            manager
                .shells
                .insert(format!("global-{index}"), empty_entry(&format!("owner-{index}")));
        }
        assert!(matches!(
            manager.ensure_capacity("new-owner"),
            Err(WinxError::CommandExecutionError(message)) if message.contains("Global")
        ));
    }

    #[test]
    fn tombstone_capacity_is_bounded_per_thread_and_globally() {
        let mut manager = BackgroundShellManager::new();
        for index in 0..BackgroundShellManager::MAX_TOMBSTONES_PER_THREAD + 3 {
            manager.insert_tombstone(format!("owned-{index}"), tombstone("owner"));
        }
        assert_eq!(
            manager.tombstones.values().filter(|entry| entry.owner_thread_id == "owner").count(),
            BackgroundShellManager::MAX_TOMBSTONES_PER_THREAD
        );

        for index in 0..BackgroundShellManager::MAX_TOMBSTONES + 5 {
            manager.insert_tombstone(format!("global-{index}"), tombstone(&format!("o-{index}")));
        }
        assert_eq!(manager.tombstones.len(), BackgroundShellManager::MAX_TOMBSTONES);
    }
}
