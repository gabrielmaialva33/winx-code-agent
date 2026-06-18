//! Implementation of the Initialize tool.

use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, instrument, warn};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::{generate_thread_id, BashState};
use crate::types::{
    normalize_thread_id, AllowedCommands, AllowedGlobs, BashCommandMode, BashMode,
    CodeWriterConfig, FileEditMode, Initialize, InitializeType, Modes, WriteIfEmptyMode,
};
use crate::utils::mmap::read_file_to_string;
use crate::utils::path::{ensure_directory_exists, expand_user, validate_path_in_workspace};

/// Create a unique scratch workspace under the system temp dir, used when the
/// caller initializes without a workspace path.
fn create_playground_dir() -> Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir =
        std::env::temp_dir().join(format!("winx-playground-{}-{:x}", std::process::id(), stamp));
    ensure_directory_exists(&dir)?;
    Ok(dir)
}

/// Whether `cmd` is on PATH (best-effort, used only for advisory hints).
fn command_exists(cmd: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("command -v {cmd}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn code_writer_state(
    config: &CodeWriterConfig,
    workspace_root: &Path,
) -> (BashCommandMode, FileEditMode, WriteIfEmptyMode) {
    let mut config = config.clone();
    // Forgive the common `["all"]` mistake before turning relative globs absolute.
    config.allowed_globs.normalize();
    config.allowed_commands.normalize();
    config.update_relative_globs(&workspace_root.to_string_lossy());

    if let AllowedCommands::List(cmds) = &config.allowed_commands {
        let bypass = crate::utils::bash_parser::detect_allowlist_bypass(cmds);
        if !bypass.is_empty() {
            warn!(
                commands = ?bypass,
                "code_writer allowlist includes shell-spawning commands; the command \
                 allowlist is effectively bypassable and does not sandbox the agent"
            );
        }
    }

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
        // wcgw parity: no path given → spin up a scratch playground instead of
        // forcing the agent to always supply a workspace.
        let playground = create_playground_dir()?;
        let _ = writeln!(
            response,
            "No workspace path provided; created a playground at {}",
            playground.display()
        );
        return Ok(playground);
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

fn load_guidelines(workspace: &Path) -> String {
    let mut output = String::new();
    let mut candidates = Vec::new();
    if let Some(home) = home::home_dir() {
        candidates.push(home.join(".winx").join("AGENTS.md"));
        candidates.push(home.join(".winx").join("CLAUDE.md"));
        candidates.push(home.join(".wcgw").join("AGENTS.md"));
        candidates.push(home.join(".wcgw").join("CLAUDE.md"));
    }
    candidates.push(workspace.join("AGENTS.md"));
    candidates.push(workspace.join("CLAUDE.md"));

    for path in candidates {
        if path.is_file() {
            if let Ok(content) = fs::read_to_string(&path) {
                let _ = writeln!(output, "\n## {}\n{}", path.display(), content);
            }
        }
    }
    output
}

#[instrument(level = "info", skip(bash_state_arc, initialize))]
#[allow(clippy::too_many_lines)]
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
    let mode = Modes::from(&initialize.mode_name);
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

            let resumed_context = if initialize.task_id_to_resume.is_empty() {
                None
            } else {
                crate::tools::context_save::load_saved_context(&initialize.task_id_to_resume)?
            };

            if let Some((memory_data, snapshot)) = &resumed_context {
                if let Some(snapshot) = snapshot {
                    new_bash_state.apply_snapshot(snapshot);
                    new_bash_state.current_thread_id.clone_from(&thread_id);
                }
                let _ = writeln!(
                    response,
                    "\n# Resumed task {}\nFollowing is the retrieved task context:\n{}",
                    initialize.task_id_to_resume, memory_data
                );
            }

            // A bash snapshot already carries cwd/workspace. Without one, prefer
            // the project root recorded in the resumed memory (so the agent lands
            // back in the right repo), then fall back to the provided folder.
            if resumed_context.as_ref().and_then(|(_, snapshot)| snapshot.as_ref()).is_none() {
                let resumed_root = resumed_context
                    .as_ref()
                    .and_then(|(memory, _)| {
                        crate::tools::context_save::extract_project_root(memory)
                    })
                    .filter(|root| root.exists());
                let target = resumed_root.as_deref().unwrap_or(folder_to_start.as_path());
                if target.exists() {
                    new_bash_state.update_cwd(target)?;
                    new_bash_state.update_workspace_root(target)?;
                }
            }
            if new_bash_state.cwd.exists() {
                new_bash_state.init_pty_shell().await?;
            }

            let attach_hint = {
                let pty_guard = new_bash_state.pty_shell.lock().await;
                pty_guard.as_ref().and_then(|shell| shell.attach_hint.clone())
            };

            *bash_state_guard = Some(new_bash_state);

            let _ = write!(
                response,
                "\n# Environment\nSystem: {}\nMachine: {}\nInitialized in directory: {}\n",
                std::env::consts::OS,
                std::env::consts::ARCH,
                bash_state_guard
                    .as_ref()
                    .map_or(folder_to_start.as_path(), |state| state.cwd.as_path())
                    .display()
            );

            if command_exists("rg") {
                let _ = writeln!(
                    response,
                    "\n# Available commands\nUse ripgrep `rg` instead of `grep`/`find -name` — \
                     it's much faster and respects .gitignore."
                );
            }

            let _ = writeln!(response, "\nUse thread_id={thread_id} for all winx tool calls.");
            if let Some(attach_hint) = attach_hint {
                let _ = writeln!(response, "\nAttach terminal: {attach_hint}");
            }

            // Inject the behavioral prompt for the active mode so the agent knows
            // how to behave (read-only / allowed globs / etc.) before its first
            // action, instead of discovering the rules by hitting enforcement errors.
            let _ = writeln!(
                response,
                "\n{}",
                crate::utils::mode_prompts::mode_prompt(
                    mode,
                    initialize.code_writer_config.as_ref()
                )
            );

            // Transparency: a code_writer command allowlist that includes a
            // shell/eval spawner is bypassable (`bash -c '...'`, `find -exec`),
            // so surface it in the response the model reads — here the allowlist
            // is a convenience filter, not a sandbox.
            if let Some(cfg) = initialize.code_writer_config.as_ref() {
                if let AllowedCommands::List(cmds) = &cfg.allowed_commands {
                    let bypass = crate::utils::bash_parser::detect_allowlist_bypass(cmds);
                    if !bypass.is_empty() {
                        let _ = writeln!(
                            response,
                            "\n⚠️  SECURITY: code_writer allowlist includes shell/eval commands ({}). \
                             They execute arbitrary code from string arguments (e.g. `bash -c …`, \
                             `find -exec …`), so the command allowlist is effectively bypassable and \
                             does NOT sandbox the agent. Drop them if you intended a hard restriction.",
                            bypass.join(", ")
                        );
                    }
                }
            }

            let active_workspace = bash_state_guard
                .as_ref()
                .map_or(folder_to_start.as_path(), |state| state.workspace_root.as_path());

            let guidelines = load_guidelines(active_workspace);
            if !guidelines.is_empty() {
                let _ = writeln!(response, "\n# Agent guidelines\n{guidelines}");
            }

            if let Ok((repo_context, _)) = crate::utils::repo::get_repo_context(active_workspace) {
                let _ = writeln!(response, "\n# Workspace structure\n{repo_context}");
            }

            if !initialize.initial_files_to_read.is_empty() {
                let content =
                    read_initial_files_simple(&initialize.initial_files_to_read, active_workspace);
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
                let _ = writeln!(
                    response,
                    "\n{}",
                    crate::utils::mode_prompts::mode_prompt(
                        mode,
                        initialize.code_writer_config.as_ref()
                    )
                );
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

    append_server_instructions(&mut response);

    Ok(response)
}

/// Append the standard "disallow" note plus any operator-provided instructions
/// from `WINX_SERVER_INSTRUCTIONS`, mirroring wcgw's Initialize output.
fn append_server_instructions(response: &mut String) {
    response.push_str(
        "\nAs soon as you encounter \"The user has chosen to disallow the tool call.\", \
         immediately stop doing everything and ask the user for the reason.\n",
    );
    if let Ok(extra) = std::env::var("WINX_SERVER_INSTRUCTIONS") {
        let extra = extra.trim();
        if !extra.is_empty() {
            let _ = write!(response, "\n# Additional instructions\n{extra}\n");
        }
    }
}
