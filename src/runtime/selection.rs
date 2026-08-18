use std::sync::Arc;

#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use crate::daemon::{default_socket_path, DaemonClient, DaemonShellRuntime, HelloResult};
use crate::errors::{Result, WinxError};

use super::{EmbeddedShellRuntime, ShellRuntime};

#[cfg(unix)]
const GUARDIAN_LIFECYCLE_CAPABILITY: &str = "guardian_activity_clock";
#[cfg(unix)]
const PLANNED_CONTROL_RESTART_CAPABILITY: &str = "planned_control_restart";
#[cfg(unix)]
const DAEMON_TRANSITION_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeMode {
    Daemon,
    Embedded,
}

fn enabled(value: Option<&str>) -> bool {
    value.and_then(crate::config::parse_bool).unwrap_or(false)
}

/// Pure runtime-mode selection, kept separate from process spawning so its
/// default and kill switches are deterministic under test.
pub fn select_runtime_mode(
    embedded: Option<&str>,
    runtime: Option<&str>,
    sandbox: Option<&str>,
) -> Result<RuntimeMode> {
    select_runtime_mode_for_platform(cfg!(unix), embedded, runtime, sandbox)
}

fn select_runtime_mode_for_platform(
    daemon_supported: bool,
    embedded: Option<&str>,
    runtime: Option<&str>,
    sandbox: Option<&str>,
) -> Result<RuntimeMode> {
    if enabled(sandbox) || enabled(embedded) {
        return Ok(RuntimeMode::Embedded);
    }
    match runtime {
        None | Some("" | "daemon") if daemon_supported => Ok(RuntimeMode::Daemon),
        None | Some("" | "embedded") => Ok(RuntimeMode::Embedded),
        Some("daemon") => Err(WinxError::ConfigurationError(
            "WINX_RUNTIME=\"daemon\" requires a Unix platform; use `embedded` on this OS"
                .to_string(),
        )),
        Some(other) => Err(WinxError::ConfigurationError(format!(
            "invalid WINX_RUNTIME={other:?}; expected `daemon` or `embedded`"
        ))),
    }
}

pub fn configured_runtime_mode() -> Result<RuntimeMode> {
    let embedded = std::env::var("WINX_EMBEDDED").ok();
    let runtime = std::env::var("WINX_RUNTIME").ok();
    let sandbox = std::env::var("WINX_SANDBOX").ok();
    select_runtime_mode(embedded.as_deref(), runtime.as_deref(), sandbox.as_deref())
}

pub async fn configured_shell_runtime() -> Result<Arc<dyn ShellRuntime>> {
    match configured_runtime_mode()? {
        RuntimeMode::Embedded => Ok(Arc::new(EmbeddedShellRuntime)),
        #[cfg(unix)]
        RuntimeMode::Daemon => {
            let socket = default_socket_path();
            let binary = configured_daemon_binary()?;
            ensure_control_daemon_at(&socket, &binary).await?;
            Ok(Arc::new(DaemonShellRuntime::new(socket)))
        }
        #[cfg(not(unix))]
        RuntimeMode::Daemon => Err(WinxError::ConfigurationError(
            "the daemon runtime requires a Unix platform".to_string(),
        )),
    }
}

#[cfg(unix)]
pub fn configured_daemon_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("WINXD_BIN") {
        return Ok(PathBuf::from(path));
    }
    let executable = std::env::current_exe().map_err(|error| {
        WinxError::ConfigurationError(format!("cannot locate current Winx executable: {error}"))
    })?;
    let sibling = executable.with_file_name("winxd");
    if sibling.is_file() {
        Ok(sibling)
    } else {
        Err(WinxError::ConfigurationError(format!(
            "winxd not found beside {} (set WINXD_BIN or WINX_EMBEDDED=1)",
            executable.display()
        )))
    }
}

#[cfg(unix)]
fn has_capability(hello: &HelloResult, name: &str) -> bool {
    hello.capabilities.iter().any(|capability| capability == name)
}

