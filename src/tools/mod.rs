//! Tools module for the Winx application.
//!
//! This module hosts the individual MCP tool implementations (shell, file IO,
//! image, context save). The live service that wires them to the MCP protocol
//! is [`crate::server::WinxService`].

pub mod bash_command;
pub mod code_map;
pub mod context_save;
pub mod file_write_or_edit;
pub mod initialize;
pub mod multi_file_edit;
pub mod outline;
pub mod read_files;
pub mod read_image;
pub mod references;
pub mod undo_edit;

/// Serialize a tool's structured output to JSON for the MCP result's
/// `structuredContent`, mapping the (practically impossible) failure to a domain
/// error instead of panicking. Used by the read-only `CodeMap` handlers.
pub(crate) fn structured_json<T: serde::Serialize>(
    value: &T,
) -> crate::errors::Result<serde_json::Value> {
    serde_json::to_value(value).map_err(|e| {
        crate::errors::WinxError::SerializationError(format!("structured output: {e}"))
    })
}
