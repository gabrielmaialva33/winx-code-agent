//! Command safety and validation module
//!
//! This module provides utilities for detecting potentially problematic commands
//! that might hang, require interaction, or cause other issues. Based on WCGW's
//! command safety patterns.

use std::collections::HashSet;
use std::time::Duration;

/// Default command timeout in seconds
pub const DEFAULT_COMMAND_TIMEOUT: u64 = 30;

/// Maximum output buffer size (1MB)
pub const MAX_OUTPUT_SIZE: usize = 1024 * 1024;

/// Commands that are known to be interactive and might hang
static INTERACTIVE_COMMANDS: &[&str] = &[
    // Editors
    "vim",
    "vi",
    "nano",
    "emacs",
    "code",
    "subl",
    // Interactive shells/languages
    "python",
    "python3",
    "node",
    "nodejs",
    "ruby",
    "irb",
    "scala",
    "ghci",
    // Interactive tools
    "mysql",
    "psql",
    "sqlite3",
    "redis-cli",
    "mongo",
    // Pagers
    "less",
    "more",
    "view",
    // System tools that might hang
    "top",
    "htop",
    "watch",
    "tail -f",
    // Version control interactive
    "git rebase -i",
    "git add -i",
    "git commit", // without -m
];

/// Commands that might run for a long time
static LONG_RUNNING_COMMANDS: &[&str] = &[
    // Build tools
    "make",
    "cargo build",
    "npm install",
    "pip install",
    "yarn install",
    // Compilation
    "gcc",
    "g++",
    "clang",
    "rustc",
    "javac",
    // Package managers
    "apt-get",
    "yum",
    "brew install",
    "pacman",
    // Network tools
    "wget",
    "curl",
    "rsync",
    "scp",
    // Archive tools
    "tar",
    "zip",
    "unzip",
    "gzip",
];

/// Commands that spawn background processes
static BACKGROUND_COMMANDS: &[&str] = &[
    // Servers
    "python -m http.server",
    "node server",
    "rails server",
    "cargo run",
    // Background services
    "nohup",
    "screen",
    "tmux",
    // System services
    "systemctl start",
    "service start",
];

/// Command safety analyzer
#[derive(Debug, Clone)]
pub struct CommandSafety {
    interactive_commands: HashSet<String>,
    long_running_commands: HashSet<String>,
    background_commands: HashSet<String>,
}

impl Default for CommandSafety {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandSafety {
    /// Create a new command safety analyzer
    pub fn new() -> Self {
        let interactive_commands = INTERACTIVE_COMMANDS.iter().map(|s| s.to_string()).collect();

        let long_running_commands = LONG_RUNNING_COMMANDS
            .iter()
            .map(|s| s.to_string())
            .collect();

        let background_commands = BACKGROUND_COMMANDS.iter().map(|s| s.to_string()).collect();

        Self {
            interactive_commands,
            long_running_commands,
            background_commands,
        }
    }

    /// Check if a command is potentially interactive
    pub fn is_interactive(&self, command: &str) -> bool {
        let normalized = self.normalize_command(command);

        // Check exact matches
        if self.interactive_commands.contains(&normalized) {
            return true;
        }

        // Check if command starts with any interactive command
        for interactive_cmd in &self.interactive_commands {
            if normalized.starts_with(interactive_cmd) {
                // Check that it's a word boundary
                let rest = &normalized[interactive_cmd.len()..];
                if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
                    return true;
                }
            }
        }

        // Special cases
        self.check_special_interactive_cases(&normalized)
    }

    /// Check if a command might run for a long time
    pub fn is_long_running(&self, command: &str) -> bool {
        let normalized = self.normalize_command(command);

        for long_cmd in &self.long_running_commands {
            if normalized.starts_with(long_cmd) {
                let rest = &normalized[long_cmd.len()..];
                if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
                    return true;
                }
            }
        }

