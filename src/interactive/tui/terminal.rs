//! Terminal setup, teardown, and notifications
//!
//! Handles crossterm terminal initialization and restoration,
//! plus terminal-specific notification support (iTerm2, Kitty, Ghostty).
//!
//! Critical: Includes custom panic hook to restore terminal on crash.

use std::io::{self, stdout, Write};
use std::panic;

use color_eyre::Result;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

/// Type alias for our terminal backend
pub type Tui = Terminal<CrosstermBackend<io::Stdout>>;

/// Install panic hook that restores terminal before showing panic info.
/// Without this, a panic in raw mode leaves the terminal unusable.
fn install_panic_hook() {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        // Restore terminal FIRST, before printing anything
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, crossterm::cursor::Show);

        // Now call the original hook (which prints the panic)
        original_hook(panic_info);
    }));
}

/// Initialize the terminal for TUI mode
///
/// - Installs custom panic hook (critical for terminal restoration)
/// - Enables raw mode (no line buffering)
/// - Enters alternate screen (preserves scrollback)
/// - Enables mouse capture
pub fn init() -> Result<Tui> {
    // Install panic hook BEFORE entering raw mode
    install_panic_hook();

    // color-eyre for better error messages (ignore if already installed)
    let _ = color_eyre::install();

    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout());
    let terminal = Terminal::new(backend)?;

    Ok(terminal)
}

/// Restore the terminal to normal mode
///
/// - Disables raw mode
/// - Leaves alternate screen
/// - Disables mouse capture
/// - Shows cursor
pub fn restore() -> Result<()> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, crossterm::cursor::Show)?;
    Ok(())
}

/// Terminal notification protocols
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationProtocol {
    /// iTerm2 notification (OSC 9)
    ITerm2,
    /// Kitty notification (OSC 99)
    Kitty,
    /// Ghostty notification (OSC 777)
    Ghostty,
    /// Terminal bell as fallback
    Bell,
    /// No notifications
    Disabled,
}

impl NotificationProtocol {
    /// Detect the best notification protocol for the current terminal
    pub fn detect() -> Self {
        // Check terminal-specific environment variables
        if std::env::var("ITERM_SESSION_ID").is_ok() {
            return Self::ITerm2;
        }

        if std::env::var("KITTY_WINDOW_ID").is_ok() {
            return Self::Kitty;
        }

        // Check TERM_PROGRAM
        if let Ok(term) = std::env::var("TERM_PROGRAM") {
            match term.as_str() {
                "iTerm.app" => return Self::ITerm2,
                "ghostty" => return Self::Ghostty,
                "WezTerm" => return Self::ITerm2, // WezTerm supports iTerm2 protocol
                _ => {}
            }
        }

        // Disabled by default (bell is annoying)
        Self::Disabled
    }
}

/// Send a notification to the terminal
///
/// Uses the detected protocol or falls back to bell.
pub fn send_notification(title: &str, body: &str) {
    send_notification_with_protocol(title, body, NotificationProtocol::detect());
}

/// Send a notification using a specific protocol
pub fn send_notification_with_protocol(title: &str, body: &str, protocol: NotificationProtocol) {
    let mut stdout = stdout();

    match protocol {
        NotificationProtocol::ITerm2 => {
            // OSC 9 ; message ST
            let _ = write!(stdout, "\x1b]9;{}: {}\x07", title, body);
        }
        NotificationProtocol::Kitty => {
            // OSC 99 ; i=1:d=0 ; message ST
            let _ = write!(stdout, "\x1b]99;i=1:d=0;{}: {}\x1b\\", title, body);
        }
        NotificationProtocol::Ghostty => {
            // OSC 777 ; notify ; title ; body ST
            let _ = write!(stdout, "\x1b]777;notify;{};{}\x1b\\", title, body);
        }
        NotificationProtocol::Bell => {
            // Simple terminal bell
            let _ = write!(stdout, "\x07");
        }
        NotificationProtocol::Disabled => {}
    }

    let _ = stdout.flush();
}

/// Set the terminal title
pub fn set_title(title: &str) {
    let mut stdout = stdout();
    // OSC 0 ; title ST - works in most terminals
    let _ = write!(stdout, "\x1b]0;{}\x1b\\", title);
    let _ = stdout.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_protocol_detect() {
        // This will vary by environment, just ensure it doesn't panic
        let _protocol = NotificationProtocol::detect();
    }

    #[test]
    fn test_notification_protocols() {
        // Test that each protocol variant exists
        assert_ne!(NotificationProtocol::ITerm2, NotificationProtocol::Kitty);
        assert_ne!(NotificationProtocol::Kitty, NotificationProtocol::Ghostty);
        assert_ne!(NotificationProtocol::Ghostty, NotificationProtocol::Bell);
        assert_ne!(NotificationProtocol::Bell, NotificationProtocol::Disabled);
    }
}
