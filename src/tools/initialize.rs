//! Implementation of the Initialize tool.
//!
//! This module provides the implementation for the Initialize tool, which is used
//! to set up the shell environment with the specified workspace path and configuration.
//!
//! This implementation aims for 1:1 parity with wcgw Python's initialize() function
//! in wcgw/src/wcgw/client/tools.py

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, instrument, warn};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::{generate_thread_id, BashState};
use crate::types::{
    AllowedCommands, AllowedGlobs, BashCommandMode, BashMode, FileEditMode, Initialize,
    InitializeType, ModeName, Modes, WriteIfEmptyMode,
};
use crate::utils::alignment::{
    check_ripgrep_available, load_task_context, read_global_alignment_file, read_initial_files,
    read_workspace_alignment_file,
};
use crate::utils::mode_prompts::get_mode_prompt;
use crate::utils::path::{ensure_directory_exists, expand_user};
use crate::utils::repo::get_repo_context;
use crate::utils::repo_context::RepoContextAnalyzer;

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
    info!("  thread_id: {}", initialize.thread_id);
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
            "Workspace path cannot be empty. Please provide a valid absolute or relative path to a directory or file.".to_string(),
        ));
    }

    let workspace_path = PathBuf::from(&workspace_path_str);
    debug!("Expanded workspace path to: {:?}", workspace_path);

    // Create thread_id if not provided
    let thread_id =
        if initialize.thread_id.is_empty() && initialize.init_type == InitializeType::FirstCall {
            let new_thread_id = generate_thread_id();
            info!("Generated new thread_id: {}", new_thread_id);
            new_thread_id
        } else {
            debug!("Using provided thread_id: {}", initialize.thread_id);
            initialize.thread_id.clone()
        };

    // Try to load existing state from disk if thread_id is provided
    let existing_state_loaded = if !thread_id.is_empty()
        && initialize.init_type == InitializeType::FirstCall
    {
        match BashState::has_saved_state(&thread_id) {
            Ok(true) => {
                info!(
                    "Found existing saved state for thread_id '{}', will attempt to load",
                    thread_id
                );
                true
            }
            Ok(false) => {
                debug!("No existing saved state for thread_id '{}'", thread_id);
                false
            }
            Err(e) => {
                warn!("Error checking for saved state: {}", e);
                false
            }
        }
    } else {
        false
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
                        "Could not determine parent directory of the specified file. Please provide a file path that has a valid parent directory.".to_string(),
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
                "Path exists but is neither file nor directory: {:?}. Please provide a valid file or directory path.",
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
                        "Failed to create workspace directory: {}. Please check permissions and ensure the parent directories exist or provide an existing directory path.",
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

            // Try to load existing state from disk if available
            let mut new_bash_state = if existing_state_loaded {
                let loaded_state = BashState::new_with_thread_id(Some(&thread_id));
                if loaded_state.initialized {
                    info!(
                        "Successfully loaded bash state from disk for thread_id '{}'",
                        thread_id
                    );
                    response.push_str(&format!(
                        "Resumed existing session with thread_id: {}\n",
                        thread_id
                    ));
                    loaded_state
                } else {
                    // Failed to load, create new
                    warn!("Failed to load state from disk, creating new state");
                    let mut state = BashState::new();
                    state.current_thread_id = thread_id.clone();
                    state.mode = mode;
                    state.bash_command_mode = bash_command_mode.clone();
                    state.file_edit_mode = file_edit_mode.clone();
                    state.write_if_empty_mode = write_if_empty_mode.clone();
                    state.initialized = true;
                    state
                }
            } else {
                // No existing state, create new
                let mut state = BashState::new();
                state.current_thread_id = thread_id.clone();
                state.mode = mode;
                state.bash_command_mode = bash_command_mode.clone();
                state.file_edit_mode = file_edit_mode.clone();
                state.write_if_empty_mode = write_if_empty_mode.clone();
                state.initialized = true;
                state
            };

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

                // Initialize the interactive bash session
                info!("Initializing interactive bash session");
                if let Err(e) = new_bash_state.init_interactive_bash() {
                    warn!("Failed to initialize interactive bash: {}", e);
                    response.push_str(&format!(
                        "\nWarning: Failed to initialize interactive bash: {}\nSome shell commands may not work properly.\n",
                        e
                    ));
                }

                // WCGW-style repository context analysis
                info!("Analyzing repository context");
                match RepoContextAnalyzer::analyze(&folder_to_start) {
                    Ok(repo_context) => {
                        debug!("Repository analysis completed successfully");

                        // Add repository context to response
                        response.push_str("\n# Workspace Analysis\n");
                        response.push_str(&repo_context.project_summary);

                        if repo_context.is_git_repo {
                            response.push_str("\n✓ Git repository detected\n");
                            if !repo_context.recent_files.is_empty() {
                                response.push_str(&format!(
                                    "Recent files: {}\n",
                                    repo_context.recent_files.join(", ")
                                ));
                            }
                        }

                        if !repo_context.important_files.is_empty() {
                            response.push_str(&format!(
                                "Key files: {}\n",
                                repo_context
                                    .important_files
                                    .iter()
                                    .take(5)
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        }

                        response.push_str(&format!(
                            "Total project files: {}\n",
                            repo_context.project_files.len()
                        ));
                    }
                    Err(e) => {
                        warn!("Failed to analyze repository context: {}", e);
                        response.push_str(&format!(
                            "\nWarning: Could not analyze repository structure: {}\n",
                            e
                        ));
                    }
                }
            } else {
                warn!("Folder to start does not exist: {:?}", folder_to_start);
            }

            // Add WCGW-style mode instructions before moving bash_state
            response.push_str("\n# Mode Instructions\n");
            let mode_prompt = get_mode_prompt(
                &mode,
                Some(&new_bash_state.file_edit_mode.allowed_globs),
                Some(&new_bash_state.write_if_empty_mode.allowed_globs),
                Some(&new_bash_state.bash_command_mode.allowed_commands),
            );
            response.push_str(&mode_prompt);

            info!("Initializing new BashState with thread_id: {}", thread_id);

            // Save state to disk for persistence
            if let Err(e) = new_bash_state.save_state_to_disk() {
                warn!("Failed to save initial bash state to disk: {}", e);
            } else {
                debug!("Saved initial bash state to disk");
            }

            *bash_state_guard = Some(new_bash_state);

            response.push_str(&format!(
                "Initialized new shell with thread_id: {}\n",
                thread_id
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

                // Save state to disk after mode change
                if let Err(e) = bash_state.save_state_to_disk() {
                    warn!("Failed to save bash state after mode change: {}", e);
                }

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

                // Save state to disk after reset
                if let Err(e) = bash_state.save_state_to_disk() {
                    warn!("Failed to save bash state after reset: {}", e);
                }

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

                    // Save state to disk after workspace change
                    if let Err(e) = bash_state.save_state_to_disk() {
                        warn!("Failed to save bash state after workspace change: {}", e);
                    }

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

    // Handle task resumption (matches wcgw Python behavior)
    if !initialize.task_id_to_resume.is_empty() {
        info!(
            "Task resumption requested for ID: {}",
            initialize.task_id_to_resume
        );

        if initialize.init_type == InitializeType::FirstCall {
            match load_task_context(&initialize.task_id_to_resume) {
                Some((_project_root, task_memory, _bash_state)) => {
                    response.push_str(&format!(
                        "\n---\n# Retrieved task\n{}\n---\n",
                        task_memory
                    ));
                }
                None => {
                    response.push_str(&format!(
                        "\nError: Unable to load task with ID \"{}\"\n",
                        initialize.task_id_to_resume
                    ));
                }
            }
        } else {
            warn!("Task resumption not allowed for non-FirstCall initialization");
            response.push_str(
                "\nWarning: task can only be resumed in a new conversation. No task loaded.\n",
            );
        }
    }

    // Add alignment context (WCGW-style CLAUDE.md/AGENTS.md reading)
    let mut alignment_context = String::new();

    // Check ripgrep availability
    if check_ripgrep_available() {
        alignment_context.push_str(
            "---\n# Available commands\n\n- Use ripgrep `rg` command instead of `grep` because it's much much faster.\n\n---\n\n",
        );
    }

    // Read global alignment file (~/.wcgw/CLAUDE.md or AGENTS.md)
    if let Some(global_content) = read_global_alignment_file() {
        alignment_context.push_str(&format!(
            "---\n# Important guidelines from the user\n```\n{}\n```\n---\n\n",
            global_content
        ));
    }

    // Read workspace alignment file
    if folder_to_start.exists() {
        if let Some((fname, ws_content)) = read_workspace_alignment_file(&folder_to_start) {
            alignment_context.push_str(&format!(
                "---\n# {} - user shared project guidelines to follow\n```\n{}\n```\n---\n\n",
                fname, ws_content
            ));
        }
    }

    if !alignment_context.is_empty() {
        response.push_str(&alignment_context);
    }

    // Read initial files and include content (WCGW-style)
    if !initialize.initial_files_to_read.is_empty() {
        let (files_content, _file_ranges) =
            read_initial_files(&initialize.initial_files_to_read, &folder_to_start);
        if !files_content.is_empty() {
            response.push_str(&format!(
                "---\n# Requested files\nHere are the contents of the requested files:\n{}\n---\n",
                files_content
            ));
        }
    }

    // Get repository context information if available
    let repo_context = if folder_to_start.exists() {
        match get_repo_context(&folder_to_start) {
            Ok((context, _)) => {
                debug!("Successfully generated repository context");
                format!("\n---\n{}\n", context)
            }
            Err(e) => {
                warn!("Failed to generate repository context: {}", e);
                String::new()
            }
        }
    } else {
        String::new()
    };

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
            "\n# Environment\nSystem: {}\nMachine: {}\nInitialized in directory (also cwd): {:?}\nUser home directory: {}\n",
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
    response.push_str(&repo_context);

    // Add thread ID instruction
    if let Some(ref bash_state) = *bash_state_guard {
        info!(
            "Final response will use thread_id: {}",
            bash_state.current_thread_id
        );

        response.push_str(&format!(
            "\nUse thread_id={} for all winx tool calls which take that.\n",
            bash_state.current_thread_id
        ));
    }

    // Add note about additional important instructions
    response.push_str("\n- Additional important note: as soon as you encounter \"The user has chosen to disallow the tool call.\", immediately stop doing everything and ask user for the reason.\n");

    info!("Initialize tool call completed successfully");
    debug!("Final response: {}", response);

    Ok(response)
}
