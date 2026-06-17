//! Implementation of the `CodeMap` tool: tree-sitter code navigation.
//!
//! One tool with two operations, consolidating what used to be the separate
//! `Outline` and `FindReferences` tools (to keep the MCP surface small). It is a
//! thin dispatcher: it builds the corresponding internal request and delegates to
//! the unchanged `outline` / `references` implementations, so all the tree-sitter
//! and ranking logic is reused as-is.

use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::instrument;

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::{CodeMap, CodeMapOperation, FindReferences, Outline};

#[instrument(level = "info", skip(bash_state_arc, code_map))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    code_map: CodeMap,
) -> Result<(String, serde_json::Value)> {
    match code_map.operation {
        CodeMapOperation::Outline => {
            let outline = Outline {
                path: code_map.path,
                max_results: code_map.max_results,
                thread_id: code_map.thread_id,
            };
            crate::tools::outline::handle_tool_call(bash_state_arc, outline).await
        }
        CodeMapOperation::References => {
            if code_map.name.trim().is_empty() {
                return Err(WinxError::ArgumentParseError(
                    "CodeMap operation 'references' requires a non-empty 'name' (the symbol to \
                     find)."
                        .to_string(),
                ));
            }
            let find = FindReferences {
                name: code_map.name,
                path: code_map.path,
                max_results: code_map.max_results,
                thread_id: code_map.thread_id,
            };
            crate::tools::references::handle_tool_call(bash_state_arc, find).await
        }
    }
}
