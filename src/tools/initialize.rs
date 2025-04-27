//! Implementation of the Initialize tool.
//!
//! This module provides the implementation for the Initialize tool, which is used
//! to set up the shell environment with the specified workspace path and configuration.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, instrument, warn};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::{generate_chat_id, BashState};
use crate::types::{
    AllowedCommands, AllowedGlobs, BashCommandMode, BashMode, FileEditMode, Initialize,
    InitializeType, ModeName, Modes, WriteIfEmptyMode,
};
use crate::utils::path::{ensure_directory_exists, expand_user};

/// Converts ModeName to the internal Modes enum
///
/// This function converts the ModeName enum (which is exposed externally)
/// to the internal Modes enum used by the application.
///
/// # Arguments
///
/// * `mode_name` - The external mode name to convert
///
/// # Returns
///
/// The corresponding internal Modes enum value
#[inline]
fn convert_mode_name(mode_name: &ModeName) -> Modes {
    match mode_name {
        ModeName::Wcgw => Modes::Wcgw,
        ModeName::Architect => Modes::Architect,
        ModeName::CodeWriter => Modes::CodeWriter,
    }
}

/// Converts a mode to its corresponding bash_command_mode, file_edit_mode, and write_if_empty_mode
///
/// This function returns the appropriate configuration for a given mode,
/// including bash command restrictions, file edit permissions, and
/// file write permissions.
///
/// # Arguments
///
/// * `mode` - The mode to get the configuration for
///
/// # Returns
///
/// A tuple of (BashCommandMode, FileEditMode, WriteIfEmptyMode) for the mode
#[instrument(level = "debug", skip(mode))]
fn mode_to_state(mode: &Modes) -> (BashCommandMode, FileEditMode, WriteIfEmptyMode) {
    debug!("Generating state configuration for mode: {:?}", mode);

    match mode {
        Modes::Wcgw => {
            debug!("Using wcgw mode: full permissions");
            (
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
            )
        }
        Modes::Architect => {
            debug!("Using architect mode: restricted permissions");
            (
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
            )
        }
        Modes::CodeWriter => {
            debug!("Using code_writer mode: normal permissions");
            (
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
            )
        }
    }
}

