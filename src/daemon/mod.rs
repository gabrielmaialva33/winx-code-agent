//! Long-lived shell daemon and its JSON-RPC client. The transport is a
//! Unix-domain socket on Unix and a named pipe on Windows.

mod client;
mod control;
mod lifecycle;
mod protocol;
mod server;
mod socket;
pub mod transport;

pub use client::{DaemonClient, DaemonShellRuntime};
pub use control::ControlServer;
pub use protocol::{
    DaemonProcessRole, HelloResult, JournalRead, PruneResult, SessionInfo,
    BUILD_IDENTITY_CAPABILITY, COMPACT_ACTION_OUTPUT_CAPABILITY,
    GENERATION_BOUND_ACTIONS_CAPABILITY, MAX_FRAME_BYTES, PROCESS_SHUTDOWN_CAPABILITY,
    PROTOCOL_MAJOR, PROTOCOL_MINOR, TYPED_ACTION_RESULT_CAPABILITY,
};
pub use server::DaemonServer;
pub use socket::{default_socket_path, socket_candidates, DaemonSocketCandidate};
