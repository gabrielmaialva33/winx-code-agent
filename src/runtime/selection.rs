use std::sync::Arc;

#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use crate::daemon::{default_socket_path, DaemonClient, DaemonShellRuntime};
use crate::errors::{Result, WinxError};

use super::{EmbeddedShellRuntime, ShellRuntime};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeMode {
    Daemon,
    Embedded,
}

fn enabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| matches!(value, "1" | "true" | "yes" | "on"))
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
        None | Some("") if daemon_supported => Ok(RuntimeMode::Daemon),
        None | Some("") => Ok(RuntimeMode::Embedded),
        Some("daemon") if daemon_supported => Ok(RuntimeMode::Daemon),
        Some("daemon") => Err(WinxError::ConfigurationError(
            "WINX_RUNTIME=\"daemon\" requires a Unix platform; use `embedded` on this OS"
                .to_string(),
        )),
        Some("embedded") => Ok(RuntimeMode::Embedded),
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
            let binary = daemon_binary()?;
            ensure_daemon_at(&socket, &binary).await?;
            Ok(Arc::new(DaemonShellRuntime::new(socket)))
        }
        #[cfg(not(unix))]
        RuntimeMode::Daemon => Err(WinxError::ConfigurationError(
            "the daemon runtime requires a Unix platform".to_string(),
        )),
    }
}

#[cfg(unix)]
fn daemon_binary() -> Result<PathBuf> {
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
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: the post-fork hook invokes only the async-signal-safe setsid(2)
        // before exec, without touching allocator-backed state.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    let mut child = command.spawn().map_err(|error| {
        WinxError::ShellInitializationError(format!(
            "failed to auto-start {}: {error}",
            daemon_binary.display()
        ))
    })?;

    let deadline = Instant::now() + Duration::from_secs(3);
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
        assert_eq!(
            select_runtime_mode_for_platform(false, None, None, None).unwrap(),
            RuntimeMode::Embedded
        );
        assert!(select_runtime_mode_for_platform(false, None, Some("daemon"), None).is_err());
    }
}
