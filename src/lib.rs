#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
#![allow(clippy::to_string_in_format_args)]
#![allow(clippy::let_and_return)]
#![allow(clippy::needless_return)]

//! # Winx Code Agent
//!
//! A high-performance Rust implementation of WCGW (What Could Go Wrong) for code agents.
//! Provides shell execution and file management capabilities for LLM code agents,
//! designed to integrate with Claude and other LLMs via the Model Context Protocol (MCP).

pub mod dashscope;
pub mod errors;
pub mod gemini;
pub mod nvidia;
pub mod server;
pub mod state;
pub mod tools;
pub mod types;
pub mod utils;

pub use errors::{Result, WinxError};
pub use server::{WinxService, start_winx_server};
pub use tools::WinxService as WinxToolsService;
