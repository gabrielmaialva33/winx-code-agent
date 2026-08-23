use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::client::DaemonClient;
use super::protocol::{HelloResult, PruneResult, SessionInfo};
use crate::errors::{Result, WinxError};
use crate::runtime::ensure_daemon_at;

const DEFAULT_MAX_GUARDIANS: usize = 32;
const MAX_CONFIGURED_GUARDIANS: usize = 4096;
const DEFAULT_IDLE_TTL_SECS: u64 = 24 * 60 * 60;
const DEFAULT_UNUSED_IDLE_TTL_SECS: u64 = 30 * 60;
const DEFAULT_SWEEP_INTERVAL_SECS: u64 = 60;
const MAX_SWEEP_INTERVAL_SECS: u64 = 24 * 60 * 60;
const GUARDIAN_STARTUP_GRACE: Duration = Duration::from_secs(30);
const TERMINATE_GRACE: Duration = Duration::from_millis(750);
const TERMINATE_POLL: Duration = Duration::from_millis(25);

#[derive(Clone, Debug)]
pub(crate) struct GuardianLimits {
    max_guardians: usize,
    idle_ttl: Option<Duration>,
    unused_idle_ttl: Option<Duration>,
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
        let unused_idle_ttl_secs = parse_u64_env(
            "WINX_UNUSED_SESSION_IDLE_TTL_SECS",
            DEFAULT_UNUSED_IDLE_TTL_SECS,
            0,
            u64::MAX,
        )?;
        let sweep_interval_secs = parse_u64_env(
            "WINX_GUARDIAN_SWEEP_INTERVAL_SECS",
            DEFAULT_SWEEP_INTERVAL_SECS,
            1,
            MAX_SWEEP_INTERVAL_SECS,
        )?;
        Ok(Self {
            max_guardians,
            idle_ttl: (idle_ttl_secs > 0).then(|| Duration::from_secs(idle_ttl_secs)),
            unused_idle_ttl: (unused_idle_ttl_secs > 0)
                .then(|| Duration::from_secs(unused_idle_ttl_secs)),
            sweep_interval: Duration::from_secs(sweep_interval_secs),
        })
    }

    #[cfg(test)]
    fn new(
        max_guardians: usize,
        idle_ttl: Option<Duration>,
        unused_idle_ttl: Option<Duration>,
    ) -> Self {
        Self { max_guardians, idle_ttl, unused_idle_ttl, sweep_interval: Duration::from_secs(1) }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct GuardianMetadata {
    thread_id: String,
    guardian_pid: u32,
    created_at_unix_ms: u64,
    last_seen_unix_ms: u64,
    /// Distinguishes a real adapter request from passive metadata reconstruction.
    #[serde(default)]
    activity_observed: bool,
}

#[derive(Debug)]
struct GuardianObservation {
    socket: PathBuf,
    hello: HelloResult,
    metadata: Option<GuardianMetadata>,
    sessions: Vec<super::protocol::SessionInfo>,
    thread_id: String,
    active: bool,
    ever_ran_command: bool,
    created_at_unix_ms: u64,
    last_activity_unix_ms: u64,
    starting: bool,
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
        if self.limits.idle_ttl.is_none() && self.limits.unused_idle_ttl.is_none() {
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

    pub(crate) async fn ensure_guardian(
        &self,
        thread_id: &str,
        socket: &Path,
    ) -> Result<HelloResult> {
        let _gate = self.mutation_gate.lock().await;
        if let Some(hello) = hello_with_retry(socket).await {
            self.record_activity(socket, thread_id, hello.daemon_pid).await?;
            return Ok(hello);
        }

        // Reclaim dead sockets and expired idle guardians before refusing a new
        // session. This keeps the hard quota useful without requiring manual
        // intervention for ordinary churn.
        let _ = self.prune_inner(None).await?;
        let mut live = self.live_guardian_count().await?;
        while live >= self.limits.max_guardians {
            if !self.reclaim_oldest_unused_guardian().await? {
                break;
            }
            live = self.live_guardian_count().await?;
        }
        if live >= self.limits.max_guardians {
            return Err(WinxError::ConfigurationError(format!(
                "Winx guardian limit reached ({live}/{}). Reuse or kill an existing session, \
                 run `winx-code-agent prune --idle-seconds 0`, or raise WINX_MAX_GUARDIANS.",
                self.limits.max_guardians
            )));
        }

        ensure_daemon_at(socket, &self.guardian_binary).await?;
        let hello = DaemonClient::new(socket).hello().await?;
        self.record_activity(socket, thread_id, hello.daemon_pid).await?;
        Ok(hello)
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
        let now = unix_ms();
        let mut result = PruneResult::default();

        for socket in guardian_sockets(&self.guardian_dir).await? {
            let Some(observation) = self.inspect_guardian(&socket, now, &mut result).await? else {
                continue;
            };

            if observation.starting {
                self.record_observed(
                    &observation.socket,
                    &observation.thread_id,
                    observation.hello.daemon_pid,
                    observation.metadata.as_ref(),
                    observation.created_at_unix_ms,
                    observation.last_activity_unix_ms,
                )
                .await?;
                continue;
            }

            let idle_ttl =
                idle_seconds.map(Duration::from_secs).or(if observation.ever_ran_command {
                    self.limits.idle_ttl
                } else {
                    self.limits.unused_idle_ttl
                });
            let expired = idle_ttl.is_some_and(|ttl| {
                now.saturating_sub(observation.last_activity_unix_ms) >= duration_ms(ttl)
            });

            if observation.sessions.is_empty() || (!observation.active && expired) {
                let _ = self.terminate_observation(&observation, &mut result).await?;
            } else {
                self.record_observed(
                    &observation.socket,
                    &observation.thread_id,
                    observation.hello.daemon_pid,
                    observation.metadata.as_ref(),
                    observation.created_at_unix_ms,
                    observation.last_activity_unix_ms,
                )
                .await?;
                if observation.active && expired {
                    result.skipped_active_thread_ids.push(observation.thread_id);
                }
            }
        }

        result.removed_thread_ids.sort();
        result.skipped_active_thread_ids.sort();
        Ok(result)
    }

    async fn inspect_guardian(
        &self,
        socket: &Path,
        now: u64,
        result: &mut PruneResult,
    ) -> Result<Option<GuardianObservation>> {
        let metadata = self.read_metadata(socket).await;
        let Some(hello) = hello_with_retry(socket).await else {
            if metadata.as_ref().is_some_and(|meta| process_exists(meta.guardian_pid)) {
                result.unreachable_guardian_count += 1;
                return Ok(None);
            }
            self.remove_artifacts(socket).await;
            result.stale_socket_count += 1;
            return Ok(None);
        };

        let sessions = match DaemonClient::new(socket).list_sessions().await {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::warn!(socket = %socket.display(), %error, "cannot inspect guardian");
                result.unreachable_guardian_count += 1;
                return Ok(None);
            }
        };
        let thread_id = sessions
            .first()
            .map(|session| session.thread_id.clone())
            .or_else(|| metadata.as_ref().map(|meta| meta.thread_id.clone()))
            .unwrap_or_else(|| socket.display().to_string());
        let active = sessions
            .iter()
            .any(|session| session.running || !session.background_command_ids.is_empty());
        let socket_timestamp = socket_modified_unix_ms(socket);
        let (ever_ran_command, created_at_unix_ms, last_activity_unix_ms) =
            derive_activity_clock(&sessions, metadata.as_ref(), socket_timestamp, now);
        let starting = sessions.is_empty()
            && now.saturating_sub(created_at_unix_ms) < duration_ms(GUARDIAN_STARTUP_GRACE);

        Ok(Some(GuardianObservation {
            socket: socket.to_path_buf(),
            hello,
            metadata,
            sessions,
            thread_id,
            active,
            ever_ran_command,
            created_at_unix_ms,
            last_activity_unix_ms,
            starting,
        }))
    }

    async fn terminate_observation(
        &self,
        observation: &GuardianObservation,
        result: &mut PruneResult,
    ) -> Result<bool> {
        for session in &observation.sessions {
            if let Err(error) =
                DaemonClient::new(&observation.socket).kill_session(&session.thread_id).await
            {
                tracing::warn!(
                    socket = %observation.socket.display(),
                    session = %session.thread_id,
                    %error,
                    "refusing to terminate guardian after session cleanup failed"
                );
                result.unreachable_guardian_count += 1;
                return Ok(false);
            }
        }
        self.terminate_guardian_with_pid(&observation.socket, observation.hello.daemon_pid).await?;
        result.removed_thread_ids.push(observation.thread_id.clone());
        Ok(true)
    }

    async fn reclaim_oldest_unused_guardian(&self) -> Result<bool> {
        let now = unix_ms();
        let mut scan = PruneResult::default();
        let mut candidates = Vec::new();
        for socket in guardian_sockets(&self.guardian_dir).await? {
            let Some(observation) = self.inspect_guardian(&socket, now, &mut scan).await? else {
                continue;
            };
            if !observation.active && !observation.ever_ran_command && !observation.starting {
                candidates.push(observation);
            }
        }
        candidates.sort_by_key(|observation| observation.last_activity_unix_ms);

        for observation in candidates {
            let mut reclaimed = PruneResult::default();
            if self.terminate_observation(&observation, &mut reclaimed).await? {
                tracing::warn!(
                    session = %observation.thread_id,
                    "reclaimed an unused guardian under quota pressure"
                );
                return Ok(true);
            }
        }
        Ok(false)
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
            activity_observed: true,
        };
        self.write_metadata(socket, &metadata).await
    }

    async fn record_observed(
        &self,
        socket: &Path,
        thread_id: &str,
        guardian_pid: u32,
        existing: Option<&GuardianMetadata>,
        created_at_unix_ms: u64,
        last_seen_unix_ms: u64,
    ) -> Result<()> {
        if existing.is_some_and(|meta| {
            meta.thread_id == thread_id
                && meta.guardian_pid == guardian_pid
                && meta.created_at_unix_ms == created_at_unix_ms
                && meta.last_seen_unix_ms == last_seen_unix_ms
        }) {
            return Ok(());
        }
        let metadata = GuardianMetadata {
            thread_id: thread_id.to_string(),
            guardian_pid,
            created_at_unix_ms,
            last_seen_unix_ms,
            activity_observed: existing.is_some_and(|meta| meta.activity_observed),
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
    crate::os::unix::signal_raw(pid, signal).map_err(Into::into)
}

fn process_exists(pid: u32) -> bool {
    crate::os::unix::process_exists(pid)
}

fn metadata_path(socket: &Path) -> PathBuf {
    socket.with_extension("json")
}

fn derive_activity_clock(
    sessions: &[SessionInfo],
    metadata: Option<&GuardianMetadata>,
    socket_timestamp: Option<u64>,
    now: u64,
) -> (bool, u64, u64) {
    let ever_ran_command = sessions.iter().any(|session| {
        session.ever_ran_command
            || session.command_id.is_some()
            || session.last_command_at_unix_ms.is_some()
            || !session.background_command_ids.is_empty()
    });
    let guardian_created = sessions.iter().filter_map(|session| session.created_at_unix_ms).min();
    let guardian_activity =
        sessions.iter().filter_map(|session| session.last_activity_unix_ms).max();
    let created_at_unix_ms = guardian_created
        .or(socket_timestamp)
        .or_else(|| metadata.map(|meta| meta.created_at_unix_ms))
        .unwrap_or(now);
    let last_activity_unix_ms = guardian_activity.unwrap_or_else(|| {
        if ever_ran_command {
            // Legacy guardians cannot report activity. Preserve a used shell
            // unless the control plane has observed a real request since the
            // metadata cache was created.
            metadata.map_or(now, |meta| meta.last_seen_unix_ms)
        } else {
            // For a never-used legacy guardian the socket birth time is the
            // best available source. Prefer it over tmpfs metadata that may
            // have been recreated en masse when winxd restarted.
            let observed_activity = metadata
                .filter(|meta| {
                    meta.activity_observed || meta.last_seen_unix_ms > meta.created_at_unix_ms
                })
                .map(|meta| meta.last_seen_unix_ms);
            socket_timestamp
                .into_iter()
                .chain(observed_activity)
                .max()
                .or_else(|| metadata.map(|meta| meta.created_at_unix_ms))
                .unwrap_or(created_at_unix_ms)
        }
    });
    (ever_ran_command, created_at_unix_ms, last_activity_unix_ms)
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

    fn session_info() -> SessionInfo {
        SessionInfo {
            thread_id: "legacy".to_string(),
            cwd: "/tmp".to_string(),
            shell_pid: Some(7),
            command_id: None,
            running: false,
            background_command_ids: Vec::new(),
            created_at_unix_ms: None,
            last_activity_unix_ms: None,
            last_command_at_unix_ms: None,
            ever_ran_command: false,
        }
    }

    #[test]
    fn legacy_unused_session_prefers_socket_age_over_reseeded_metadata() {
        let sessions = [session_info()];
        let metadata = GuardianMetadata {
            thread_id: "legacy".to_string(),
            guardian_pid: 7,
            created_at_unix_ms: 900_000,
            last_seen_unix_ms: 900_000,
            activity_observed: false,
        };
        let (ever_ran, created, last_activity) =
            derive_activity_clock(&sessions, Some(&metadata), Some(100_000), 1_000_000);
        assert!(!ever_ran);
        assert_eq!(created, 100_000);
        assert_eq!(last_activity, 100_000);
    }

    #[test]
    fn legacy_unused_session_keeps_real_post_seed_activity() {
        let sessions = [session_info()];
        let metadata = GuardianMetadata {
            thread_id: "legacy".to_string(),
            guardian_pid: 7,
            created_at_unix_ms: 100_000,
            last_seen_unix_ms: 850_000,
            activity_observed: true,
        };
        let (ever_ran, _, last_activity) =
            derive_activity_clock(&sessions, Some(&metadata), Some(100_000), 1_000_000);
        assert!(!ever_ran);
        assert_eq!(last_activity, 850_000);
    }

    #[test]
    fn legacy_used_session_uses_control_observation_as_safe_fallback() {
        let mut session = session_info();
        session.command_id = Some("command".to_string());
        let metadata = GuardianMetadata {
            thread_id: "legacy".to_string(),
            guardian_pid: 7,
            created_at_unix_ms: 100_000,
            last_seen_unix_ms: 850_000,
            activity_observed: true,
        };
        let (ever_ran, _, last_activity) =
            derive_activity_clock(&[session], Some(&metadata), Some(100_000), 1_000_000);
        assert!(ever_ran);
        assert_eq!(last_activity, 850_000);
    }

    #[test]
    fn guardian_activity_clock_overrides_tmpfs_metadata() {
        let mut session = session_info();
        session.created_at_unix_ms = Some(200_000);
        session.last_activity_unix_ms = Some(700_000);
        session.last_command_at_unix_ms = Some(650_000);
        session.ever_ran_command = true;
        let metadata = GuardianMetadata {
            thread_id: "legacy".to_string(),
            guardian_pid: 7,
            created_at_unix_ms: 900_000,
            last_seen_unix_ms: 900_000,
            activity_observed: false,
        };
        let (ever_ran, created, last_activity) =
            derive_activity_clock(&[session], Some(&metadata), Some(100_000), 1_000_000);
        assert!(ever_ran);
        assert_eq!(created, 200_000);
        assert_eq!(last_activity, 700_000);
    }

    #[test]
    fn guardian_socket_is_stable_and_workspace_local() {
        let limits =
            GuardianLimits::new(2, Some(Duration::from_secs(60)), Some(Duration::from_secs(10)));
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
