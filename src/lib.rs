// Documentation coverage for the broad pre-1.0 module API is tracked separately;
// all correctness and unused-code lints remain enabled.
#![allow(missing_docs)]
#![deny(unsafe_code)]

//! # Winx - High Performance MCP Server
//!
//! A high-performance Rust implementation of the Model Context Protocol (MCP).
//! It provides core tools for shell execution and file management with extreme efficiency.

pub mod build_info;
pub mod config;
#[cfg(unix)]
pub mod daemon;
pub mod diagnostics;
pub mod errors;
#[cfg(feature = "fuzzing")]
pub mod fuzzing;
pub mod http_server;
pub mod logging;
mod os;
pub mod report;
pub mod runtime;
pub mod sandbox;
pub mod server;
pub mod state;
pub mod tool_policy;
pub mod tool_registry;
pub mod tools;
pub mod types;
pub mod utils;

pub use errors::{Result, WinxError};
pub use server::{start_winx_server, start_winx_server_with_runtime, SharedBashState, WinxService};
