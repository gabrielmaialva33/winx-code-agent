use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::client::DaemonClient;
use super::protocol::{HelloResult, PruneResult};
use crate::errors::{Result, WinxError};
use crate::runtime::ensure_daemon_at;

const DEFAULT_MAX_GUARDIANS: usize = 32;
const MAX_CONFIGURED_GUARDIANS: usize = 4096;
const DEFAULT_IDLE_TTL_SECS: u64 = 24 * 60 * 60;
const DEFAULT_SWEEP_INTERVAL_SECS: u64 = 60;
const MAX_SWEEP_INTERVAL_SECS: u64 = 24 * 60 * 60;
const GUARDIAN_STARTUP_GRACE: Duration = Duration::from_secs(30);
const TERMINATE_GRACE: Duration = Duration::from_millis(750);
const TERMINATE_POLL: Duration = Duration::from_millis(25);

#[derive(Clone, Debug)]
pub(crate) struct GuardianLimits {
    max_guardians: usize,
    idle_ttl: Option<Duration>,
    sweep_interval: Duration,
}

impl GuardianLimits {
    pub(crate) fn from_env() -> Result<Self> {
        let max_guardians = parse_usize_env(
            "WINX_MAX_GUARDIANS",
            DEFAULT_MAX_GUARDIANS,
            1,
            MAX_CONFIGURED_GUARDIANS,
        )?;
        let idle_ttl_secs =
            parse_u64_env("WINX_SESSION_IDLE_TTL_SECS", DEFAULT_IDLE_TTL_SECS, 0, u64::MAX)?;
        let sweep_interval_secs = parse_u64_env(
            "WINX_GUARDIAN_SWEEP_INTERVAL_SECS",
            DEFAULT_SWEEP_INTERVAL_SECS,
            1,
            MAX_SWEEP_INTERVAL_SECS,
        )?;
        Ok(Self {
            max_guardians,
            idle_ttl: (idle_ttl_secs > 0).then(|| Duration::from_secs(idle_ttl_secs)),
            sweep_interval: Duration::from_secs(sweep_interval_secs),
        })
    }

    #[cfg(test)]
    fn new(max_guardians: usize, idle_ttl: Option<Duration>) -> Self {
        Self { max_guardians, idle_ttl, sweep_interval: Duration::from_secs(1) }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct GuardianMetadata {
    thread_id: String,
    guardian_pid: u32,
    created_at_unix_ms: u64,
    last_seen_unix_ms: u64,
}

#[derive(Debug)]
pub(crate) struct GuardianLifecycle {
    guardian_dir: PathBuf,
    guardian_binary: PathBuf,
    limits: GuardianLimits,
    mutation_gate: Mutex<()>,
}

impl GuardianLifecycle {
    pub(crate) fn new(
        guardian_dir: PathBuf,
        guardian_binary: PathBuf,
        limits: GuardianLimits,
    ) -> Self {
        Self { guardian_dir, guardian_binary, limits, mutation_gate: Mutex::new(()) }
    }

    pub(crate) fn guardian_dir(&self) -> &Path {
        &self.guardian_dir
    }

    pub(crate) fn socket_for(&self, thread_id: &str) -> PathBuf {
        use sha2::{Digest, Sha256};
        use std::fmt::Write as _;

        let digest = Sha256::digest(thread_id.as_bytes());
        let mut name = String::with_capacity(24);
        for byte in &digest[..12] {
            let _ = write!(name, "{byte:02x}");
        }
        self.guardian_dir.join(format!("{name}.sock"))
    }

    pub(crate) fn spawn_sweeper(self: Arc<Self>) {
        if self.limits.idle_ttl.is_none() {
            return;
        }
        tokio::spawn(async move {
            let start = tokio::time::Instant::now() + self.limits.sweep_interval;
            let mut interval = tokio::time::interval_at(start, self.limits.sweep_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                match self.prune(None).await {
                    Ok(result) if !result.removed_thread_ids.is_empty() => tracing::info!(
                        removed = result.removed_thread_ids.len(),
                        "pruned idle Winx guardian sessions"
                    ),
                    Ok(_) => {}
                    Err(error) => tracing::warn!(%error, "guardian idle sweep failed"),
                }
            }
        });
    }

    pub(crate) async fn ensure_guardian(&self, thread_id: &str, socket: &Path) -> Result<()> {
        let _gate = self.mutation_gate.lock().await;
        if let Some(hello) = hello_with_retry(socket).await {
            self.record_activity(socket, thread_id, hello.daemon_pid).await?;
            return Ok(());
        }

        // Reclaim dead sockets and expired idle guardians before refusing a new
        // session. This keeps the hard quota useful without requiring manual
        // intervention for ordinary churn.
        let _ = self.prune_inner(None).await?;
        let live = self.live_guardian_count().await?;
        if live >= self.limits.max_guardians {
            return Err(WinxError::ConfigurationError(format!(
                "Winx guardian limit reached ({live}/{}). Reuse or kill an existing session, run \
                 `winx-code-agent prune`, or raise WINX_MAX_GUARDIANS.",
                self.limits.max_guardians
            )));
        }

        ensure_daemon_at(socket, &self.guardian_binary).await?;
        let hello = DaemonClient::new(socket).hello().await?;
        self.record_activity(socket, thread_id, hello.daemon_pid).await
    }

    pub(crate) async fn note_activity(&self, socket: &Path, thread_id: &str, guardian_pid: u32) {
        let _gate = self.mutation_gate.lock().await;
        match tokio::fs::try_exists(socket).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                tracing::warn!(socket = %socket.display(), %error, "cannot inspect guardian socket");
                return;
            }
        }
        if let Err(error) = self.record_activity(socket, thread_id, guardian_pid).await {
            tracing::warn!(
                socket = %socket.display(),
                %error,
                "failed to persist guardian activity metadata"
            );
        }
    }

