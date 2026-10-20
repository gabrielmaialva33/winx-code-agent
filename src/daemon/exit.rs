//! Why a daemon process stopped, recorded beside its socket.
//!
//! A control daemon or guardian that disappears without a trace forces a
//! post-mortem from process lists alone. Every planned exit path (the
//! `winx.shutdown` RPC, a termination signal, a listener failure) records a
//! small JSON file next to the socket and logs the reason once, so `doctor`
//! and the control daemon's reaper can tell a planned restart from a crash.
//! A hard kill still leaves no record, which is itself the signal: a stale
//! socket with no exit record means the process did not get to say goodbye.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Why the process is exiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitReason {
    /// A client sent `winx.shutdown` (planned control restart, session kill).
    ShutdownRequest,
    /// The operating system delivered a termination signal.
    Signal(&'static str),
    /// The listener stopped accepting connections.
    ListenerFailure(String),
}

impl ExitReason {
    fn kind(&self) -> &'static str {
        match self {
            Self::ShutdownRequest => "shutdown_request",
            Self::Signal(_) => "signal",
            Self::ListenerFailure(_) => "listener_failure",
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            Self::ShutdownRequest => None,
            Self::Signal(name) => Some((*name).to_string()),
            Self::ListenerFailure(error) => Some(error.clone()),
        }
    }
}

impl fmt::Display for ExitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShutdownRequest => f.write_str("winx.shutdown request"),
            Self::Signal(name) => write!(f, "signal {name}"),
            Self::ListenerFailure(error) => write!(f, "listener failure: {error}"),
        }
    }
}

/// What a daemon wrote when it last exited on purpose.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExitRecord {
    pub role: String,
    pub pid: u32,
    pub epoch: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub exited_at_unix_ms: u64,
}

/// Per-process identity captured when the daemon binds its socket.
#[derive(Debug, Clone)]
struct ExitContext {
    role: &'static str,
    socket: PathBuf,
    epoch: String,
}

static CONTEXT: OnceLock<ExitContext> = OnceLock::new();

/// Path of the exit record kept beside `socket`.
pub fn record_path(socket: &Path) -> PathBuf {
    socket.with_extension("exit.json")
}

/// Remember who this process is and arm the signal handlers that turn a
/// termination signal into a recorded exit. Later calls are ignored: one
/// process binds one socket.
pub fn install(role: &'static str, socket: &Path, epoch: &str) {
    let context = ExitContext { role, socket: socket.to_path_buf(), epoch: epoch.to_string() };
    if CONTEXT.set(context).is_err() {
        return;
    }
    tokio::spawn(wait_for_termination_signal());
}

/// Record `reason` for this process and exit with status 0. Callers use it
/// only after the in-flight response has been written.
pub fn leave(reason: &ExitReason) -> ! {
    record_current(reason);
    std::process::exit(0)
}

/// Record `reason` for this process without exiting (listener failures that
/// propagate an error to `main`).
pub fn record_current(reason: &ExitReason) {
    let Some(context) = CONTEXT.get() else {
        tracing::info!(%reason, "daemon exiting before its socket was bound");
        return;
    };
    tracing::info!(role = context.role, %reason, epoch = %context.epoch, "daemon exiting");
    if let Err(error) = write_record(&context.socket, context.role, &context.epoch, reason) {
        tracing::warn!(%error, "could not write the exit record");
    }
}

/// Write the exit record for `socket` atomically (temp file plus rename).
pub fn write_record(
    socket: &Path,
    role: &str,
    epoch: &str,
    reason: &ExitReason,
) -> std::io::Result<()> {
    let record = ExitRecord {
        role: role.to_string(),
        pid: std::process::id(),
        epoch: epoch.to_string(),
        reason: reason.kind().to_string(),
        detail: reason.detail(),
        exited_at_unix_ms: unix_ms(),
    };
    let path = record_path(socket);
    let temp = path.with_extension(format!("exit.json.tmp-{:016x}", rand::random::<u64>()));
    let data = serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?;
    std::fs::write(&temp, data)?;
    std::fs::rename(&temp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

/// Read the exit record beside `socket`, if the daemon left one.
pub fn read_record(socket: &Path) -> Option<ExitRecord> {
    let data = std::fs::read(record_path(socket)).ok()?;
    serde_json::from_slice(&data).ok()
}

/// Remove the exit record beside `socket`, returning what it said.
pub async fn take_record(socket: &Path) -> Option<ExitRecord> {
    let record = read_record(socket);
    let _ = tokio::fs::remove_file(record_path(socket)).await;
    record
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(unix)]
async fn wait_for_termination_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, "could not install the SIGTERM handler");
            return;
        }
    };
    let mut interrupt = match signal(SignalKind::interrupt()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, "could not install the SIGINT handler");
            return;
        }
    };
    let name = tokio::select! {
        _ = terminate.recv() => "SIGTERM",
        _ = interrupt.recv() => "SIGINT",
    };
    leave(&ExitReason::Signal(name));
}

#[cfg(not(unix))]
async fn wait_for_termination_signal() {
    // Detached daemons have no console, so the handler may fail to install;
    // that only means a console Ctrl-C can never reach them anyway.
    if tokio::signal::ctrl_c().await.is_ok() {
        leave(&ExitReason::Signal("CTRL_C"));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    #[test]
    fn record_round_trips_beside_the_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("winxd.sock");
        write_record(&socket, "winxd", "abc", &ExitReason::Signal("SIGTERM")).expect("write");
        let record = read_record(&socket).expect("record");
        assert_eq!(record.role, "winxd");
        assert_eq!(record.epoch, "abc");
        assert_eq!(record.reason, "signal");
        assert_eq!(record.detail.as_deref(), Some("SIGTERM"));
        assert_eq!(record.pid, std::process::id());
        assert!(record.exited_at_unix_ms > 0);
        assert_eq!(record_path(&socket), dir.path().join("winxd.exit.json"));
    }

    #[test]
    fn shutdown_request_has_no_detail_and_overwrites_the_previous_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("g.sock");
        write_record(&socket, "winx-guardian", "1", &ExitReason::ListenerFailure("boom".into()))
            .expect("write");
        write_record(&socket, "winx-guardian", "2", &ExitReason::ShutdownRequest).expect("write");
        let record = read_record(&socket).expect("record");
        assert_eq!(record.epoch, "2");
        assert_eq!(record.reason, "shutdown_request");
        assert_eq!(record.detail, None);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["g.exit.json".to_string()], "no temp files left behind");
    }

    #[tokio::test]
    async fn take_record_removes_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("g.sock");
        write_record(&socket, "winx-guardian", "1", &ExitReason::ShutdownRequest).expect("write");
        assert!(take_record(&socket).await.is_some(), "expected a record to take");
        assert!(take_record(&socket).await.is_none(), "record should be gone");
    }

    #[test]
    fn missing_or_corrupt_records_read_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("g.sock");
        assert!(read_record(&socket).is_none(), "nothing written yet");
        std::fs::write(record_path(&socket), b"{not json").expect("write");
        assert!(read_record(&socket).is_none(), "corrupt record is ignored");
    }
}
