//! Tools module for the Winx application.
//!
//! This module contains all the tools that are exposed to the MCP client,
//! including shell initialization, command execution, file operations, etc.
//!
//! The `WinxService` struct is the main entry point for all tool calls.

pub mod bash_command;
pub mod code_analyzer;
pub mod command_suggestions;
pub mod context_save;
pub mod file_write_or_edit;
pub mod initialize;
pub mod read_files;
pub mod read_image;

use std::sync::{Arc, Mutex};
use tracing::info;

use crate::errors::WinxError;
use crate::state::bash_state::BashState;

/// Version of the MCP protocol implemented by this service
#[allow(dead_code)]
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Main service implementation for Winx
///
/// This struct maintains the state of the shell environment and provides
/// methods for interacting with it through the MCP protocol.
#[derive(Debug, Clone)]
pub struct WinxService {
    /// Shared state for the bash shell environment
    bash_state: Arc<Mutex<Option<BashState>>>,
    /// Version information for the service
    version: String,
    /// Startup timestamp
    start_time: std::time::Instant,
}

impl WinxService {
    /// Create a new instance of the WinxService
    ///
    /// # Returns
    ///
    /// A new WinxService instance with an uninitialized bash state
    pub fn new() -> Self {
        info!("Creating new WinxService instance");
        Self {
            bash_state: Arc::new(Mutex::new(None)),
            version: env!("CARGO_PKG_VERSION").to_string(),
            start_time: std::time::Instant::now(),
        }
    }

    /// Get the uptime of the service
    ///
    /// # Returns
    ///
    /// The duration since the service was started
    pub fn uptime(&self) -> std::time::Duration {
        self.start_time.elapsed()
    }

    /// Get the version of the service
    ///
    /// # Returns
    ///
    /// The version string of the service
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Get a reference to the bash state, locking the mutex
    ///
    /// # Returns
    ///
    /// A Result containing a MutexGuard for the bash state
    #[allow(dead_code)]
    fn lock_bash_state(
        &self,
    ) -> crate::errors::Result<std::sync::MutexGuard<'_, Option<BashState>>> {
        self.bash_state
            .lock()
            .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bash state: {}", e)))
    }
}

// NOTE: Individual tool implementations are temporarily commented out 
// while we migrate to the new rmcp 0.5.0 pattern in server.rs
// The tools will be re-implemented using the #[tool] macro pattern