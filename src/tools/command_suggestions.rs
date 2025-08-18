//! Command suggestions tool for the Winx application.
//!
//! This module provides functionality for suggesting commands based on
//! command history, usage patterns, and context.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::debug;

use crate::errors::WinxError;
use crate::types::CommandSuggestions;

/// Handle the CommandSuggestions tool call
///
/// This processes the command suggestions request by analyzing patterns
/// in the command history and providing relevant suggestions.
///
/// # Arguments
///
/// * `bash_state` - The shared bash state containing command history and pattern analyzer
/// * `args` - The arguments for the command suggestions tool
///
/// # Returns
///
/// Returns a Result containing a string with the command suggestions formatted
/// for display to the user.
pub async fn handle_tool_call(
    bash_state: &Arc<Mutex<Option<crate::state::bash_state::BashState>>>,
    args: CommandSuggestions,
) -> Result<String, WinxError> {
    debug!("CommandSuggestions tool call with args: {:?}", args);

    // We need to extract information without holding the lock across an await point
    let (current_dir, pattern_analyzer);
    let last_command_str;

    // Scope for the lock
    {
        // Get bash state guard
        let bash_state_guard = bash_state.lock().map_err(|e| {
            WinxError::BashStateLockError(format!("Failed to lock bash state: {}", e))
        })?;

        // Check if bash state is initialized
        let bash_state = bash_state_guard
            .as_ref()
            .ok_or(WinxError::BashStateNotInitialized)?;

        // Determine current directory
        current_dir = match &args.current_dir {
            Some(dir) if !dir.is_empty() => dir.clone(),
            _ => bash_state.cwd.display().to_string(),
        };

        // Get the last command from arguments or from bash state
        let last_command_opt = match &args.last_command {
            Some(cmd) if !cmd.is_empty() => Some(cmd.clone()),
            _ => {
                // Create a separate scope for the lock to ensure it's dropped before we assign the result
                let last_cmd = {
                    let bash_guard = bash_state.interactive_bash.lock().await;
                    if let Some(bash) = bash_guard.as_ref() {
                        if !bash.last_command.is_empty() {
                            Some(bash.last_command.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };
                last_cmd
            }
        };

        // Clone the last command to owned string to avoid reference issues
        last_command_str = last_command_opt.map(|s| s.to_string());

        // Clone the pattern analyzer
        pattern_analyzer = bash_state.pattern_analyzer.clone();
    } // Lock is released here

    // Get suggestions from pattern analyzer
    let suggestions = pattern_analyzer
        .suggest_commands(
            &args.partial_command,
            &current_dir,
            last_command_str.as_deref(),
        )
        .await;

    // Limit suggestions to max_suggestions
    let limited_suggestions = if suggestions.len() > args.max_suggestions {
        &suggestions[0..args.max_suggestions]
    } else {
        &suggestions
    };

    // Format results
    let mut result = String::new();

    if limited_suggestions.is_empty() {
        result.push_str("No command suggestions available.\n\n");

        // If partial command is provided but no suggestions found, provide some hints
        if !args.partial_command.is_empty() {
            result.push_str(&format!(
                "No commands matching '{}'.\n",
                args.partial_command
            ));
            result.push_str("Try using a shorter prefix or check your input.\n");
        } else {
            result.push_str("Try executing some commands to build up command history.\n");
        }
    } else {
        // Format the suggestions
        if args.partial_command.is_empty() {
            result.push_str("Suggested commands based on context:\n\n");
        } else {
            result.push_str(&format!("Suggestions for '{}':\n\n", args.partial_command));
        }

        for (i, suggestion) in limited_suggestions.iter().enumerate() {
            result.push_str(&format!("{}. `{}`", i + 1, suggestion));

            // Add explanation if requested
            if args.include_explanations {
                let explanation = get_command_explanation(suggestion);
                if !explanation.is_empty() {
                    result.push_str(&format!(" - {}", explanation));
                }
            }

            result.push('\n');
        }
    }

    // Add context information
    result.push_str("\n---\n\n");

    // Add more detailed information if explanations are requested
    if args.include_explanations {
        result.push_str(&format!("Context directory: {}\n", current_dir));
        if let Some(cmd) = last_command_str.as_deref() {
            result.push_str(&format!("Previous command: {}\n", cmd));
        }
    }

    Ok(result)
}

/// Get a brief explanation for a command
///
/// This provides a short explanation of what the command does, based on
/// common command patterns and knowledge.
///
/// # Arguments
///
/// * `command` - The command to explain
///
/// # Returns
///
/// A string containing a brief explanation of the command
fn get_command_explanation(command: &str) -> String {
    // Extract the base command (first word)
    let base_command = command.split_whitespace().next().unwrap_or(command);

    // Common command explanations
    let explanations: HashMap<&str, &str> = [
        ("ls", "List files and directories"),
        ("cd", "Change directory"),
        ("pwd", "Print working directory"),
        ("mkdir", "Create directory"),
        ("touch", "Create empty file"),
        ("rm", "Remove files or directories"),
        ("cp", "Copy files or directories"),
        ("mv", "Move or rename files"),
        ("cat", "Display file contents"),
        ("grep", "Search for patterns in files"),
        ("find", "Search for files and directories"),
        ("chmod", "Change file permissions"),
        ("chown", "Change file owner"),
        ("git", "Version control operations"),
        ("npm", "Node.js package manager"),
        ("yarn", "Alternative package manager for Node.js"),
        ("cargo", "Rust package manager"),
        ("pip", "Python package manager"),
        ("python", "Run Python code"),
        ("node", "Run JavaScript code"),
        ("rustc", "Compile Rust code"),
        ("gcc", "Compile C code"),
        ("make", "Build using Makefile"),
        ("docker", "Container operations"),
        ("curl", "Transfer data from/to servers"),
        ("wget", "Download files from the web"),
        ("ssh", "Secure shell connection"),
        ("scp", "Secure copy files"),
        ("tar", "Archive files"),
        ("zip", "Compress files"),
        ("unzip", "Extract zip archives"),
        ("ps", "List running processes"),
        ("kill", "Terminate processes"),
        ("top", "Monitor system processes"),
        ("df", "Check disk space"),
        ("du", "Check directory size"),
        ("sudo", "Execute as superuser"),
        ("echo", "Print text to terminal"),
        ("history", "Show command history"),
        ("man", "Display manual pages"),
        ("less", "View file contents with pagination"),
        ("head", "Show beginning of file"),
        ("tail", "Show end of file"),
        ("vim", "Edit files with Vim"),
        ("nano", "Edit files with Nano"),
        ("code", "Open in Visual Studio Code"),
    ]
    .iter()
    .cloned()
    .collect();

    // Special case handling for git commands
    if base_command == "git" {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.len() > 1 {
            let git_subcommand = parts[1];
            let git_explanations: HashMap<&str, &str> = [
                ("add", "Stage changes for commit"),
                ("commit", "Record changes to the repository"),
                ("push", "Upload local repository content to remote"),
                ("pull", "Fetch and integrate with remote"),
                ("clone", "Create a copy of a repository"),
                ("status", "Show working tree status"),
                ("log", "Show commit logs"),
                ("branch", "List, create, or delete branches"),
                ("checkout", "Switch branches or restore files"),
                ("merge", "Join two or more development histories"),
                ("init", "Create an empty Git repository"),
                ("remote", "Manage set of tracked repositories"),
                ("fetch", "Download objects and refs from another repository"),
                ("diff", "Show changes between commits"),
                ("reset", "Reset current HEAD to the specified state"),
                ("rebase", "Reapply commits on top of another base tip"),
                ("stash", "Stash the changes in a dirty working directory"),
            ]
            .iter()
            .cloned()
            .collect();

            return git_explanations
                .get(git_subcommand)
                .copied()
                .unwrap_or("Git operation")
                .to_string();
        }
    }

    // Special case handling for npm commands
    if base_command == "npm" {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.len() > 1 {
            let npm_subcommand = parts[1];
            let npm_explanations: HashMap<&str, &str> = [
                ("install", "Install a package"),
                ("uninstall", "Remove a package"),
                ("start", "Run the start script"),
                ("test", "Run the test script"),
                ("run", "Run a script defined in package.json"),
                ("init", "Create a package.json file"),
                ("update", "Update packages"),
                ("audit", "Run a security audit"),
                ("publish", "Publish a package"),
                ("list", "List installed packages"),
            ]
            .iter()
            .cloned()
            .collect();

            return npm_explanations
                .get(npm_subcommand)
                .copied()
                .unwrap_or("NPM operation")
                .to_string();
        }
    }

    // Return explanation or empty string if not found
    explanations
        .get(base_command)
        .copied()
        .unwrap_or("")
        .to_string()
}
