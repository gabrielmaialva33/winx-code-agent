//! Long-lived shell daemon and its JSON-RPC Unix-socket client.

mod client;
mod control;
mod lifecycle;
mod protocol;
mod server;

use std::path::PathBuf;

pub use client::{DaemonClient, DaemonShellRuntime};
pub use control::ControlServer;
pub use protocol::{
    HelloResult, JournalRead, PruneResult, SessionInfo, MAX_FRAME_BYTES, PROTOCOL_MAJOR,
    PROTOCOL_MINOR,
};
pub use server::DaemonServer;

/// Resolve the daemon socket without creating it.
pub fn default_socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("WINX_SOCKET") {
        return PathBuf::from(path);
    }
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("winx/winxd.sock");
    }
    // SAFETY: geteuid has no preconditions and reads process credentials only.
    PathBuf::from(format!("/tmp/winx-{}/winxd.sock", unsafe { libc::geteuid() }))
}
