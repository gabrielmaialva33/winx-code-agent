//! Implementation of the Initialize tool.

use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, instrument, warn};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::{generate_thread_id, BashState};
use crate::types::{
    normalize_thread_id, AllowedCommands, AllowedGlobs, BashCommandMode, BashMode,
    CodeWriterConfig, FileEditMode, Initialize, InitializeType, ModeName, Modes, WriteIfEmptyMode,
};
use crate::utils::mmap::read_file_to_string;
use crate::utils::path::{ensure_directory_exists, expand_user, validate_path_in_workspace};

#[inline]
fn convert_mode_name(mode_name: &ModeName) -> Modes {
    match mode_name {
        ModeName::Wcgw => Modes::Wcgw,
        ModeName::Architect => Modes::Architect,
        ModeName::CodeWriter => Modes::CodeWriter,
    }
}

fn code_writer_state(
    config: &CodeWriterConfig,
    workspace_root: &Path,
) -> (BashCommandMode, FileEditMode, WriteIfEmptyMode) {
    let mut config = config.clone();
    config.update_relative_globs(&workspace_root.to_string_lossy());

    (
        BashCommandMode {
            bash_mode: BashMode::NormalMode,
            allowed_commands: config.allowed_commands,
        },
        FileEditMode { allowed_globs: config.allowed_globs.clone() },
        WriteIfEmptyMode { allowed_globs: config.allowed_globs },
    )
}

fn mode_to_state(
    mode: Modes,
    config: Option<&CodeWriterConfig>,
    workspace_root: &Path,
) -> Result<(BashCommandMode, FileEditMode, WriteIfEmptyMode)> {
    match mode {
        Modes::Wcgw => Ok((
            BashCommandMode {
                bash_mode: BashMode::NormalMode,
                allowed_commands: AllowedCommands::All("all".to_string()),
            },
            FileEditMode { allowed_globs: AllowedGlobs::All("all".to_string()) },
            WriteIfEmptyMode { allowed_globs: AllowedGlobs::All("all".to_string()) },
        )),
        Modes::Architect => Ok((
            BashCommandMode {
                bash_mode: BashMode::RestrictedMode,
                allowed_commands: AllowedCommands::All("all".to_string()),
            },
            FileEditMode { allowed_globs: AllowedGlobs::List(vec![]) },
            WriteIfEmptyMode { allowed_globs: AllowedGlobs::List(vec![]) },
        )),
        Modes::CodeWriter => {
            let config = config.ok_or_else(|| {
                WinxError::ArgumentParseError(
                    "code_writer_config is required when mode_name is code_writer.".to_string(),
                )
            })?;
            Ok(code_writer_state(config, workspace_root))
        }
    }
}

fn read_initial_files_simple(files: &[String], workspace: &std::path::Path) -> String {
    let mut output = String::new();
    for file_path in files {
        let expanded = expand_user(file_path);
        let path = if std::path::Path::new(&expanded).is_absolute() {
            PathBuf::from(&expanded)
        } else {
            workspace.join(&expanded)
        };

        if let Ok(validated) = validate_path_in_workspace(&path, workspace) {
            if validated.exists() && validated.is_file() {
                if let Ok(content) = read_file_to_string(&validated, 10_000_000) {
                    let _ = write!(output, "\n{file_path}\n```\n{content}\n```\n");
                }
            }
        }
    }
    output
}

fn prepare_workspace(initialize: &Initialize, response: &mut String) -> Result<PathBuf> {
    let workspace_path_str = expand_user(&initialize.any_workspace_path);
    if workspace_path_str.is_empty() {
        return Err(WinxError::WorkspacePathError("Workspace path cannot be empty.".to_string()));
    }

    let workspace_path = PathBuf::from(&workspace_path_str);
    let mut folder_to_start = workspace_path.clone();

    if workspace_path.exists() {
        if workspace_path.is_file() {
            folder_to_start = workspace_path.parent().unwrap_or(&workspace_path).to_path_buf();
            let _ =
                writeln!(response, "Using parent directory of file: {}", folder_to_start.display());
        } else if workspace_path.is_dir() {
            let _ = writeln!(response, "Using workspace directory: {}", folder_to_start.display());
        }
    } else if workspace_path.is_absolute() {
        ensure_directory_exists(&workspace_path).map_err(|e| {
            WinxError::WorkspacePathError(format!("Failed to create workspace: {e}"))
        })?;
        let _ = writeln!(response, "Created workspace directory: {}", workspace_path.display());
    }

    // Canonicalize so downstream comparisons (workspace checks, glob prefixes) match
    // paths that were canonicalized via fs::canonicalize — important on macOS where
    // /var, /tmp etc. are symlinks to /private/var, /private/tmp.
    if folder_to_start.exists() {
        if let Ok(canonical) = folder_to_start.canonicalize() {
            folder_to_start = canonical;
        }
    }

    Ok(folder_to_start)
}

