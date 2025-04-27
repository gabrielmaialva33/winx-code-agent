use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::state::bash_state::{generate_chat_id, BashState};
use crate::types::{
    AllowedCommands, AllowedGlobs, BashCommandMode, BashMode, FileEditMode, Initialize,
    InitializeType, ModeName, Modes, WriteIfEmptyMode,
};
use crate::utils::path::{ensure_directory_exists, expand_user};

/// Converts ModeName to the internal Modes enum
fn convert_mode_name(mode_name: &ModeName) -> Modes {
    match mode_name {
        ModeName::Wcgw => Modes::Wcgw,
        ModeName::Architect => Modes::Architect,
        ModeName::CodeWriter => Modes::CodeWriter,
    }
}

/// Converts a mode to its corresponding bash_command_mode, file_edit_mode, and write_if_empty_mode
fn mode_to_state(mode: &Modes) -> (BashCommandMode, FileEditMode, WriteIfEmptyMode) {
    match mode {
        Modes::Wcgw => (
            BashCommandMode {
                bash_mode: BashMode::NormalMode,
                allowed_commands: AllowedCommands::All("all".to_string()),
            },
            FileEditMode {
                allowed_globs: AllowedGlobs::All("all".to_string()),
            },
            WriteIfEmptyMode {
                allowed_globs: AllowedGlobs::All("all".to_string()),
            },
        ),
        Modes::Architect => (
            BashCommandMode {
                bash_mode: BashMode::RestrictedMode,
                allowed_commands: AllowedCommands::All("all".to_string()),
            },
            FileEditMode {
                allowed_globs: AllowedGlobs::List(vec![]),
            },
            WriteIfEmptyMode {
                allowed_globs: AllowedGlobs::List(vec![]),
            },
        ),
        Modes::CodeWriter => (
            BashCommandMode {
                bash_mode: BashMode::NormalMode,
                allowed_commands: AllowedCommands::All("all".to_string()),
            },
            FileEditMode {
                allowed_globs: AllowedGlobs::All("all".to_string()),
            },
            WriteIfEmptyMode {
                allowed_globs: AllowedGlobs::All("all".to_string()),
            },
        ),
    }
}

