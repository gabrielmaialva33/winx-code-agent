//! Tools module for the Winx application.
//!
//! This module hosts the individual MCP tool implementations (shell, file IO,
//! image, context save). The live service that wires them to the MCP protocol
//! is [`crate::server::WinxService`].

pub mod bash_command;
pub mod context_save;
pub mod file_write_or_edit;
pub mod initialize;
pub mod read_files;
pub mod read_image;
