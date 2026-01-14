// Library code - some functions are part of public API but may not be used internally
#![allow(dead_code)]
// Allow unused items - library code may export items not used internally
#![allow(unused)]
// Clippy lints for library flexibility
#![allow(clippy::too_many_arguments)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_wrap)]
// Internal modules don't need full documentation
#![allow(missing_docs)]

//! # Winx - High Performance MCP Server
//!
//! A high-performance Rust implementation of the Model Context Protocol (MCP).
//! It provides core tools for shell execution and file management with extreme efficiency.

pub mod errors;
pub mod server;
pub mod state;
pub mod tools;
pub mod types;
pub mod utils;

pub use errors::{Result, WinxError};
pub use server::{start_winx_server, SharedBashState, WinxService};
pub use tools::WinxService as WinxToolsService;