/// Handles the Initialize tool call
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    initialize: Initialize,
) -> Result<String> {
    let mut response = String::new();

    // Log full initialization parameters for debugging
    tracing::info!("Initialize tool called with:");
    tracing::info!("  type: {:?}", initialize.init_type);
    
    // Log mode_name with more details to debug serialization issues
    let mode_name_str = match initialize.mode_name {
        ModeName::Wcgw => "wcgw",
        ModeName::Architect => "architect",
        ModeName::CodeWriter => "code_writer",
    };
    tracing::info!("  mode_name: {} ({:?})", mode_name_str, initialize.mode_name);
    
    tracing::info!("  workspace_path: {}", initialize.any_workspace_path);
    tracing::info!("  chat_id: {}", initialize.chat_id);
    tracing::info!("  code_writer_config: {:?}", initialize.code_writer_config);
    tracing::info!(
        "  initial_files_to_read: {:?}",
        initialize.initial_files_to_read
    );
    tracing::info!("  task_id_to_resume: {}", initialize.task_id_to_resume);

    // Expand workspace path
    let workspace_path_str = expand_user(&initialize.any_workspace_path);
    let workspace_path = PathBuf::from(&workspace_path_str);

    tracing::debug!("Expanded workspace path to: {:?}", workspace_path);

    // Create chat_id if not provided
    let chat_id =
        if initialize.chat_id.is_empty() && initialize.init_type == InitializeType::FirstCall {
            let new_chat_id = generate_chat_id();
            tracing::info!("Generated new chat_id: {}", new_chat_id);
            new_chat_id
        } else {
            tracing::debug!("Using provided chat_id: {}", initialize.chat_id);
            initialize.chat_id.clone()
        };

    // Handle workspace path
    let mut folder_to_start = workspace_path.clone();
    if workspace_path.exists() {
        if workspace_path.is_file() {
            folder_to_start = workspace_path
                .parent()
                .ok_or_else(|| anyhow!("Could not determine parent directory"))?
                .to_path_buf();

            response.push_str(&format!(
                "Using parent directory of file: {:?}\n",
                folder_to_start
            ));

            // If no files to read were specified, add the original file
            if initialize.initial_files_to_read.is_empty() {
                response.push_str(&format!("Adding file to read list: {:?}\n", workspace_path));
                // We don't actually modify the initialize struct since we just use this for reporting
            }
        } else {
            response.push_str(&format!(
                "Using workspace directory: {:?}\n",
                folder_to_start
            ));
        }
    } else {
        if workspace_path.is_absolute() {
            // Create the directory if it doesn't exist
            ensure_directory_exists(&workspace_path)
                .context("Failed to create workspace directory")?;
            response.push_str(&format!(
                "Created workspace directory: {:?}\n",
                workspace_path
            ));
        } else {
            response.push_str(&format!(
                "Warning: Workspace path {:?} does not exist\n",
                workspace_path
            ));
        }
    }

    // Initialize or update BashState
    let mut bash_state_guard = bash_state_arc
        .lock()
        .map_err(|_| anyhow!("Failed to lock bash state"))?;

    let mode = convert_mode_name(&initialize.mode_name);
    let mut bash_command_mode;
    let mut file_edit_mode;
    let mut write_if_empty_mode;

    if mode == Modes::CodeWriter {
        // Check if code_writer_config is provided
        if initialize.code_writer_config.is_some() {
            // Use the provided code_writer_config
            let config = initialize.code_writer_config.as_ref().unwrap();
            tracing::info!("Using provided CodeWriter config: {:?}", config);

            // Update relative globs to absolute paths if needed
            let folder_to_start_str = folder_to_start.to_string_lossy().to_string();
            let mut config_clone = config.clone();
            config_clone.update_relative_globs(&folder_to_start_str);

            bash_command_mode = BashCommandMode {
                bash_mode: BashMode::NormalMode,
                allowed_commands: config_clone.allowed_commands.clone(),
            };

            file_edit_mode = FileEditMode {
                allowed_globs: config_clone.allowed_globs.clone(),
            };

            write_if_empty_mode = WriteIfEmptyMode {
                allowed_globs: config_clone.allowed_globs.clone(),
            };

            response.push_str(&format!("Using custom CodeWriter configuration\n"));
        } else {
            tracing::warn!("CodeWriter mode specified but no config provided, using defaults");
            response.push_str("CodeWriter mode specified but no config provided, using defaults\n");
            
            // Use default code writer mode configuration
            (bash_command_mode, file_edit_mode, write_if_empty_mode) = mode_to_state(&mode);
        }
    } else {
        // Use default mode configuration
        (bash_command_mode, file_edit_mode, write_if_empty_mode) = mode_to_state(&mode);
    }

    match initialize.init_type {
        InitializeType::FirstCall => {
            let mut new_bash_state = BashState::new();
            new_bash_state.current_chat_id = chat_id.clone();
            new_bash_state.mode = mode.clone();
            new_bash_state.bash_command_mode = bash_command_mode;
            new_bash_state.file_edit_mode = file_edit_mode;
            new_bash_state.write_if_empty_mode = write_if_empty_mode;

            if folder_to_start.exists() {
                new_bash_state
                    .update_cwd(&folder_to_start)
                    .context("Failed to update current working directory")?;
                new_bash_state
                    .update_workspace_root(&folder_to_start)
                    .context("Failed to update workspace root")?;
            }

            *bash_state_guard = Some(new_bash_state);
            response.push_str(&format!(
                "Initialized new shell with chat_id: {}\n",
                chat_id
            ));
        }
        InitializeType::UserAskedModeChange => {
            if let Some(ref mut bash_state) = *bash_state_guard {
                bash_state.mode = mode.clone();
                bash_state.bash_command_mode = bash_command_mode;
                bash_state.file_edit_mode = file_edit_mode;
                bash_state.write_if_empty_mode = write_if_empty_mode;

                response.push_str(&format!("Changed mode to: {:?}\n", mode));
            } else {
                return Err(anyhow!(
                    "BashState not initialized. Call with type=first_call first."
                ));
            }
        }
        InitializeType::ResetShell => {
            if let Some(ref mut bash_state) = *bash_state_guard {
                bash_state.mode = mode.clone();
                bash_state.bash_command_mode = bash_command_mode;
                bash_state.file_edit_mode = file_edit_mode;
                bash_state.write_if_empty_mode = write_if_empty_mode;

                response.push_str("Reset shell\n");
            } else {
                return Err(anyhow!(
                    "BashState not initialized. Call with type=first_call first."
                ));
            }
        }
        InitializeType::UserAskedChangeWorkspace => {
            if let Some(ref mut bash_state) = *bash_state_guard {
                if folder_to_start.exists() {
                    bash_state
                        .update_cwd(&folder_to_start)
                        .context("Failed to update current working directory")?;
                    bash_state
                        .update_workspace_root(&folder_to_start)
                        .context("Failed to update workspace root")?;

                    response.push_str(&format!("Changed workspace to: {:?}\n", folder_to_start));
                } else {
                    response.push_str(&format!(
                        "Warning: Workspace path {:?} does not exist\n",
                        folder_to_start
                    ));
                }
            } else {
                return Err(anyhow!(
                    "BashState not initialized. Call with type=first_call first."
                ));
            }
        }
    }

    // Handle initial files to read
    if !initialize.initial_files_to_read.is_empty() {
        response.push_str("\nInitial files to read:\n");
        for file in &initialize.initial_files_to_read {
            response.push_str(&format!("- {}\n", file));
        }
    }

    // Handle task resumption
    if !initialize.task_id_to_resume.is_empty() {
        if initialize.init_type == InitializeType::FirstCall {
            response.push_str(&format!(
                "\nAttempting to resume task: {}\n",
                initialize.task_id_to_resume
            ));
            // In actual implementation, load task state here
        } else {
            response.push_str(
                "\nWarning: task can only be resumed in a new conversation. No task loaded.\n",
            );
        }
    }

    // Additional information
    let current_state = if let Some(ref bash_state) = *bash_state_guard {
        format!(
            "\nEnvironment:\nSystem: {}\nMachine: {}\nInitialized in directory (also cwd): {:?}\nUser home directory: {:?}\n",
            std::env::consts::OS,
            std::env::consts::ARCH,
            bash_state.cwd,
            home::home_dir().unwrap_or_default(),
        )
    } else {
        String::new()
    };

    response.push_str(&current_state);

    // Final instruction
    if let Some(ref bash_state) = *bash_state_guard {
        response.push_str(&format!(
            "\nUse chat_id={} for all winx tool calls which take that.\n",
            bash_state.current_chat_id
        ));
    }

    // Add note about additional important instructions
    response.push_str("\n- Additional important note: as soon as you encounter \"The user has chosen to disallow the tool call.\", immediately stop doing everything and ask user for the reason.\n");

    Ok(response)
}
