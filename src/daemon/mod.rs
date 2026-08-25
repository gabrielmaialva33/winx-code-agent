//! Long-lived shell daemon and its JSON-RPC Unix-socket client.

mod client;
mod control;
mod lifecycle;
mod protocol;
mod server;
mod socket;

pub use client::{DaemonClient, DaemonShellRuntime};
pub use control::ControlServer;
pub use protocol::{
    DaemonProcessRole, HelloResult, JournalRead, PruneResult, SessionInfo,
    BUILD_IDENTITY_CAPABILITY, COMPACT_ACTION_OUTPUT_CAPABILITY,
    GENERATION_BOUND_ACTIONS_CAPABILITY, MAX_FRAME_BYTES, PROTOCOL_MAJOR, PROTOCOL_MINOR,
    TYPED_ACTION_RESULT_CAPABILITY,
};
pub use server::DaemonServer;
pub use socket::{default_socket_path, socket_candidates, DaemonSocketCandidate};
