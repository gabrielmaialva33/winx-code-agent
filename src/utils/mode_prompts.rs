//! WCGW-style mode prompts and instructions
//!
//! This module provides the exact prompt text and instructions used by WCGW
//! for different operational modes, ensuring consistent behavior between
//! the Python original and Rust implementation.

use crate::types::{AllowedCommands, AllowedGlobs, Modes};

/// Main WCGW prompt for full access mode
pub const WCGW_PROMPT: &str = r"
# Instructions

    - You should use the provided bash execution, reading and writing file tools to complete objective.
    - Do not provide code snippets unless asked by the user, instead directly add/edit the code.
    - Do not install new tools/packages before ensuring no such tools/package or an alternative already exists.
    - Do not use artifacts if you have access to the repository and not asked by the user to provide artifacts/snippets. Directly create/update using wcgw tools
    - Do not use Ctrl-c or interrupt commands without asking the user, because often the programs don't show any update but they still are running.
    - Do not use echo to write multi-line files, always use FileWriteOrEdit tool to update a code.
    - Provide as many file paths as you need in ReadFiles in one go.

Additional instructions:
    Always run `pwd` if you get any file or directory not found error to make sure you're not lost, or to get absolute cwd.

";

/// Architect mode prompt for read-only operations
pub const ARCHITECT_PROMPT: &str = r#"
# Instructions
You are now running in "architect" mode. This means
- You are not allowed to edit or update any file. You are not allowed to create any file. 
- You are not allowed to run any commands that may change disk, system configuration, packages or environment. Only read-only commands are allowed.
- Only run commands that allows you to explore the repository, understand the system or read anything of relevance. 
- Do not use Ctrl-c or interrupt commands without asking the user, because often the programs don't show any update but they still are running.
- You are not allowed to change directory (bash will run in -r mode)
- Share only snippets when any implementation is requested.
- Provide as many file paths as you need in ReadFiles in one go.

# Disallowed tools (important!)
- FileWriteOrEdit

# Response instructions
Respond only after doing the following:
- Read as many relevant files as possible. 
- Be comprehensive in your understanding and search of relevant files.
- First understand about the project by getting the folder structure (ignoring .git, node_modules, venv, etc.)
- Share minimal snippets higlighting the changes (avoid large number of lines in the snippets, use ... comments)
"#;

/// Common instructions for command execution
const COMMAND_COMMON_INSTRUCTIONS: &str = r"
    - Do not use Ctrl-c interrupt commands without asking the user, because often the programs don't show any update but they still are running.
    - Do not use echo to write multi-line files, always use FileWriteOrEdit tool to update a code.
    - Do not provide code snippets unless asked by the user, instead directly add/edit the code.
    - You should use the provided bash execution, reading and writing file tools to complete objective.
    - Do not use artifacts if you have access to the repository and not asked by the user to provide artifacts/snippets. Directly create/update using wcgw tools.
";

/// Generate code writer mode prompt with specific permissions
pub fn code_writer_prompt(
    allowed_file_edit_globs: &AllowedGlobs,
    allowed_write_new_globs: &AllowedGlobs,
    allowed_commands: &AllowedCommands,
) -> String {
    let mut prompt = String::from("\nYou are now running in \"code_writer\" mode.\n");

    // File editing permissions
    let edit_prompt = match allowed_file_edit_globs {
        AllowedGlobs::All(_) => {
            "    - You are allowed to edit files in the provided repository only.\n".to_string()
        }
        AllowedGlobs::List(globs) => {
            if globs.is_empty() {
                "    - You are not allowed to edit files.\n".to_string()
            } else {
                format!(
                    "    - You are allowed to edit files for files matching only the following globs: {}\n",
                    globs.join(", ")
                )
            }
        }
    };
    prompt.push_str(&edit_prompt);

    // File writing permissions
    let write_prompt = match allowed_write_new_globs {
        AllowedGlobs::All(_) => {
            "    - You are allowed to write files in the provided repository only.\n".to_string()
        }
        AllowedGlobs::List(globs) => {
            if globs.is_empty() {
                "    - You are not allowed to write files.\n".to_string()
            } else {
                format!(
                    "    - You are allowed to write files files matching only the following globs: {}\n",
                    globs.join(", ")
                )
            }
        }
    };
    prompt.push_str(&write_prompt);

    // Command execution permissions
    let command_prompt = match allowed_commands {
        AllowedCommands::All(_) => {
            format!(
                "    - You are only allowed to run commands for project setup, code writing, editing, updating, testing, running and debugging related to the project.\n    - Do not run anything that adds or removes packages, changes system configuration or environment.\n{COMMAND_COMMON_INSTRUCTIONS}"
            )
        }
        AllowedCommands::List(commands) => {
            if commands.is_empty() {
                "    - You are not allowed to run any commands.\n".to_string()
            } else {
                format!(
                    "    - You are only allowed to run the following commands: {}\n{}",
                    commands.join(", "),
                    COMMAND_COMMON_INSTRUCTIONS
                )
            }
        }
    };
    prompt.push_str(&command_prompt);

    prompt
}

