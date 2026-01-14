// Library code - some functions are part of public API but may not be used internally
#![allow(dead_code)]
// Allow unused items - library code may export items not used internally
#![allow(unused)]
// Clippy lints for library flexibility
#![allow(clippy::too_many_arguments)]
// Internal modules don't need full documentation
#![allow(missing_docs)]

//! # Winx Code Agent
//!
//! A high-performance Rust implementation of WCGW (What Could Go Wrong) for code agents.
//! Provides shell execution and file management capabilities for LLM code agents,
//! designed to integrate with Claude and other LLMs via the Model Context Protocol (MCP).

pub mod agent;
pub mod canvas;
pub mod chat;
pub mod errors;
pub mod interactive;
pub mod learning;
pub mod providers;
pub mod server;
pub mod state;
pub mod tools;
pub mod types;
pub mod utils;

pub use errors::{Result, WinxError};
pub use learning::{LearningReport, LearningSystem};
pub use server::{start_winx_server, SharedBashState, WinxService};
pub use tools::WinxService as WinxToolsService;
