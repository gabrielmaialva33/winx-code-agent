//! Implementation of the Initialize tool.

use std::collections::VecDeque;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tokio::sync::Mutex;
use tracing::{info, instrument, warn};

use crate::errors::{Result, WinxError};
use crate::runtime::{EmbeddedShellRuntime, ShellRuntime, ShellSessionTransition};
use crate::state::bash_state::{generate_thread_id, BashState};
use crate::types::{
    normalize_thread_id, AllowedCommands, AllowedGlobs, BashCommandMode, BashMode,
    CodeWriterConfig, FileEditMode, Initialize, InitializeType, Modes, WriteIfEmptyMode,
};
use crate::utils::mmap::read_file_to_string;
use crate::utils::path::{ensure_directory_exists, expand_user, validate_path_in_workspace};

const POLICY_WARNING_CACHE_CAPACITY: usize = 128;
static WARNED_POLICY_CONFIGURATIONS: OnceLock<StdMutex<VecDeque<Vec<String>>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InitializeTransition {
    Created,
    AttachedExisting,
    ModeChanged,
    ShellReset,
    WorkspaceChanged,
}

impl InitializeTransition {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::AttachedExisting => "attached_existing",
            Self::ModeChanged => "mode_changed",
            Self::ShellReset => "shell_reset",
            Self::WorkspaceChanged => "workspace_changed",
        }
    }
}

#[derive(Debug)]
pub(crate) struct InitializeOutcome {
    pub(crate) text: String,
    pub(crate) transition: InitializeTransition,
    pub(crate) context_bytes: usize,
    pub(crate) guidelines_bytes: usize,
    pub(crate) initial_files_count: usize,
    pub(crate) compact_response: bool,
    pub(crate) code_writer_policy_strength: Option<&'static str>,
    pub(crate) shell_spawners_present: bool,
    pub(crate) temporary_artifact_dir: PathBuf,
    pub(crate) temporary_artifact_ttl_seconds: u64,
    pub(crate) temporary_artifact_max_bytes: u64,
    pub(crate) temporary_artifact_max_file_bytes: u64,
}

