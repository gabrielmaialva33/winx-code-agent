use std::path::{Path, PathBuf};

use tokio::net::UnixStream;

use super::protocol::{
    read_json_frame, write_json_frame, ConfigureSessionParams, ConfigureSessionResult,
    ConfigureSessionTransition, HelloResult, JournalRead, JournalReadParams, PruneParams,
    PruneResult, RpcRequest, RpcResponse, RunActionParams, RunActionResult, SessionInfo,
    SessionParams, WireShellError, PROTOCOL_MAJOR,
};
use crate::errors::{Result, WinxError};
use crate::runtime::{
    ShellRuntime, ShellRuntimeConfigureFuture, ShellRuntimeFuture, ShellRuntimeUnitFuture,
    ShellSessionTransition,
};
use crate::state::bash_state::BashState;
use crate::types::BashCommand;

/// Shell runtime backed by a `winxd` Unix-domain socket.
#[derive(Clone, Debug)]
pub struct DaemonShellRuntime {
    socket_path: PathBuf,
    consumer_id: String,
}

/// Control-plane client used by the CLI and reconnection tests.
#[derive(Clone, Debug)]
pub struct DaemonClient {
    socket_path: PathBuf,
}

impl DaemonClient {
    pub fn new(socket_path: impl AsRef<Path>) -> Self {
        Self { socket_path: socket_path.as_ref().to_path_buf() }
    }

    async fn connected(&self) -> Result<UnixStream> {
        let mut stream = UnixStream::connect(&self.socket_path).await.map_err(|error| {
            WinxError::ShellInitializationError(format!(
                "cannot connect to winxd at {}: {error}",
                self.socket_path.display()
            ))
        })?;
        let hello: HelloResult = DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "winx.hello",
            serde_json::json!({ "protocol_major": PROTOCOL_MAJOR }),
        )
        .await?;
        if hello.protocol_major != PROTOCOL_MAJOR {
            return Err(WinxError::ConfigurationError(format!(
                "winxd protocol major {} is incompatible with client {}",
                hello.protocol_major, PROTOCOL_MAJOR
            )));
        }
        Ok(stream)
    }

    pub async fn hello(&self) -> Result<HelloResult> {
        let mut stream = UnixStream::connect(&self.socket_path).await.map_err(|error| {
            WinxError::ShellInitializationError(format!(
                "cannot connect to winxd at {}: {error}",
                self.socket_path.display()
            ))
        })?;
        let hello: HelloResult = DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "winx.hello",
            serde_json::json!({ "protocol_major": PROTOCOL_MAJOR }),
        )
        .await?;
        if hello.protocol_major != PROTOCOL_MAJOR {
            return Err(WinxError::ConfigurationError(format!(
                "winxd protocol major {} is incompatible with client {}",
                hello.protocol_major, PROTOCOL_MAJOR
            )));
        }
        Ok(hello)
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let mut stream = self.connected().await?;
        DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "session.list",
            serde_json::json!({}),
        )
        .await
    }

    pub async fn session_info(&self, thread_id: &str) -> Result<SessionInfo> {
        let mut stream = self.connected().await?;
        DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "session.info",
            serde_json::to_value(SessionParams { thread_id: thread_id.to_string() })
                .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await
    }

    pub async fn read_output(&self, thread_id: &str, consumer_id: &str) -> Result<JournalRead> {
        let mut stream = self.connected().await?;
        DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "session.read_output",
            serde_json::to_value(JournalReadParams {
                thread_id: thread_id.to_string(),
                consumer_id: consumer_id.to_string(),
            })
            .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await
    }

    pub async fn kill_session(&self, thread_id: &str) -> Result<bool> {
        let mut stream = self.connected().await?;
        DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "session.kill",
            serde_json::to_value(SessionParams { thread_id: thread_id.to_string() })
                .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await
    }

    pub async fn prune_sessions(&self, idle_seconds: Option<u64>) -> Result<PruneResult> {
        let mut stream = self.connected().await?;
        DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "session.prune",
            serde_json::to_value(PruneParams { idle_seconds })
                .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await
    }

    pub async fn interrupt_session(&self, thread_id: &str) -> Result<()> {
        let mut stream = self.connected().await?;
        let _: bool = DaemonShellRuntime::request(
            &mut stream,
            rand::random::<u64>(),
            "session.interrupt",
            serde_json::to_value(SessionParams { thread_id: thread_id.to_string() })
                .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await?;
        Ok(())
    }
}

impl DaemonShellRuntime {
    pub fn new(socket_path: impl AsRef<Path>) -> Self {
        Self::with_consumer_id(socket_path, format!("adapter-{:016x}", rand::random::<u64>()))
    }

