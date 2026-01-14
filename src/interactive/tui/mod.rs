//! Winx TUI - Claude Code style terminal interface
//!
//! Full-featured TUI using ratatui, inspired by Claude Code's interface.
//!
//! ## Architecture
//!
//! - `app.rs` - Application state and event handling
//! - `terminal.rs` - Terminal setup/teardown and notifications
//! - `ui.rs` - Layout and rendering
//! - `components/` - Reusable UI components
//!
//! ## Usage
//!
//! ```ignore
//! use winx_code_agent::interactive::tui;
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     tui::run().await
//! }
//! ```

pub mod app;
pub mod terminal;
pub mod ui;

// Re-exports
pub use app::{App, AppMode, ChatMessage, MessageRole};
pub use terminal::{init, restore, send_notification, NotificationProtocol};

use color_eyre::Result;

/// Run the TUI application
///
/// This is the main entry point for the TUI mode.
/// It initializes the terminal, runs the app loop, and restores on exit.
pub fn run() -> Result<()> {
    // Initialize terminal
    let mut terminal = terminal::init()?;

    // Create and run app
    let mut app = App::new();
    let result = app.run(&mut terminal);

    // Always restore terminal, even on error
    terminal::restore()?;

    result
}

/// Run the TUI with custom app configuration
pub fn run_with_app(mut app: App) -> Result<()> {
    let mut terminal = terminal::init()?;
    let result = app.run(&mut terminal);
    terminal::restore()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_creation() {
        let app = App::new();
        assert!(!app.should_quit);
    }
}