/// Handles the Initialize tool call
///
/// This function processes the Initialize tool call, which sets up the shell
/// environment with the specified workspace path and configuration.
///
/// # Arguments
///
/// * `bash_state_arc` - Shared reference to the bash state
/// * `initialize` - The initialization parameters
///
/// # Returns
///
/// A Result containing the response message to send to the client
///
/// # Errors
///
/// Returns an error if the initialization fails for any reason
#[instrument(level = "info", skip(bash_state_arc, initialize))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    initialize: Initialize,
) -> Result<String> {
    // Start building the response
    let mut response = String::new();

    // Log full initialization parameters for debugging
    info!("Initialize tool called with detailed parameters:");
    info!("  type: {:?}", initialize.init_type);

    // Log mode_name with more details to debug serialization issues
    let mode_name_str = match initialize.mode_name {
        ModeName::Wcgw => "wcgw",
        ModeName::Architect => "architect",
        ModeName::CodeWriter => "code_writer",
    };
    info!(
        "  mode_name: {} ({:?})",
        mode_name_str, initialize.mode_name
    );

    info!("  workspace_path: {}", initialize.any_workspace_path);
    info!("  chat_id: {}", initialize.chat_id);
    info!("  code_writer_config: {:?}", initialize.code_writer_config);
    info!(
        "  initial_files_to_read: {:?}",
        initialize.initial_files_to_read
    );
    info!("  task_id_to_resume: {}", initialize.task_id_to_resume);

    // Expand workspace path with proper error handling
    let workspace_path_str = expand_user(&initialize.any_workspace_path);
    if workspace_path_str.is_empty() {
        warn!("Empty workspace path provided");
        return Err(WinxError::WorkspacePathError(
            "Workspace path cannot be empty".to_string(),
        ));
    }

    let workspace_path = PathBuf::from(&workspace_path_str);
    debug!("Expanded workspace path to: {:?}", workspace_path);

    // Create chat_id if not provided
    let chat_id =
        if initialize.chat_id.is_empty() && initialize.init_type == InitializeType::FirstCall {
            let new_chat_id = generate_chat_id();
            info!("Generated new chat_id: {}", new_chat_id);
            new_chat_id
        } else {
            debug!("Using provided chat_id: {}", initialize.chat_id);
            initialize.chat_id.clone()
        };

    // Handle workspace path with appropriate validation and error handling
    let mut folder_to_start = workspace_path.clone();

    if workspace_path.exists() {
        debug!("Workspace path exists: {:?}", workspace_path);

        if workspace_path.is_file() {
            debug!("Workspace path is a file, using parent directory");

            folder_to_start = workspace_path
                .parent()
                .ok_or_else(|| {
                    WinxError::WorkspacePathError(
                        "Could not determine parent directory of file".to_string(),
                    )
                })?
                .to_path_buf();

            response.push_str(&format!(
                "Using parent directory of file: {:?}\n",
                folder_to_start
            ));

            // If no files to read were specified, add the original file
            if initialize.initial_files_to_read.is_empty() {
                info!("No files to read specified, suggesting the workspace file");
                response.push_str(&format!("Adding file to read list: {:?}\n", workspace_path));
                // We don't actually modify the initialize struct since we just use this for reporting
            }
        } else if workspace_path.is_dir() {
            info!("Using existing workspace directory: {:?}", folder_to_start);
            response.push_str(&format!(
                "Using workspace directory: {:?}\n",
                folder_to_start
            ));
        } else {
            warn!(
                "Workspace path exists but is neither file nor directory: {:?}",
                workspace_path
            );
            return Err(WinxError::WorkspacePathError(format!(
                "Path exists but is neither file nor directory: {:?}",
                workspace_path
            )));
        }
    } else {
        warn!("Workspace path does not exist: {:?}", workspace_path);

        if workspace_path.is_absolute() {
            info!("Creating workspace directory: {:?}", workspace_path);

            // Create the directory if it doesn't exist
            match ensure_directory_exists(&workspace_path) {
                Ok(()) => {
                    response.push_str(&format!(
                        "Created workspace directory: {:?}\n",
                        workspace_path
                    ));
                }
                Err(err) => {
                    return Err(WinxError::WorkspacePathError(format!(
                        "Failed to create workspace directory: {}",
                        err
                    )));
                }
            }
        } else {
            warn!(
                "Non-absolute workspace path does not exist: {:?}",
                workspace_path
            );
            response.push_str(&format!(
                "Warning: Workspace path {:?} does not exist\n",
                workspace_path
            ));
        }
    }

    // Initialize or update BashState with proper error handling
    let mut bash_state_guard = bash_state_arc
        .lock()
        .map_err(|e| WinxError::BashStateLockError(format!("Failed to lock bash state: {}", e)))?;

    let mode = convert_mode_name(&initialize.mode_name);

    // Determine mode configurations
    let (bash_command_mode, file_edit_mode, write_if_empty_mode) = if mode == Modes::CodeWriter {
        // Special handling for CodeWriter mode
        if let Some(config) = initialize.code_writer_config.as_ref() {
            info!("Using provided CodeWriter config: {:?}", config);

            // Update relative globs to absolute paths if needed
            let folder_to_start_str = folder_to_start.to_string_lossy().to_string();
            let mut config_clone = config.clone();
            config_clone.update_relative_globs(&folder_to_start_str);

            debug!("Updated config with absolute paths: {:?}", config_clone);

            let cmd_mode = BashCommandMode {
                bash_mode: BashMode::NormalMode,
                allowed_commands: config_clone.allowed_commands.clone(),
            };

            let edit_mode = FileEditMode {
                allowed_globs: config_clone.allowed_globs.clone(),
            };

            let write_mode = WriteIfEmptyMode {
                allowed_globs: config_clone.allowed_globs.clone(),
            };

            response.push_str("Using custom CodeWriter configuration\n");
            (cmd_mode, edit_mode, write_mode)
        } else {
            warn!("CodeWriter mode specified but no config provided, using defaults");
            response.push_str("CodeWriter mode specified but no config provided, using defaults\n");
            mode_to_state(&mode)
        }
    } else {
        // Use default mode configuration
        debug!("Using default configuration for mode: {:?}", mode);
        mode_to_state(&mode)
    };

    // Process based on initialization type
    match initialize.init_type {
        InitializeType::FirstCall => {
            info!("Handling FirstCall initialization type");

            let mut new_bash_state = BashState::new();
            new_bash_state.current_chat_id = chat_id.clone();
            new_bash_state.mode = mode;
            new_bash_state.bash_command_mode = bash_command_mode;
            new_bash_state.file_edit_mode = file_edit_mode;
            new_bash_state.write_if_empty_mode = write_if_empty_mode;

            if folder_to_start.exists() {
                debug!("Setting working directory to: {:?}", folder_to_start);

                // Update CWD with error handling
                new_bash_state.update_cwd(&folder_to_start).map_err(|e| {
                    WinxError::WorkspacePathError(format!(
                        "Failed to update current working directory: {}",
                        e
                    ))
                })?;

                // Update workspace root with error handling
                new_bash_state
                    .update_workspace_root(&folder_to_start)
                    .map_err(|e| {
                        WinxError::WorkspacePathError(format!(
                            "Failed to update workspace root: {}",
                            e
                        ))
                    })?;
            } else {
                warn!("Folder to start does not exist: {:?}", folder_to_start);
            }

            info!("Initializing new BashState with chat_id: {}", chat_id);
            *bash_state_guard = Some(new_bash_state);

            response.push_str(&format!(
                "Initialized new shell with chat_id: {}\n",
                chat_id
            ));
        }
        InitializeType::UserAskedModeChange => {
            info!("Handling UserAskedModeChange initialization type");

            if let Some(ref mut bash_state) = *bash_state_guard {
                debug!("Changing mode from {:?} to {:?}", bash_state.mode, mode);

                bash_state.mode = mode;
                bash_state.bash_command_mode = bash_command_mode;
                bash_state.file_edit_mode = file_edit_mode;
                bash_state.write_if_empty_mode = write_if_empty_mode;

                response.push_str(&format!("Changed mode to: {:?}\n", mode));
            } else {
                warn!("BashState not initialized for UserAskedModeChange");
                return Err(WinxError::BashStateNotInitialized);
            }
        }
        InitializeType::ResetShell => {
            info!("Handling ResetShell initialization type");

            if let Some(ref mut bash_state) = *bash_state_guard {
                debug!("Resetting shell with mode: {:?}", mode);

                bash_state.mode = mode;
                bash_state.bash_command_mode = bash_command_mode;
                bash_state.file_edit_mode = file_edit_mode;
                bash_state.write_if_empty_mode = write_if_empty_mode;

                response.push_str("Reset shell\n");
            } else {
                warn!("BashState not initialized for ResetShell");
                return Err(WinxError::BashStateNotInitialized);
            }
        }
        InitializeType::UserAskedChangeWorkspace => {
            info!("Handling UserAskedChangeWorkspace initialization type");

            if let Some(ref mut bash_state) = *bash_state_guard {
                if folder_to_start.exists() {
                    debug!("Changing workspace to: {:?}", folder_to_start);

                    // Update CWD with error handling
                    bash_state.update_cwd(&folder_to_start).map_err(|e| {
                        WinxError::WorkspacePathError(format!(
                            "Failed to update current working directory: {}",
                            e
                        ))
                    })?;

                    // Update workspace root with error handling
                    bash_state
                        .update_workspace_root(&folder_to_start)
                        .map_err(|e| {
                            WinxError::WorkspacePathError(format!(
                                "Failed to update workspace root: {}",
                                e
                            ))
                        })?;

                    response.push_str(&format!("Changed workspace to: {:?}\n", folder_to_start));
                } else {
                    warn!("Workspace path does not exist: {:?}", folder_to_start);
                    response.push_str(&format!(
                        "Warning: Workspace path {:?} does not exist\n",
                        folder_to_start
                    ));
                }
            } else {
                warn!("BashState not initialized for UserAskedChangeWorkspace");
                return Err(WinxError::BashStateNotInitialized);
            }
        }
    }

    // Handle initial files to read
    if !initialize.initial_files_to_read.is_empty() {
        info!(
            "Processing {} initial files to read",
            initialize.initial_files_to_read.len()
        );

        response.push_str("\nInitial files to read:\n");
        for file in &initialize.initial_files_to_read {
            // Validate each file path
            let expanded_path = expand_user(file);
            if !expanded_path.is_empty() {
                response.push_str(&format!("- {}\n", file));
            } else {
                warn!("Empty file path in initial_files_to_read");
                response.push_str(&format!("- Warning: Invalid file path: {}\n", file));
            }
        }
    }

    // Handle task resumption
    if !initialize.task_id_to_resume.is_empty() {
        info!(
            "Task resumption requested for ID: {}",
            initialize.task_id_to_resume
        );

        if initialize.init_type == InitializeType::FirstCall {
            response.push_str(&format!(
                "\nAttempting to resume task: {}\n",
                initialize.task_id_to_resume
            ));
            // In actual implementation, load task state here
            // TODO: Implement task resumption logic
        } else {
            warn!("Task resumption not allowed for non-FirstCall initialization");
            response.push_str(
                "\nWarning: task can only be resumed in a new conversation. No task loaded.\n",
            );
        }
    }

    // Build and add environment information
    let current_state = if let Some(ref bash_state) = *bash_state_guard {
        // Get home directory with proper error handling
        let home_dir = home::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| {
                warn!("Could not determine home directory");
                "Unknown".to_string()
            });

        debug!("Adding environment information to response");

        format!(
            "\nEnvironment:\nSystem: {}\nMachine: {}\nInitialized in directory (also cwd): {:?}\nUser home directory: {}\n",
            std::env::consts::OS,
            std::env::consts::ARCH,
            bash_state.cwd,
            home_dir,
        )
    } else {
        warn!("No bash state available for environment information");
        String::new()
    };

    response.push_str(&current_state);

    // Add chat ID instruction
    if let Some(ref bash_state) = *bash_state_guard {
        info!(
            "Final response will use chat_id: {}",
            bash_state.current_chat_id
        );

        response.push_str(&format!(
            "\nUse chat_id={} for all winx tool calls which take that.\n",
            bash_state.current_chat_id
        ));
    }

    // Add note about additional important instructions
    response.push_str("\n- Additional important note: as soon as you encounter \"The user has chosen to disallow the tool call.\", immediately stop doing everything and ask user for the reason.\n");

    info!("Initialize tool call completed successfully");
    debug!("Final response: {}", response);

    Ok(response)
}