/// Get the appropriate mode prompt based on the current mode
pub fn get_mode_prompt(
    mode: &Modes,
    allowed_file_edit_globs: Option<&AllowedGlobs>,
    allowed_write_new_globs: Option<&AllowedGlobs>,
    allowed_commands: Option<&AllowedCommands>,
) -> String {
    match mode {
        Modes::Wcgw => WCGW_PROMPT.to_string(),
        Modes::Architect => ARCHITECT_PROMPT.to_string(),
        Modes::CodeWriter => {
            // Create default values with longer lifetimes
            let default_edit_globs = AllowedGlobs::All("all".to_string());
            let default_write_globs = AllowedGlobs::All("all".to_string());
            let default_commands = AllowedCommands::All("all".to_string());

            let edit_globs = allowed_file_edit_globs.unwrap_or(&default_edit_globs);
            let write_globs = allowed_write_new_globs.unwrap_or(&default_write_globs);
            let commands = allowed_commands.unwrap_or(&default_commands);

            code_writer_prompt(edit_globs, write_globs, commands)
        }
    }
}

/// Enhanced mode instruction generator with WCGW context
pub fn generate_mode_instructions(
    mode: &Modes,
    project_context: Option<&str>,
    allowed_file_edit_globs: Option<&AllowedGlobs>,
    allowed_write_new_globs: Option<&AllowedGlobs>,
    allowed_commands: Option<&AllowedCommands>,
) -> String {
    let mut instructions = String::new();

    // Add project context if available
    if let Some(context) = project_context {
        instructions.push_str("# Project Context\n");
        instructions.push_str(context);
        instructions.push_str("\n\n");
    }

    // Add mode-specific instructions
    instructions.push_str(&get_mode_prompt(
        mode,
        allowed_file_edit_globs,
        allowed_write_new_globs,
        allowed_commands,
    ));

    // Add safety reminders
    instructions.push_str("\n# Safety Reminders\n");
    instructions
        .push_str("- Always read files before editing them to understand the current content\n");
    instructions.push_str(
        "- Use search/replace blocks for precise edits instead of rewriting entire files\n",
    );
    instructions.push_str("- Run tests after making changes to ensure nothing is broken\n");

    match mode {
        Modes::Wcgw => {
            instructions.push_str("- You have full access to the repository and system\n");
            instructions.push_str("- Be careful with destructive operations\n");
        }
        Modes::Architect => {
            instructions
                .push_str("- READ-ONLY MODE: No file modifications or system changes allowed\n");
            instructions.push_str("- Focus on analysis, understanding, and providing guidance\n");
        }
        Modes::CodeWriter => {
            instructions.push_str("- Limited access based on configured permissions\n");
            instructions.push_str("- Stay within the defined scope of allowed operations\n");
        }
    }

    instructions.push_str("\n# Tool Usage Guidelines\n");
    instructions.push_str("- Use ReadFiles to examine multiple files at once for efficiency\n");
    instructions
        .push_str("- Use BashCommand for running shell commands with proper error handling\n");
    instructions
        .push_str("- Use FileWriteOrEdit for making precise changes with search/replace blocks\n");
    instructions.push_str("- Use Initialize to set up or change workspace and mode settings\n");

    instructions
}