        false
    }

    /// Check if a command spawns background processes
    pub fn is_background_command(&self, command: &str) -> bool {
        let normalized = self.normalize_command(command);

        // Check for explicit background operators
        if normalized.contains(" &") || normalized.ends_with('&') {
            return true;
        }

        for bg_cmd in &self.background_commands {
            if normalized.starts_with(bg_cmd) {
                let rest = &normalized[bg_cmd.len()..];
                if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
                    return true;
                }
            }
        }

        false
    }

    /// Get recommended timeout for a command
    pub fn get_timeout(&self, command: &str) -> Duration {
        if self.is_long_running(command) {
            Duration::from_secs(300) // 5 minutes for long-running commands
        } else if self.is_background_command(command) {
            Duration::from_secs(60) // 1 minute for background commands
        } else {
            Duration::from_secs(DEFAULT_COMMAND_TIMEOUT) // 30 seconds default
        }
    }

    /// Get safety warnings for a command
    pub fn get_warnings(&self, command: &str) -> Vec<String> {
        let mut warnings = Vec::new();

        if self.is_interactive(command) {
            warnings.push(format!(
                "Command '{}' appears to be interactive and may hang waiting for input",
                command
            ));
            warnings.push("Consider using non-interactive flags or alternatives".to_string());
        }

        if self.is_long_running(command) {
            warnings.push(format!(
                "Command '{}' may take a long time to complete",
                command
            ));
            warnings.push("Consider using status_check to monitor progress".to_string());
        }

        if self.is_background_command(command) {
            warnings.push(format!(
                "Command '{}' may spawn background processes",
                command
            ));
            warnings.push("Use explicit process management if needed".to_string());
        }

        warnings
    }

    /// Normalize command for comparison
    fn normalize_command(&self, command: &str) -> String {
        command.trim().to_lowercase()
    }

    /// Check special cases for interactive commands
    fn check_special_interactive_cases(&self, command: &str) -> bool {
        // Git commit without -m flag
        if command.starts_with("git commit")
            && !command.contains("-m")
            && !command.contains("--message")
        {
            return true;
        }

        // Docker run without -d flag (detached)
        if command.starts_with("docker run")
            && !command.contains("-d")
            && !command.contains("--detach")
        {
            return true;
        }

        // SSH without command
        if command == "ssh" || (command.starts_with("ssh ") && !command.contains(" -- ")) {
            return true;
        }

        // FTP/SFTP
        if command.starts_with("ftp ") || command.starts_with("sftp ") {
            return true;
        }

        false
    }
}

/// Command execution context
#[derive(Debug, Clone)]
pub struct CommandContext {
    pub command: String,
    pub timeout: Duration,
    pub max_output_size: usize,
    pub is_interactive: bool,
    pub is_long_running: bool,
    pub is_background: bool,
    pub warnings: Vec<String>,
}

impl CommandContext {
    /// Create a new command context with safety analysis
    pub fn new(command: &str) -> Self {
        let safety = CommandSafety::new();
        let timeout = safety.get_timeout(command);
        let is_interactive = safety.is_interactive(command);
        let is_long_running = safety.is_long_running(command);
        let is_background = safety.is_background_command(command);
        let warnings = safety.get_warnings(command);

        Self {
            command: command.to_string(),
            timeout,
            max_output_size: MAX_OUTPUT_SIZE,
            is_interactive,
            is_long_running,
            is_background,
            warnings,
        }
    }

    /// Check if the command should be allowed to execute
    pub fn should_allow_execution(&self) -> Result<(), crate::errors::WinxError> {
        if self.is_interactive {
            return Err(crate::errors::WinxError::InteractiveCommandDetected {
                command: self.command.clone(),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interactive_detection() {
        let safety = CommandSafety::new();

        // Interactive commands
        assert!(safety.is_interactive("vim file.txt"));
        assert!(safety.is_interactive("python"));
        assert!(safety.is_interactive("git commit"));
        assert!(safety.is_interactive("mysql -u root"));

        // Non-interactive commands
        assert!(!safety.is_interactive("ls -la"));
        assert!(!safety.is_interactive("git commit -m 'message'"));
        assert!(!safety.is_interactive("python script.py"));
        assert!(!safety.is_interactive("cat file.txt"));
    }

    #[test]
    fn test_long_running_detection() {
        let safety = CommandSafety::new();

        // Long-running commands
        assert!(safety.is_long_running("cargo build"));
        assert!(safety.is_long_running("npm install"));
        assert!(safety.is_long_running("make all"));

        // Quick commands
        assert!(!safety.is_long_running("ls"));
        assert!(!safety.is_long_running("echo hello"));
    }

    #[test]
    fn test_background_detection() {
        let safety = CommandSafety::new();

        // Background commands
        assert!(safety.is_background_command("python -m http.server &"));
        assert!(safety.is_background_command("nohup long_process"));
        assert!(safety.is_background_command("screen -S session"));

        // Foreground commands
        assert!(!safety.is_background_command("ls"));
        assert!(!safety.is_background_command("python script.py"));
    }

    #[test]
    fn test_timeout_calculation() {
        let safety = CommandSafety::new();

        // Long-running should get 5 minutes
        assert_eq!(safety.get_timeout("cargo build"), Duration::from_secs(300));

        // Background should get 1 minute
        assert_eq!(
            safety.get_timeout("nohup process &"),
            Duration::from_secs(60)
        );

        // Default should get 30 seconds
        assert_eq!(safety.get_timeout("ls"), Duration::from_secs(30));
    }

    #[test]
    fn test_command_context() {
        let ctx = CommandContext::new("vim file.txt");

        assert!(ctx.is_interactive);
        assert!(!ctx.warnings.is_empty());
        assert!(ctx.should_allow_execution().is_err());

        let ctx2 = CommandContext::new("ls -la");
        assert!(!ctx2.is_interactive);
        assert!(ctx2.should_allow_execution().is_ok());
    }
}
