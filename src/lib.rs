// Library code - some functions are part of public API but may not be used internally
#![allow(dead_code)]
// Clippy lints for library flexibility
#![allow(clippy::too_many_arguments)]

//! # Winx Code Agent
//!
//! A high-performance Rust implementation of WCGW (What Could Go Wrong) for code agents.
//! Provides shell execution and file management capabilities for LLM code agents,
//! designed to integrate with Claude and other LLMs via the Model Context Protocol (MCP).

pub mod errors;
pub mod server;
pub mod state;
pub mod tools;
pub mod types;
pub mod utils;

pub use errors::{Result, WinxError};
pub use server::{start_winx_server, WinxService};
pub use tools::WinxService as WinxToolsService;