/// Knowledge transfer prompt for wcgw mode
/// Exact copy from wcgw Python: `WCGW_KT`
pub const WCGW_KT: &str = "Use ContextSave tool to do a knowledge transfer of the task in hand.\n\
Write detailed description in order to do a KT.\n\
Save all information necessary for a person to understand the task and the problems.\n\n\
Format the description field using Markdown with the following sections.\n\
- # Objective section containing project and task objective.\n\
- # All user instructions section should be provided containing all instructions user shared in the conversation.\n\
- # Current status of the task should be provided containing only what is already achieved, not what is remaining.\n\
- # Pending issues with snippets section containing snippets of pending errors, traceback, file snippets, commands, etc. But no comments or solutions.\n\
- Be very verbose in the all issues with snippets section providing as much error context as possible.\n\
- # Build and development instructions section containing instructions to build or run project or run tests, or envrionment related information. Only include what is known. Leave empty if unknown.\n\
- Any other relevant sections following the above.\n\
- After the tool completes succesfully, tell me the task id and the file path the tool generated (important!)\n\
- This tool marks end of your conversation, do not run any further tools after calling this.\n\n\
Provide all relevant file paths in order to understand and solve the the task. Err towards providing more file paths than fewer.\n\n\
(Note to self: this conversation can then be resumed later asking Resume wcgw task <generated id> which should call Initialize tool)\n";

/// Knowledge transfer prompt for architect mode
/// Exact copy from wcgw Python: `ARCHITECT_KT`
pub const ARCHITECT_KT: &str = "Use ContextSave tool to do a knowledge transfer of the task in hand.\n\
Write detailed description in order to do a KT.\n\
Save all information necessary for a person to understand the task and the problems.\n\n\
Format the description field using Markdown with the following sections.\n\
- # Objective section containing project and task objective.\n\
- # All user instructions section should be provided containing all instructions user shared in the conversation.\n\
- # Designed plan should be provided containing the designed plan as discussed.\n\
- Any other relevant sections following the above.\n\
- After the tool completes succesfully, tell me the task id and the file path the tool generated (important!)\n\
- This tool marks end of your conversation, do not run any further tools after calling this.\n\n\
Provide all relevant file paths in order to understand and solve the the task. Err towards providing more file paths than fewer.\n\n\
(Note to self: this conversation can then be resumed later asking Resume wcgw task <generated id> which should call Initialize tool)\n";

/// Get the appropriate knowledge transfer prompt for a mode
/// Matches wcgw Python: `get_kt_prompt()`
pub fn get_kt_prompt(mode: &Modes) -> &'static str {
    match mode {
        Modes::Wcgw => WCGW_KT,
        Modes::Architect => ARCHITECT_KT,
        Modes::CodeWriter => WCGW_KT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wcgw_prompt_contains_key_instructions() {
        assert!(WCGW_PROMPT.contains("bash execution"));
        assert!(WCGW_PROMPT.contains("FileWriteOrEdit"));
        assert!(WCGW_PROMPT.contains("pwd"));
    }

    #[test]
    fn test_architect_prompt_restrictions() {
        assert!(ARCHITECT_PROMPT.contains("not allowed to edit"));
        assert!(ARCHITECT_PROMPT.contains("read-only"));
        assert!(ARCHITECT_PROMPT.contains("FileWriteOrEdit"));
    }

    #[test]
    fn test_code_writer_prompt_generation() {
        let edit_globs = AllowedGlobs::List(vec!["*.rs".to_string(), "*.toml".to_string()]);
        let write_globs = AllowedGlobs::List(vec!["src/**".to_string()]);
        let commands = AllowedCommands::List(vec!["cargo".to_string(), "git".to_string()]);

        let prompt = code_writer_prompt(&edit_globs, &write_globs, &commands);

        assert!(prompt.contains("code_writer"));
        assert!(prompt.contains("*.rs"));
        assert!(prompt.contains("cargo"));
    }

    #[test]
    fn test_mode_prompt_selection() {
        let wcgw_prompt = get_mode_prompt(&Modes::Wcgw, None, None, None);
        let architect_prompt = get_mode_prompt(&Modes::Architect, None, None, None);

        assert!(wcgw_prompt.contains("bash execution"));
        assert!(architect_prompt.contains("architect\" mode"));
    }

    #[test]
    fn test_kt_prompts() {
        assert!(WCGW_KT.contains("ContextSave"));
        assert!(WCGW_KT.contains("Pending issues"));
        assert!(ARCHITECT_KT.contains("Designed plan"));
        assert!(ARCHITECT_KT.contains("ContextSave"));
    }

    #[test]
    fn test_get_kt_prompt() {
        let wcgw_kt = get_kt_prompt(&Modes::Wcgw);
        let architect_kt = get_kt_prompt(&Modes::Architect);
        let code_writer_kt = get_kt_prompt(&Modes::CodeWriter);

        assert!(wcgw_kt.contains("Pending issues"));
        assert!(architect_kt.contains("Designed plan"));
        assert!(code_writer_kt.contains("Pending issues")); // Uses WCGW_KT
    }
}
