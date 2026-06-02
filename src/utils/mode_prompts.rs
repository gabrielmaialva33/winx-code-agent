//! Behavioral prompts injected per operating mode, ported from wcgw's `modes.py`.
//!
//! The mode (wcgw / architect / `code_writer`) already enforces permissions
//! technically (allowed commands, write globs, `bash -r`). But enforcement alone
//! makes the model *discover* the rules by hitting errors. These prompts tell the
//! model up front how to behave in the current mode — the "system-prompt policy"
//! layer that research shows is as important as the tool descriptions themselves.
//! Returned by `Initialize` so the agent reads them before its first action.

use std::fmt::Write as _;

use crate::types::{AllowedCommands, AllowedGlobs, CodeWriterConfig, Modes};

/// Default (full-access) behavior guidance.
const WCGW_PROMPT: &str = "# Operating mode: wcgw (full access)

- Use the provided shell, file-read and file-write tools to accomplish the objective.
- Do not provide code snippets unless asked — edit the code directly with the winx tools.
- Do not install new tools/packages before checking that the tool (or an alternative) doesn't already exist.
- Do not use echo/cat to write files; always use FileWriteOrEdit to create/update files.
- Do not send Ctrl-c / interrupts without asking — programs often keep running with no visible output.
- Provide as many file paths as you need to ReadFiles in a single call.
- Run `pwd` if you hit a file/dir not found error, to make sure you're not lost.";

/// Read-only architect behavior guidance.
const ARCHITECT_PROMPT: &str = "# Operating mode: architect (read-only)

- You may NOT edit, create, or overwrite any file. FileWriteOrEdit is disabled in this mode.
- You may NOT run commands that change disk, packages, system config or environment — read-only commands only.
- The shell runs with `-r` (restricted): you cannot change directory.
- Only run commands that help you explore the repo, understand the system, or read what's relevant.
- Do not send Ctrl-c / interrupts without asking the user.
- When an implementation is requested, share minimal snippets (use `...` for elided lines), not whole files.

# How to respond
- Read as many relevant files as possible first; be comprehensive.
- Start from the folder structure (ignore .git, node_modules, target, .venv, etc.).
- Provide as many file paths as you need to ReadFiles in a single call.";

/// Common command-discipline lines shared by `code_writer` variants.
const RUN_COMMAND_COMMON: &str =
    "- Do not send Ctrl-c / interrupts without asking — programs often keep running silently.
- Do not use echo/cat to write files; always use FileWriteOrEdit.
- Do not provide code snippets unless asked — edit the code directly with the winx tools.";

/// Build the behavioral prompt for the active mode. `config` is only consulted
/// for `code_writer`, where the allowed globs/commands shape the wording.
pub fn mode_prompt(mode: Modes, config: Option<&CodeWriterConfig>) -> String {
    match mode {
        Modes::Wcgw => WCGW_PROMPT.to_string(),
        Modes::Architect => ARCHITECT_PROMPT.to_string(),
        Modes::CodeWriter => code_writer_prompt(config),
    }
}

fn code_writer_prompt(config: Option<&CodeWriterConfig>) -> String {
    let mut out = String::from("# Operating mode: code_writer\n\n");

    // winx uses the same glob set for both edit and write-if-empty.
    match config.map(|c| &c.allowed_globs) {
        Some(AllowedGlobs::List(globs)) if globs.is_empty() => {
            out.push_str("- You are NOT allowed to edit or create any file.\n");
        }
        Some(AllowedGlobs::List(globs)) => {
            let _ = writeln!(
                out,
                "- You may edit/create files matching ONLY these globs: {}",
                globs.join(", ")
            );
        }
        _ => out.push_str("- You may edit/create files within the provided repository only.\n"),
    }

    match config.map(|c| &c.allowed_commands) {
        Some(AllowedCommands::List(cmds)) if cmds.is_empty() => {
            out.push_str("- You are NOT allowed to run any commands.\n");
        }
        Some(AllowedCommands::List(cmds)) => {
            let _ = writeln!(out, "- You may run ONLY the following commands: {}", cmds.join(", "));
            out.push_str(RUN_COMMAND_COMMON);
        }
        _ => {
            out.push_str(
                "- You may run commands for project setup, code writing, testing, running and debugging only.\n\
                 - Do not add/remove packages or change system configuration or environment.\n",
            );
            out.push_str(RUN_COMMAND_COMMON);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn architect_is_read_only_and_names_disabled_tool() {
        let p = mode_prompt(Modes::Architect, None);
        assert!(p.contains("read-only"));
        assert!(p.contains("FileWriteOrEdit is disabled"));
    }

    #[test]
    fn code_writer_lists_allowed_globs_and_commands() {
        let config = CodeWriterConfig {
            allowed_globs: AllowedGlobs::List(vec!["src/**/*.rs".to_string()]),
            allowed_commands: AllowedCommands::List(vec!["cargo test".to_string()]),
        };
        let p = mode_prompt(Modes::CodeWriter, Some(&config));
        assert!(p.contains("src/**/*.rs"));
        assert!(p.contains("cargo test"));
        assert!(p.contains("ONLY"));
    }

    #[test]
    fn code_writer_empty_lists_forbid() {
        let config = CodeWriterConfig {
            allowed_globs: AllowedGlobs::List(vec![]),
            allowed_commands: AllowedCommands::List(vec![]),
        };
        let p = mode_prompt(Modes::CodeWriter, Some(&config));
        assert!(p.contains("NOT allowed to edit"));
        assert!(p.contains("NOT allowed to run any commands"));
    }

    #[test]
    fn wcgw_is_full_access() {
        assert!(mode_prompt(Modes::Wcgw, None).contains("full access"));
    }
}
