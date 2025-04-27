//! Initialize tool implementation
//!
//! This module contains the implementation of the Initialize tool, which is used
//! to set up the bash environment with a specified workspace path and mode.

use crate::bash::{
    expand_user, AllowedCommandsType, AllowedGlobsType, BashCommandMode, BashMode, BashState,
    Context, FileEditMode, SimpleConsole, WriteIfEmptyMode,
};
use crate::error::{WinxError, WinxResult};
use crate::types::{AllowedCommands, AllowedGlobs, Initialize, InitializeType, Mode};
use std::fs;
use std::path::Path;

/// Initialize a bash environment with the given parameters
///
/// This function sets up a bash environment with the specified workspace path and mode.
/// It can be used to initialize a new environment or to modify an existing one.
///
/// # Arguments
///
/// * `ctx` - The context containing the bash state
/// * `init` - The initialization parameters
///
/// # Returns
///
/// A result containing a string with information about the initialization
pub async fn initialize_tool(ctx: &Context, init: &Initialize) -> WinxResult<String> {
    let workspace_path = expand_user(&init.any_workspace_path);

    // Ensure the workspace path exists
    if !workspace_path.is_empty() {
        if let Some(parent) = Path::new(&workspace_path).parent() {
            fs::create_dir_all(parent).map_err(|e| WinxError::Io(e))?;
        }
    }

    let mut output = String::new();

    match init.r#type {
        InitializeType::FirstCall => {
            // Create a new bash state
            let bash_command_mode = match &init.mode_name {
                Mode::Wcgw => BashCommandMode {
                    bash_mode: BashMode::NormalMode,
                    allowed_commands: AllowedCommandsType::All,
                },
                Mode::Architect => BashCommandMode {
                    bash_mode: BashMode::RestrictedMode,
                    allowed_commands: AllowedCommandsType::All,
                },
                Mode::CodeWriter => {
                    if let Some(ref config) = init.code_writer_config {
                        BashCommandMode {
                            bash_mode: BashMode::NormalMode,
                            allowed_commands: match &config.allowed_commands {
                                AllowedCommands::All(_) => AllowedCommandsType::All,
                                AllowedCommands::Specific(_) => AllowedCommandsType::None,
                            },
                        }
                    } else {
                        BashCommandMode {
                            bash_mode: BashMode::NormalMode,
                            allowed_commands: AllowedCommandsType::All,
                        }
                    }
                }
            };

            let file_edit_mode = match &init.mode_name {
                Mode::Wcgw => FileEditMode {
                    allowed_globs: AllowedGlobsType::All,
                },
                Mode::Architect => FileEditMode {
                    allowed_globs: AllowedGlobsType::Specific(vec![]),
                },
                Mode::CodeWriter => {
                    if let Some(ref config) = init.code_writer_config {
                        FileEditMode {
                            allowed_globs: match &config.allowed_globs {
                                AllowedGlobs::All(_) => AllowedGlobsType::All,
                                AllowedGlobs::Specific(globs) => {
                                    AllowedGlobsType::Specific(globs.clone())
                                }
                            },
                        }
                    } else {
                        FileEditMode {
                            allowed_globs: AllowedGlobsType::All,
                        }
                    }
                }
            };

            let write_if_empty_mode = match &init.mode_name {
                Mode::Wcgw => WriteIfEmptyMode {
                    allowed_globs: AllowedGlobsType::All,
                },
                Mode::Architect => WriteIfEmptyMode {
                    allowed_globs: AllowedGlobsType::Specific(vec![]),
                },
                Mode::CodeWriter => {
                    if let Some(ref config) = init.code_writer_config {
                        WriteIfEmptyMode {
                            allowed_globs: match &config.allowed_globs {
                                AllowedGlobs::All(_) => AllowedGlobsType::All,
                                AllowedGlobs::Specific(globs) => {
                                    AllowedGlobsType::Specific(globs.clone())
                                }
                            },
                        }
                    } else {
                        WriteIfEmptyMode {
                            allowed_globs: AllowedGlobsType::All,
                        }
                    }
                }
            };

            let chat_id = if init.chat_id.is_empty() {
                crate::bash::generate_chat_id()
            } else {
                init.chat_id.clone()
            };

            let bash_state = BashState::new(
                Box::new(SimpleConsole),
                &workspace_path,
                Some(bash_command_mode),
                Some(file_edit_mode),
                Some(write_if_empty_mode),
                Some(init.mode_name.clone()),
                Some(chat_id),
            )
            .map_err(|e| WinxError::ServiceInitialization(e.to_string()))?;

            // Replace the current bash state with the new one
            {
                let mut state = ctx
                    .bash_state
                    .lock()
                    .map_err(|_| WinxError::Unknown("Failed to lock bash state".to_string()))?;
                *state = bash_state;
            }

            // Read initial files if any
            if !init.initial_files_to_read.is_empty() {
                output.push_str("Reading initial files...\n");
                for file_path in &init.initial_files_to_read {
                    output.push_str(&format!("- {}\n", file_path));
                }
            }

            // Add mode-specific information to output
            match init.mode_name {
                Mode::Wcgw => {
                    output.push_str("\n# Instructions\n\n");
                    output.push_str("- You should use the provided bash execution, reading and writing file tools to complete objective.\n");
                    output.push_str("- Do not provide code snippets unless asked by the user, instead directly add/edit the code.\n");
                    output.push_str("- Do not install new tools/packages before ensuring no such tools/package or an alternative already exists.\n");
                }
                Mode::Architect => {
                    output.push_str("\n# Instructions\n\n");
                    output.push_str("You are now running in \"architect\" mode. This means\n");
                    output.push_str("- You are not allowed to edit or update any file. You are not allowed to create any file.\n");
                    output.push_str("- You are not allowed to run any commands that may change disk, system configuration, packages or environment.\n");
                }
                Mode::CodeWriter => {
                    output.push_str("\n# Instructions\n\n");
                    output.push_str("You are now running in \"code_writer\" mode.\n");
                    if let Some(config) = &init.code_writer_config {
                        match &config.allowed_globs {
                            AllowedGlobs::All(_) => {
                                output.push_str(
                                    "- You are allowed to edit files in the provided repository.\n",
                                );
                            }
                            AllowedGlobs::Specific(globs) => {
                                output.push_str(&format!("- You are allowed to edit files only matching these globs: {:?}\n", globs));
                            }
                        }
                    }
                }
            }

            // Add environment information
            output.push_str("\n# Environment\n");
            output.push_str(&format!("System: {}\n", std::env::consts::OS));
            output.push_str(&format!("Machine: {}\n", std::env::consts::ARCH));
            output.push_str(&format!(
                "Initialized in directory (also cwd): {}\n",
                workspace_path
            ));
            if let Some(home_dir) = home::home_dir() {
                output.push_str(&format!("User home directory: {}\n", home_dir.display()));
            }

            // Add chat ID
            let state = ctx
                .bash_state
                .lock()
                .map_err(|_| WinxError::Unknown("Failed to lock bash state".to_string()))?;
            output.push_str(&format!(
                "\n---\n\nUse chat_id={} for all winx tool calls which take that.\n",
                state.current_chat_id
            ));

            // Add additional note
            output.push_str("\n- Additional important note: as soon as you encounter \"The user has chosen to disallow the tool call.\", immediately stop doing everything and ask user for the reason.\n");
        }
        InitializeType::UserAskedModeChange => {
            // Change the mode of the existing bash state
            let mut state = ctx
                .bash_state
                .lock()
                .map_err(|_| WinxError::Unknown("Failed to lock bash state".to_string()))?;
            state.mode = init.mode_name.clone();
            output.push_str(&format!("Mode changed to {:?}\n", init.mode_name));
        }
        InitializeType::ResetShell => {
            // Reset the shell
            let mut state = ctx
                .bash_state
                .lock()
                .map_err(|_| WinxError::Unknown("Failed to lock bash state".to_string()))?;
            state
                .init_shell()
                .map_err(|e| WinxError::ServiceInitialization(e.to_string()))?;
            output.push_str("Shell reset successful\n");
        }
        InitializeType::UserAskedChangeWorkspace => {
            // Change the workspace
            let mut state = ctx
                .bash_state
                .lock()
                .map_err(|_| WinxError::Unknown("Failed to lock bash state".to_string()))?;
            state.cwd = workspace_path.clone();
            state.workspace_root = workspace_path;
            state
                .init_shell()
                .map_err(|e| WinxError::ServiceInitialization(e.to_string()))?;
            output.push_str(&format!("Workspace changed to {}\n", state.cwd));
        }
    }

    Ok(output)
}