    pub fn with_consumer_id(socket_path: impl AsRef<Path>, consumer_id: impl Into<String>) -> Self {
        Self { socket_path: socket_path.as_ref().to_path_buf(), consumer_id: consumer_id.into() }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    async fn request<T: serde::de::DeserializeOwned>(
        stream: &mut UnixStream,
        id: u64,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T> {
        write_json_frame(
            stream,
            &RpcRequest { jsonrpc: "2.0".to_string(), id, method: method.to_string(), params },
        )
        .await?;
        let response: RpcResponse = read_json_frame(stream).await?;
        if response.id != id || response.jsonrpc != "2.0" {
            return Err(WinxError::ParseError("mismatched JSON-RPC response".to_string()));
        }
        if let Some(error) = response.error {
            return Err(WinxError::CommandExecutionError(format!(
                "winxd JSON-RPC error {}: {}",
                error.code, error.message
            )));
        }
        serde_json::from_value(
            response.result.ok_or_else(|| {
                WinxError::ParseError("JSON-RPC response omitted result".to_string())
            })?,
        )
        .map_err(|error| WinxError::DeserializationError(error.to_string()))
    }

    async fn run_remote(
        &self,
        bash_state: &std::sync::Arc<tokio::sync::Mutex<Option<BashState>>>,
        command: BashCommand,
    ) -> Result<String> {
        let snapshot =
            bash_state.lock().await.as_ref().ok_or(WinxError::BashStateNotInitialized)?.snapshot();
        let mut stream = DaemonClient::new(&self.socket_path).connected().await?;

        let request_key = format!("{:016x}", rand::random::<u64>());
        let id = rand::random::<u64>();
        let result: RunActionResult = Self::request(
            &mut stream,
            id,
            "shell.run_action",
            serde_json::to_value(RunActionParams {
                snapshot,
                command,
                request_key,
                consumer_id: self.consumer_id.clone(),
            })
            .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await?;

        if let Some(state) = bash_state.lock().await.as_mut() {
            state.apply_snapshot(&result.snapshot);
        }
        match (result.output, result.error) {
            (Some(output), None) => Ok(output),
            (_, Some(error)) => Err(from_wire_error(error)),
            _ => Err(WinxError::ParseError(
                "winxd action response had neither output nor error".to_string(),
            )),
        }
    }

    async fn configure_remote(
        &self,
        bash_state: &mut BashState,
        transition: ShellSessionTransition,
    ) -> Result<Option<String>> {
        let transition = match transition {
            ShellSessionTransition::FirstCall => ConfigureSessionTransition::FirstCall,
            ShellSessionTransition::ModeChange => ConfigureSessionTransition::ModeChange,
            ShellSessionTransition::Reset => ConfigureSessionTransition::Reset,
            ShellSessionTransition::WorkspaceChange => ConfigureSessionTransition::WorkspaceChange,
        };
        let mut stream = DaemonClient::new(&self.socket_path).connected().await?;
        let result: ConfigureSessionResult = Self::request(
            &mut stream,
            rand::random::<u64>(),
            "session.configure",
            serde_json::to_value(ConfigureSessionParams {
                snapshot: bash_state.snapshot(),
                transition,
            })
            .map_err(|error| WinxError::SerializationError(error.to_string()))?,
        )
        .await?;
        Ok(result.attach_hint)
    }
}

impl ShellRuntime for DaemonShellRuntime {
    fn configure_session<'a>(
        &'a self,
        bash_state: &'a mut BashState,
        transition: ShellSessionTransition,
    ) -> ShellRuntimeConfigureFuture<'a> {
        Box::pin(self.configure_remote(bash_state, transition))
    }

    fn run_action<'a>(
        &'a self,
        bash_state: &'a std::sync::Arc<tokio::sync::Mutex<Option<BashState>>>,
        command: BashCommand,
    ) -> ShellRuntimeFuture<'a> {
        Box::pin(self.run_remote(bash_state, command))
    }

    fn interrupt<'a>(
        &'a self,
        bash_state: &'a std::sync::Arc<tokio::sync::Mutex<Option<BashState>>>,
    ) -> ShellRuntimeUnitFuture<'a> {
        Box::pin(async move {
            let thread_id = bash_state
                .lock()
                .await
                .as_ref()
                .ok_or(WinxError::BashStateNotInitialized)?
                .current_thread_id
                .clone();
            DaemonClient::new(&self.socket_path).interrupt_session(&thread_id).await
        })
    }

    fn terminate_session<'a>(&'a self, thread_id: &'a str) -> ShellRuntimeUnitFuture<'a> {
        Box::pin(async move {
            let _ = DaemonClient::new(&self.socket_path).kill_session(thread_id).await?;
            Ok(())
        })
    }
}

fn from_wire_error(error: WireShellError) -> WinxError {
    match error {
        WireShellError::BashStateNotInitialized => WinxError::BashStateNotInitialized,
        WireShellError::ShellInitialization(message) => {
            WinxError::ShellInitializationError(message)
        }
        WireShellError::CommandExecution(message) | WireShellError::Other(message) => {
            WinxError::CommandExecutionError(message)
        }
        WireShellError::NoActiveCommand(message) => WinxError::NoActiveCommand(message),
        WireShellError::BackgroundSessionNotFound(message) => {
            WinxError::BackgroundSessionNotFound(message)
        }
        WireShellError::EmptyInteractiveInput(action) => {
            WinxError::EmptyInteractiveInput { action }
        }
        WireShellError::InteractiveTargetNotRunning(message) => {
            WinxError::InteractiveTargetNotRunning(message)
        }
        WireShellError::CommandAlreadyRunning { current_command, duration_seconds } => {
            WinxError::CommandAlreadyRunning { current_command, duration_seconds }
        }
        WireShellError::CommandNotAllowed(message) => WinxError::CommandNotAllowed(message),
        WireShellError::ThreadIdMismatch(message) => WinxError::ThreadIdMismatch(message),
        WireShellError::InvalidInput(message) => WinxError::InvalidInput(message),
    }
}
