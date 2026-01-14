//! Implementation of the Initialize tool.
//! 
//! This module provides the implementation for the Initialize tool, which is used
//! to set up the shell environment with the specified workspace path and configuration.

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, instrument, warn};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::{generate_thread_id, BashState};
use crate::types::{
    AllowedCommands, AllowedGlobs, BashCommandMode, BashMode, FileEditMode, Initialize,
    InitializeType, ModeName, Modes, WriteIfEmptyMode,
};
use crate::utils::path::{ensure_directory_exists, expand_user};

/// Converts `ModeName` to the internal Modes enum
#[inline]
fn convert_mode_name(mode_name: &ModeName) -> Modes {
    match mode_name {
        ModeName::Wcgw => Modes::Wcgw,
        ModeName::Architect => Modes::Architect,
        ModeName::CodeWriter => Modes::CodeWriter,
    }
}

/// Converts a mode to its corresponding state configuration
fn mode_to_state(mode: &Modes) -> (BashCommandMode, FileEditMode, WriteIfEmptyMode) {
    match mode {
        Modes::Wcgw => {
            (
                BashCommandMode {
                    bash_mode: BashMode::NormalMode,
                    allowed_commands: AllowedCommands::All("all".to_string()),
                },
                FileEditMode { allowed_globs: AllowedGlobs::All("all".to_string()) },
                WriteIfEmptyMode { allowed_globs: AllowedGlobs::All("all".to_string()) },
            )
        }
        Modes::Architect => {
            (
                BashCommandMode {
                    bash_mode: BashMode::RestrictedMode,
                    allowed_commands: AllowedCommands::All("all".to_string()),
                },
                FileEditMode { allowed_globs: AllowedGlobs::List(vec![]) },
                WriteIfEmptyMode { allowed_globs: AllowedGlobs::List(vec![]) },
            )
        }
        Modes::CodeWriter => {
            (
                BashCommandMode {
                    bash_mode: BashMode::NormalMode,
                    allowed_commands: AllowedCommands::All("all".to_string()),
                },
                FileEditMode { allowed_globs: AllowedGlobs::All("all".to_string()) },
                WriteIfEmptyMode { allowed_globs: AllowedGlobs::All("all".to_string()) },
            )
        }
    }
}

/// Handles the Initialize tool call
#[instrument(level = "info", skip(bash_state_arc, initialize))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    initialize: Initialize,
) -> Result<String> {
    let mut response = String::new();

    info!("Initialize called for workspace: {}", initialize.any_workspace_path);

    let workspace_path_str = expand_user(&initialize.any_workspace_path);
    if workspace_path_str.is_empty() {
        return Err(WinxError::WorkspacePathError(
            "Workspace path cannot be empty.".to_string(),
        ));
    }

    let workspace_path = PathBuf::from(&workspace_path_str);
    let mut folder_to_start = workspace_path.clone();

    if workspace_path.exists() {
        if workspace_path.is_file() {
            folder_to_start = workspace_path.parent().unwrap_or(&workspace_path).to_path_buf();
            response.push_str(&format!("Using parent directory of file: {:?}\n", folder_to_start));
        } else if workspace_path.is_dir() {
            response.push_str(&format!("Using workspace directory: {:?}\n", folder_to_start));
        }
    } else if workspace_path.is_absolute() {
        ensure_directory_exists(&workspace_path).map_err(|e| {
            WinxError::WorkspacePathError(format!("Failed to create workspace: {e}"))
        })?;
        response.push_str(&format!("Created workspace directory: {:?}\n", workspace_path));
    }

    let thread_id = if initialize.thread_id.is_empty() {
        generate_thread_id()
    } else {
        initialize.thread_id.clone()
    };

    let mut bash_state_guard = bash_state_arc.lock().await;
    let mode = convert_mode_name(&initialize.mode_name);
    let (bash_command_mode, file_edit_mode, write_if_empty_mode) = mode_to_state(&mode);

    let mut new_bash_state = BashState::new();
    new_bash_state.current_thread_id = thread_id.clone();
    new_bash_state.mode = mode;
    new_bash_state.bash_command_mode = bash_command_mode;
    new_bash_state.file_edit_mode = file_edit_mode;
    new_bash_state.write_if_empty_mode = write_if_empty_mode;
    new_bash_state.initialized = true;

    if folder_to_start.exists() {
        new_bash_state.update_cwd(&folder_to_start)?;
        new_bash_state.update_workspace_root(&folder_to_start)?;
        new_bash_state.init_interactive_bash()?;
    }

    *bash_state_guard = Some(new_bash_state);

    response.push_str(&format!("\n# Environment\nSystem: {}\nMachine: {}\nInitialized in directory: {:?}\n",
        std::env::consts::OS, std::env::consts::ARCH, folder_to_start));
    
    response.push_str(&format!("\nUse thread_id={} for all winx tool calls.\n", thread_id));

    Ok(response)
}