/// Ensure the current control-plane feature set is reachable. An older `winxd`
/// that advertises safe planned restarts is replaced in place; per-session
/// guardians keep owning their PTYs throughout the control-plane transition.
#[cfg(unix)]
pub async fn ensure_control_daemon_at(socket: &Path, daemon_binary: &Path) -> Result<()> {
    ensure_daemon_at(socket, daemon_binary).await?;
    let client = DaemonClient::new(socket);
    let hello = client.hello().await?;
    if has_capability(&hello, GUARDIAN_LIFECYCLE_CAPABILITY) {
        return Ok(());
    }
    if !has_capability(&hello, PLANNED_CONTROL_RESTART_CAPABILITY) {
        return Err(WinxError::ConfigurationError(format!(
            "the running winxd (pid {}) lacks the current guardian lifecycle controls and cannot be safely \
             restarted automatically; stop it manually, then retry",
            hello.daemon_pid
        )));
    }

    tracing::info!(
        pid = hello.daemon_pid,
        "restarting older winxd control plane while preserving session guardians"
    );
    let restarted = restart_control_daemon_from_hello(socket, daemon_binary, hello).await?;
    if !has_capability(&restarted, GUARDIAN_LIFECYCLE_CAPABILITY) {
        return Err(WinxError::ConfigurationError(format!(
            "restarted winxd at {} still lacks required capability {GUARDIAN_LIFECYCLE_CAPABILITY}",
            socket.display()
        )));
    }
    Ok(())
}

/// Safely restart only `winxd`; guardian processes and their PTYs remain alive.
#[cfg(unix)]
pub async fn restart_control_daemon_at(socket: &Path, daemon_binary: &Path) -> Result<HelloResult> {
    let hello = DaemonClient::new(socket).hello().await?;
    restart_control_daemon_from_hello(socket, daemon_binary, hello).await
}

#[cfg(unix)]
async fn restart_control_daemon_from_hello(
    socket: &Path,
    daemon_binary: &Path,
    previous: HelloResult,
) -> Result<HelloResult> {
    if !has_capability(&previous, PLANNED_CONTROL_RESTART_CAPABILITY) {
        return Err(WinxError::ConfigurationError(format!(
            "winxd pid {} does not advertise safe planned control restarts",
            previous.daemon_pid
        )));
    }
    signal_control_shutdown(previous.daemon_pid)?;

    let client = DaemonClient::new(socket);
    let deadline = Instant::now() + DAEMON_TRANSITION_TIMEOUT;
    while Instant::now() < deadline {
        match client.hello().await {
            Ok(current) if current.daemon_epoch != previous.daemon_epoch => return Ok(current),
            Ok(_) => tokio::time::sleep(Duration::from_millis(25)).await,
            Err(_) => break,
        }
    }
    if let Ok(current) = client.hello().await {
        if current.daemon_epoch == previous.daemon_epoch {
            return Err(WinxError::ShellInitializationError(format!(
                "winxd pid {} did not stop within {} seconds",
                previous.daemon_pid,
                DAEMON_TRANSITION_TIMEOUT.as_secs()
            )));
        }
        return Ok(current);
    }

    ensure_daemon_at(socket, daemon_binary).await?;
    let restarted = client.hello().await?;
    if restarted.daemon_epoch == previous.daemon_epoch {
        return Err(WinxError::ShellInitializationError(
            "winxd restart returned the previous daemon epoch".to_string(),
        ));
    }
    Ok(restarted)
}

#[cfg(unix)]
fn signal_control_shutdown(pid: u32) -> Result<()> {
    crate::os::unix::signal_process(pid, libc::SIGTERM).map_err(Into::into)
}

/// Ensure a compatible daemon is reachable, starting `daemon_binary` only when
/// the socket cannot be reached. A reachable incompatible daemon is never
/// replaced or killed.
#[cfg(unix)]
pub async fn ensure_daemon_at(socket: &Path, daemon_binary: &Path) -> Result<()> {
    let client = DaemonClient::new(socket);
    match client.hello().await {
        Ok(_) => return Ok(()),
        Err(error @ WinxError::ConfigurationError(_)) => return Err(error),
        Err(_) => {}
    }

    let mut command = Command::new(daemon_binary);
    command
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::os::unix::configure_detached(&mut command);
    let mut child = command.spawn().map_err(|error| {
        WinxError::ShellInitializationError(format!(
            "failed to auto-start {}: {error}",
            daemon_binary.display()
        ))
    })?;

    let deadline = Instant::now() + DAEMON_TRANSITION_TIMEOUT;
    while Instant::now() < deadline {
        match client.hello().await {
            Ok(_) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            Err(error @ WinxError::ConfigurationError(_)) => return Err(error),
            Err(_) => {}
        }
        if let Some(status) = child.try_wait()? {
            return Err(WinxError::ShellInitializationError(format!(
                "winxd exited before accepting connections: {status}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Err(WinxError::ShellInitializationError(format!(
        "timed out waiting for winxd at {}",
        socket.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::{select_runtime_mode_for_platform, RuntimeMode};

    #[test]
    fn platform_without_unix_sockets_defaults_to_embedded() {
        assert!(matches!(
            select_runtime_mode_for_platform(false, None, None, None),
            Ok(RuntimeMode::Embedded)
        ));
        assert!(select_runtime_mode_for_platform(false, None, Some("daemon"), None).is_err());
    }
}