    pub(crate) async fn finish_kill(&self, socket: &Path, guardian_pid: u32) -> Result<()> {
        let _gate = self.mutation_gate.lock().await;
        self.terminate_guardian_with_pid(socket, guardian_pid).await
    }

    pub(crate) async fn prune(&self, idle_seconds: Option<u64>) -> Result<PruneResult> {
        let _gate = self.mutation_gate.lock().await;
        self.prune_inner(idle_seconds).await
    }

    async fn prune_inner(&self, idle_seconds: Option<u64>) -> Result<PruneResult> {
        let idle_ttl = idle_seconds.map(Duration::from_secs).or(self.limits.idle_ttl);
        let now = unix_ms();
        let mut result = PruneResult::default();

        for socket in guardian_sockets(&self.guardian_dir).await? {
            let metadata = self.read_metadata(&socket).await;
            let Some(hello) = hello_with_retry(&socket).await else {
                if metadata.as_ref().is_some_and(|meta| process_exists(meta.guardian_pid)) {
                    result.unreachable_guardian_count += 1;
                    continue;
                }
                self.remove_artifacts(&socket).await;
                result.stale_socket_count += 1;
                continue;
            };

            let sessions = match DaemonClient::new(&socket).list_sessions().await {
                Ok(sessions) => sessions,
                Err(error) => {
                    tracing::warn!(socket = %socket.display(), %error, "cannot inspect guardian");
                    result.unreachable_guardian_count += 1;
                    continue;
                }
            };
            let thread_id = sessions
                .first()
                .map(|session| session.thread_id.clone())
                .or_else(|| metadata.as_ref().map(|meta| meta.thread_id.clone()))
                .unwrap_or_else(|| socket.display().to_string());

            // Guardians from releases before lifecycle metadata have no reliable
            // last-activity timestamp. On automatic/default pruning, seed them at
            // first observation instead of treating the socket creation time as
            // idle time and unexpectedly removing a recently used legacy session.
            // An explicit `--idle-seconds` override remains authoritative.
            if metadata.is_none() && !sessions.is_empty() && idle_seconds.is_none() {
                self.record_observed(&socket, &thread_id, hello.daemon_pid, None).await?;
                continue;
            }

            let active = sessions
                .iter()
                .any(|session| session.running || !session.background_command_ids.is_empty());
            let socket_timestamp = socket_modified_unix_ms(&socket);
            let created_at = metadata
                .as_ref()
                .map(|meta| meta.created_at_unix_ms)
                .or(socket_timestamp)
                .unwrap_or(now);
            let last_seen = metadata
                .as_ref()
                .map(|meta| meta.last_seen_unix_ms)
                .or(socket_timestamp)
                .unwrap_or(now);
            let expired =
                idle_ttl.is_some_and(|ttl| now.saturating_sub(last_seen) >= duration_ms(ttl));
            let starting = sessions.is_empty()
                && now.saturating_sub(created_at) < duration_ms(GUARDIAN_STARTUP_GRACE);

            // The control plane spawns a guardian immediately before relaying its
            // first configure call. A concurrent manual/automatic prune can observe
            // that brief empty state, so preserve young guardians until the relay
            // either initializes them or the startup grace expires.
            if starting {
                self.record_observed(&socket, &thread_id, hello.daemon_pid, metadata.as_ref())
                    .await?;
                continue;
            }

            if sessions.is_empty() || (!active && expired) {
                let mut safe_to_terminate = true;
                for session in &sessions {
                    if let Err(error) =
                        DaemonClient::new(&socket).kill_session(&session.thread_id).await
                    {
                        tracing::warn!(
                            socket = %socket.display(),
                            session = %session.thread_id,
                            %error,
                            "refusing to terminate guardian after session cleanup failed"
                        );
                        result.unreachable_guardian_count += 1;
                        safe_to_terminate = false;
                        break;
                    }
                }
                if !safe_to_terminate {
                    continue;
                }
                self.terminate_guardian_with_pid(&socket, hello.daemon_pid).await?;
                result.removed_thread_ids.push(thread_id);
            } else {
                self.record_observed(&socket, &thread_id, hello.daemon_pid, metadata.as_ref())
                    .await?;
                if active && expired {
                    result.skipped_active_thread_ids.push(thread_id);
                }
            }
        }

        result.removed_thread_ids.sort();
        result.skipped_active_thread_ids.sort();
        Ok(result)
    }