/// Create a unique scratch workspace under the system temp dir, used when the
/// caller initializes without a workspace path.
fn create_playground_dir(thread_id: &str) -> Result<PathBuf> {
    #[cfg(unix)]
    let owner = crate::os::unix::effective_uid().to_string();
    #[cfg(not(unix))]
    let owner = std::process::id().to_string();

    let dir = std::env::temp_dir().join(format!("winx-playground-{owner}-{thread_id}"));
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
                allowed_commands: AllowedCommands::List(
                    crate::utils::bash_parser::architect_allowed_commands(),
                ),
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

fn active_code_writer_config(state: &BashState) -> Option<CodeWriterConfig> {
    (state.mode == Modes::CodeWriter).then(|| CodeWriterConfig {
        allowed_globs: state.file_edit_mode.allowed_globs.clone(),
        allowed_commands: state.bash_command_mode.allowed_commands.clone(),
    })
}

fn active_code_writer_policy(state: &BashState) -> (Option<&'static str>, Vec<String>) {
    if state.mode != Modes::CodeWriter {
        return (None, Vec::new());
    }

    match &state.bash_command_mode.allowed_commands {
        AllowedCommands::All(value) if value == "all" => (Some("unrestricted"), Vec::new()),
        AllowedCommands::All(_) => (Some("restricted"), Vec::new()),
        AllowedCommands::List(commands) => {
            let bypass = crate::utils::bash_parser::detect_allowlist_bypass(commands);
            let strength = if bypass.is_empty() { "restricted" } else { "weak" };
            (Some(strength), bypass)
        }
    }
}

fn warn_code_writer_policy_once(state: &BashState, bypass: &[String]) {
    if bypass.is_empty() {
        return;
    }
    let AllowedCommands::List(commands) = &state.bash_command_mode.allowed_commands else {
        return;
    };

    let cache = WARNED_POLICY_CONFIGURATIONS
        .get_or_init(|| StdMutex::new(VecDeque::with_capacity(POLICY_WARNING_CACHE_CAPACITY)));
    let mut cache = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.iter().any(|seen| seen == commands) {
        return;
    }
    if cache.len() == POLICY_WARNING_CACHE_CAPACITY {
        cache.pop_front();
    }
    cache.push_back(commands.clone());
    drop(cache);

    warn!(
        commands = ?bypass,
        "code_writer allowlist includes shell-spawning commands; the command allowlist is \
         effectively bypassable and does not sandbox the agent"
    );
}

fn append_code_writer_policy_warning(response: &mut String, bypass: &[String]) {
    if bypass.is_empty() {
        return;
    }
    let _ = writeln!(
        response,
        "\n⚠️  SECURITY: code_writer allowlist includes shell/eval commands ({}). \
         They execute arbitrary code from string arguments (e.g. `bash -c …`, \
         `find -exec …`), so the command allowlist is effectively bypassable and \
         does NOT sandbox the agent. Drop them if you intended a hard restriction.",
        bypass.join(", ")
    );
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

fn prepare_workspace(
    initialize: &Initialize,
    thread_id: &str,
    response: &mut String,
) -> Result<PathBuf> {
    let workspace_path_str = expand_user(&initialize.any_workspace_path);
    if workspace_path_str.is_empty() {
        // wcgw parity: no path given → spin up a scratch playground instead of
        // forcing the agent to always supply a workspace.
        let playground = create_playground_dir(thread_id)?;
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

pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    initialize: Initialize,
) -> Result<String> {
    handle_tool_call_with_runtime(&EmbeddedShellRuntime, bash_state_arc, initialize).await
}

pub async fn handle_tool_call_with_runtime(
    runtime: &dyn ShellRuntime,
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    initialize: Initialize,
) -> Result<String> {
    Ok(handle_tool_call_with_runtime_detailed(runtime, bash_state_arc, initialize).await?.text)
}

#[instrument(level = "info", skip(runtime, bash_state_arc, initialize))]
#[allow(clippy::too_many_lines)]
pub(crate) async fn handle_tool_call_with_runtime_detailed(
    runtime: &dyn ShellRuntime,
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    initialize: Initialize,
) -> Result<InitializeOutcome> {
    let mut response = String::new();
    let mut context_bytes = 0;
    let mut guidelines_bytes = 0;
    let initial_files_count = initialize.initial_files_to_read.len();

    info!("Initialize called for workspace: {}", initialize.any_workspace_path);

    validate_thread_id(&initialize)?;
    let thread_id = initialize_thread_id(&initialize);
    let mut bash_state_guard = bash_state_arc.lock().await;
    if initialize.init_type != InitializeType::FirstCall && bash_state_guard.is_none() {
        return Err(WinxError::BashStateNotInitialized);
    }
    let local_attach = initialize.init_type == InitializeType::FirstCall
        && bash_state_guard
            .as_ref()
            .is_some_and(|state| state.initialized && state.current_thread_id == thread_id);
    let folder_to_start = if local_attach {
        bash_state_guard
            .as_ref()
            .map_or_else(|| PathBuf::from("/tmp"), |state| state.workspace_root.clone())
    } else {
        prepare_workspace(&initialize, &thread_id, &mut response)?
    };
    let requested_mode = Modes::from(&initialize.mode_name);
    let preserve_active_mode =
        local_attach || initialize.init_type == InitializeType::UserAskedChangeWorkspace;
    let (mode, bash_command_mode, file_edit_mode, write_if_empty_mode) = if preserve_active_mode {
        let Some(state) = bash_state_guard.as_ref() else {
            return Err(WinxError::BashStateNotInitialized);
        };
        (
            state.mode,
            state.bash_command_mode.clone(),
            state.file_edit_mode.clone(),
            state.write_if_empty_mode.clone(),
        )
    } else {
        let (bash_command_mode, file_edit_mode, write_if_empty_mode) = mode_to_state(
            requested_mode,
            initialize.code_writer_config.as_ref(),
            &folder_to_start,
        )?;
        (requested_mode, bash_command_mode, file_edit_mode, write_if_empty_mode)
    };

    let transition;
    let mut compact_response = false;

    match initialize.init_type {
        InitializeType::FirstCall => {
            let configured = if local_attach {
                let Some(state) = bash_state_guard.as_mut() else {
                    return Err(WinxError::BashStateNotInitialized);
                };
                // Refresh guardian activity without resetting the PTY. Reusing the
                // current snapshot as a mode transition is also compatible with
                // protocol-1.2 guardians that predate attach-or-create.
                let mut configured =
                    runtime.configure_session(state, ShellSessionTransition::ModeChange).await?;
                configured.attached_existing = true;
                configured
            } else {
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
                let configured = runtime
                    .configure_session(&mut new_bash_state, ShellSessionTransition::FirstCall)
                    .await?;
                *bash_state_guard = Some(new_bash_state);
                configured
            };

            transition = if configured.attached_existing {
                InitializeTransition::AttachedExisting
            } else {
                InitializeTransition::Created
            };
            compact_response = configured.attached_existing
                && initialize.initial_files_to_read.is_empty()
                && initialize.task_id_to_resume.is_empty();

            if compact_response {
                response.clear();
                let state = bash_state_guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
                let _ = writeln!(
                    response,
                    "Attached to the existing durable Winx session; PTY, cwd, mode, and prior \
                     context were preserved.\ncwd={}\nmode={}",
                    state.cwd.display(),
                    state.mode
                );
                if let Some(attach_hint) = configured.attach_hint.as_deref() {
                    let _ = writeln!(response, "Attach terminal: {attach_hint}");
                }
                response.push_str(
                    "Context and instructions are unchanged. Continue with the existing thread; \
                     use an explicit mode/workspace change or reset only when intended.\n",
                );
            } else {
                if configured.attached_existing {
                    response.push_str(
                        "\nAttached to the existing durable session for this principal/workspace; \
                         the guardian-owned PTY and cwd were preserved. Use `reset_shell` or an \
                         explicit workspace/mode change when replacement is intended.\n",
                    );
                }

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

                if let Some(attach_hint) = configured.attach_hint.as_deref() {
                    let _ = writeln!(response, "\nAttach terminal: {attach_hint}");
                }

                // Explain the actual active policy. A daemon reattach can restore a
                // different mode than the model redundantly supplied.
                let active_config = bash_state_guard.as_ref().and_then(active_code_writer_config);
                let active_mode = bash_state_guard.as_ref().map_or(mode, |state| state.mode);
                let _ = writeln!(
                    response,
                    "\n{}",
                    crate::utils::mode_prompts::mode_prompt(active_mode, active_config.as_ref())
                );

                let active_workspace = bash_state_guard
                    .as_ref()
                    .map_or(folder_to_start.as_path(), |state| state.workspace_root.as_path());

                let guidelines = load_guidelines(active_workspace);
                guidelines_bytes = guidelines.len();
                if !guidelines.is_empty() {
                    let _ = writeln!(response, "\n# Agent guidelines\n{guidelines}");
                }

                if let Ok((repo_context, _)) =
                    crate::utils::repo::get_repo_context(active_workspace)
                {
                    context_bytes = repo_context.len();
                    let _ = writeln!(response, "\n# Workspace structure\n{repo_context}");
                }

                if !initialize.initial_files_to_read.is_empty() {
                    let content = read_initial_files_simple(
                        &initialize.initial_files_to_read,
                        active_workspace,
                    );
                    if !content.is_empty() {
                        let _ = writeln!(response, "\n# Requested files\n{content}");
                    }
                }
            }
        }
        InitializeType::UserAskedModeChange => {
            transition = InitializeTransition::ModeChanged;
            if let Some(state) = bash_state_guard.as_mut() {
                state.mode = mode;
                state.bash_command_mode = bash_command_mode;
                state.file_edit_mode = file_edit_mode;
                state.write_if_empty_mode = write_if_empty_mode;
                runtime.configure_session(state, ShellSessionTransition::ModeChange).await?;
                let _ = writeln!(response, "Changed mode to: {mode:?}");
                let active_config = active_code_writer_config(state);
                let _ = writeln!(
                    response,
                    "\n{}",
                    crate::utils::mode_prompts::mode_prompt(mode, active_config.as_ref())
                );
            } else {
                return Err(WinxError::BashStateNotInitialized);
            }
        }
        InitializeType::ResetShell => {
            transition = InitializeTransition::ShellReset;
            if let Some(state) = bash_state_guard.as_mut() {
                state.mode = mode;
                state.bash_command_mode = bash_command_mode;
                state.file_edit_mode = file_edit_mode;
                state.write_if_empty_mode = write_if_empty_mode;
                runtime.configure_session(state, ShellSessionTransition::Reset).await?;
                response.push_str("Reset shell (new PTY created)\n");
            } else {
                return Err(WinxError::BashStateNotInitialized);
            }
        }
        InitializeType::UserAskedChangeWorkspace => {
            transition = InitializeTransition::WorkspaceChanged;
            if let Some(state) = bash_state_guard.as_mut() {
                if folder_to_start.exists() {
                    state.update_cwd(&folder_to_start)?;
                    state.update_workspace_root(&folder_to_start)?;
                    runtime
                        .configure_session(state, ShellSessionTransition::WorkspaceChange)
                        .await?;
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

    let state = bash_state_guard.as_ref().ok_or(WinxError::BashStateNotInitialized)?;
    let temporary_artifact = if compact_response {
        crate::utils::agent_temp::session_info(&state.workspace_root, &state.current_thread_id)
    } else {
        crate::utils::agent_temp::prepare_session(&state.workspace_root, &state.current_thread_id)
    };
    let temporary_artifact_instruction = if compact_response {
        format!(
            "Use temporary_artifact_dir={} for session-local derived helpers; BashCommand exports \
             the same path as WINX_TEMP_DIR.",
            temporary_artifact.directory.display()
        )
    } else {
        format!(
            "Use temporary_artifact_dir={} for session-local derived helpers only; keep names \
             short, preserve source-path/line provenance, and treat helpers as non-canonical. The \
             directory is created on demand and expired after {} seconds of inactivity. Every \
             BashCommand PTY exports the same path as WINX_TEMP_DIR; shell-generated helpers must \
             stay beneath it.",
            temporary_artifact.directory.display(),
            temporary_artifact.ttl_seconds,
        )
    };
    let _ = writeln!(
        response,
        "\nUse thread_id={thread_id} for all winx tool calls.\nUse workspace_root={} for all winx tool calls.\n{temporary_artifact_instruction}\nBefore every reuse, confirm workspace_root still matches the user's current project. Keep this exact pair together. workspace_root identifies this project session; it does not restrict target paths allowed by policy.",
        state.workspace_root.display(),
    );

    let (code_writer_policy_strength, bypass) =
        bash_state_guard.as_ref().map_or((None, Vec::new()), active_code_writer_policy);
    if let Some(state) = bash_state_guard.as_ref() {
        warn_code_writer_policy_once(state, &bypass);
    }
    if compact_response {
        if let Some(strength) = code_writer_policy_strength {
            let _ = writeln!(
                response,
                "code_writer_policy={strength} shell_spawners_present={}",
                !bypass.is_empty()
            );
        }
    } else {
        append_code_writer_policy_warning(&mut response, &bypass);
        crate::utils::orchestration::append_initialize_instructions(&mut response);
    }

    Ok(InitializeOutcome {
        text: response,
        transition,
        context_bytes,
        guidelines_bytes,
        initial_files_count,
        compact_response,
        code_writer_policy_strength,
        shell_spawners_present: !bypass.is_empty(),
        temporary_artifact_dir: temporary_artifact.directory,
        temporary_artifact_ttl_seconds: temporary_artifact.ttl_seconds,
        temporary_artifact_max_bytes: temporary_artifact.max_total_bytes,
        temporary_artifact_max_file_bytes: temporary_artifact.max_file_bytes,
    })
}