fn initialize_thread_id(initialize: &Initialize) -> String {
    let thread_id = normalize_thread_id(&initialize.thread_id);
    if thread_id.is_empty() {
        generate_thread_id()
    } else {
        thread_id
    }
}

fn validate_thread_id(initialize: &Initialize) -> Result<()> {
    if initialize.init_type != InitializeType::FirstCall
        && normalize_thread_id(&initialize.thread_id).is_empty()
    {
        return Err(WinxError::ThreadIdMismatch(
            "Thread id should be provided if type != 'first_call', including when resetting."
                .to_string(),
        ));
    }

    Ok(())
}

#[instrument(level = "info", skip(bash_state_arc, initialize))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    initialize: Initialize,
) -> Result<String> {
    let mut response = String::new();

    info!("Initialize called for workspace: {}", initialize.any_workspace_path);

    validate_thread_id(&initialize)?;
    let folder_to_start = prepare_workspace(&initialize, &mut response)?;
    let thread_id = initialize_thread_id(&initialize);

    let mut bash_state_guard = bash_state_arc.lock().await;
    let mode = convert_mode_name(&initialize.mode_name);
    let (bash_command_mode, file_edit_mode, write_if_empty_mode) =
        mode_to_state(mode, initialize.code_writer_config.as_ref(), &folder_to_start)?;

    match initialize.init_type {
        InitializeType::FirstCall => {
            let mut new_bash_state = BashState::new();
            new_bash_state.current_thread_id.clone_from(&thread_id);
            new_bash_state.mode = mode;
            new_bash_state.bash_command_mode = bash_command_mode;
            new_bash_state.file_edit_mode = file_edit_mode;
            new_bash_state.write_if_empty_mode = write_if_empty_mode;
            new_bash_state.initialized = true;

            if folder_to_start.exists() {
                new_bash_state.update_cwd(&folder_to_start)?;
                new_bash_state.update_workspace_root(&folder_to_start)?;
                new_bash_state.init_pty_shell().await?;
            }

            *bash_state_guard = Some(new_bash_state);

            let _ = write!(
                response,
                "\n# Environment\nSystem: {}\nMachine: {}\nInitialized in directory: {}\n",
                std::env::consts::OS,
                std::env::consts::ARCH,
                folder_to_start.display()
            );

            let _ = writeln!(response, "\nUse thread_id={thread_id} for all winx tool calls.");

            if let Ok((repo_context, _)) = crate::utils::repo::get_repo_context(&folder_to_start) {
                let _ = writeln!(response, "\n# Workspace structure\n{repo_context}");
            }

            if !initialize.initial_files_to_read.is_empty() {
                let content =
                    read_initial_files_simple(&initialize.initial_files_to_read, &folder_to_start);
                if !content.is_empty() {
                    let _ = writeln!(response, "\n# Requested files\n{content}");
                }
            }
        }
        InitializeType::UserAskedModeChange => {
            if let Some(state) = bash_state_guard.as_mut() {
                state.mode = mode;
                state.bash_command_mode = bash_command_mode;
                state.file_edit_mode = file_edit_mode;
                state.write_if_empty_mode = write_if_empty_mode;
                let _ = writeln!(response, "Changed mode to: {mode:?}");
            } else {
                return Err(WinxError::BashStateNotInitialized);
            }
        }
        InitializeType::ResetShell => {
            if let Some(state) = bash_state_guard.as_mut() {
                state.mode = mode;
                state.bash_command_mode = bash_command_mode;
                state.file_edit_mode = file_edit_mode;
                state.write_if_empty_mode = write_if_empty_mode;
                state.init_pty_shell().await?;
                response.push_str("Reset shell (new PTY created)\n");
            } else {
                return Err(WinxError::BashStateNotInitialized);
            }
        }
        InitializeType::UserAskedChangeWorkspace => {
            if let Some(state) = bash_state_guard.as_mut() {
                if folder_to_start.exists() {
                    state.update_cwd(&folder_to_start)?;
                    state.update_workspace_root(&folder_to_start)?;
                    let _ =
                        writeln!(response, "Changed workspace to: {}", folder_to_start.display());
                } else {
                    let _ = writeln!(
                        response,
                        "Warning: Workspace path {} does not exist",
                        folder_to_start.display()
                    );
                }
            } else {
                return Err(WinxError::BashStateNotInitialized);
            }
        }
    }

    Ok(response)
}