    async fn live_guardian_count(&self) -> Result<usize> {
        let mut live = 0;
        for socket in guardian_sockets(&self.guardian_dir).await? {
            if hello_with_retry(&socket).await.is_some() {
                live += 1;
                continue;
            }
            let metadata = self.read_metadata(&socket).await;
            if metadata.as_ref().is_some_and(|meta| process_exists(meta.guardian_pid)) {
                live += 1;
            } else {
                self.remove_artifacts(&socket).await;
            }
        }
        Ok(live)
    }

    async fn terminate_guardian_with_pid(&self, socket: &Path, pid: u32) -> Result<()> {
        let pid_i32 = i32::try_from(pid).map_err(|_| {
            WinxError::ConfigurationError(format!("guardian pid {pid} does not fit in pid_t"))
        })?;
        signal_process(pid_i32, libc::SIGTERM)?;
        if !wait_for_process_exit(pid, TERMINATE_GRACE).await {
            signal_process(pid_i32, libc::SIGKILL)?;
            if !wait_for_process_exit(pid, TERMINATE_GRACE).await {
                return Err(WinxError::CommandExecutionError(format!(
                    "guardian process {pid} did not exit after SIGTERM/SIGKILL"
                )));
            }
        }
        self.remove_artifacts(socket).await;
        Ok(())
    }

    async fn record_activity(
        &self,
        socket: &Path,
        thread_id: &str,
        guardian_pid: u32,
    ) -> Result<()> {
        let existing = self.read_metadata(socket).await;
        let now = unix_ms();
        let metadata = GuardianMetadata {
            thread_id: thread_id.to_string(),
            guardian_pid,
            created_at_unix_ms: existing.as_ref().map_or(now, |meta| meta.created_at_unix_ms),
            last_seen_unix_ms: now,
        };
        self.write_metadata(socket, &metadata).await
    }

    async fn record_observed(
        &self,
        socket: &Path,
        thread_id: &str,
        guardian_pid: u32,
        existing: Option<&GuardianMetadata>,
    ) -> Result<()> {
        if existing
            .is_some_and(|meta| meta.thread_id == thread_id && meta.guardian_pid == guardian_pid)
        {
            return Ok(());
        }
        let now = unix_ms();
        let metadata = GuardianMetadata {
            thread_id: thread_id.to_string(),
            guardian_pid,
            created_at_unix_ms: existing.map_or(now, |meta| meta.created_at_unix_ms),
            last_seen_unix_ms: existing.map_or(now, |meta| meta.last_seen_unix_ms),
        };
        self.write_metadata(socket, &metadata).await
    }

    async fn read_metadata(&self, socket: &Path) -> Option<GuardianMetadata> {
        let path = metadata_path(socket);
        let data = match tokio::fs::read(&path).await {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "cannot read guardian metadata");
                return None;
            }
        };
        match serde_json::from_slice(&data) {
            Ok(metadata) => Some(metadata),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "invalid guardian metadata");
                None
            }
        }
    }

    async fn write_metadata(&self, socket: &Path, metadata: &GuardianMetadata) -> Result<()> {
        let path = metadata_path(socket);
        let temp = path.with_extension(format!("json.tmp-{:016x}", rand::random::<u64>()));
        let data = serde_json::to_vec(metadata)
            .map_err(|error| WinxError::SerializationError(error.to_string()))?;
        tokio::fs::write(&temp, data).await?;
        tokio::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600)).await?;
        tokio::fs::rename(&temp, &path).await?;
        Ok(())
    }

    async fn remove_artifacts(&self, socket: &Path) {
        for path in [socket.to_path_buf(), metadata_path(socket)] {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => tracing::warn!(path = %path.display(), %error, "cleanup failed"),
            }
        }
    }
}

async fn guardian_sockets(guardian_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut sockets = Vec::new();
    let mut entries = tokio::fs::read_dir(guardian_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("sock") {
            sockets.push(path);
        }
    }
    sockets.sort();
    Ok(sockets)
}

async fn hello_with_retry(socket: &Path) -> Option<HelloResult> {
    let client = DaemonClient::new(socket);
    for attempt in 0..3 {
        if let Ok(hello) = client.hello().await {
            return Some(hello);
        }
        if attempt < 2 {
            tokio::time::sleep(TERMINATE_POLL).await;
        }
    }
    None
}

async fn wait_for_process_exit(pid: u32, budget: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    while tokio::time::Instant::now() < deadline {
        if !process_exists(pid) {
            return true;
        }
        tokio::time::sleep(TERMINATE_POLL).await;
    }
    !process_exists(pid)
}

fn signal_process(pid: i32, signal: i32) -> Result<()> {
    // SAFETY: pid and signal are plain values; kill(2) has no pointer preconditions.
    if unsafe { libc::kill(pid, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error.into())
    }
}

fn process_exists(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else { return false };
    // SAFETY: signal 0 performs permission/existence checking only.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn metadata_path(socket: &Path) -> PathBuf {
    socket.with_extension("json")
}

fn socket_modified_unix_ms(socket: &Path) -> Option<u64> {
    std::fs::metadata(socket)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn parse_usize_env(name: &str, default: usize, min: usize, max: usize) -> Result<usize> {
    let Some(value) = std::env::var_os(name) else { return Ok(default) };
    let text = value.to_string_lossy();
    let parsed = text.parse::<usize>().map_err(|error| {
        WinxError::ConfigurationError(format!("invalid {name}={text:?}: {error}"))
    })?;
    if !(min..=max).contains(&parsed) {
        return Err(WinxError::ConfigurationError(format!(
            "invalid {name}={parsed}; expected a value in {min}..={max}"
        )));
    }
    Ok(parsed)
}

fn parse_u64_env(name: &str, default: u64, min: u64, max: u64) -> Result<u64> {
    let Some(value) = std::env::var_os(name) else { return Ok(default) };
    let text = value.to_string_lossy();
    let parsed = text.parse::<u64>().map_err(|error| {
        WinxError::ConfigurationError(format!("invalid {name}={text:?}: {error}"))
    })?;
    if !(min..=max).contains(&parsed) {
        return Err(WinxError::ConfigurationError(format!(
            "invalid {name}={parsed}; expected a value in {min}..={max}"
        )));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guardian_socket_is_stable_and_workspace_local() {
        let limits = GuardianLimits::new(2, Some(Duration::from_secs(60)));
        let lifecycle = GuardianLifecycle::new(
            PathBuf::from("/tmp/winx-test-guardians"),
            PathBuf::from("/bin/false"),
            limits,
        );
        let first = lifecycle.socket_for("thread-a");
        let second = lifecycle.socket_for("thread-a");
        let other = lifecycle.socket_for("thread-b");
        assert_eq!(first, second);
        assert_ne!(first, other);
        assert_eq!(first.parent(), Some(lifecycle.guardian_dir()));
    }
}
