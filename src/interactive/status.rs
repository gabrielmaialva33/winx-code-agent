//! Status display for TUI
//!
//! Shows process status like Claude Code:
//! - Connecting to model...
//! - Thinking...
//! - Generating response...

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::i18n::Language;

/// Status phases during API call
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Phase {
    #[default]
    Idle = 0,
    Connecting = 1,
    Thinking = 2,
    Generating = 3,
    Streaming = 4,
    Done = 5,
    Error = 6,
}

impl Phase {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Phase::Connecting,
            2 => Phase::Thinking,
            3 => Phase::Generating,
            4 => Phase::Streaming,
            5 => Phase::Done,
            6 => Phase::Error,
            _ => Phase::Idle,
        }
    }

    /// Check if this phase represents an active operation (spinner should animate)
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Phase::Connecting | Phase::Thinking | Phase::Generating | Phase::Streaming
        )
    }
}

/// Status display with animated spinner
pub struct StatusDisplay {
    phase: Arc<AtomicU8>,
    running: Arc<AtomicBool>,
    lang: Language,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StatusDisplay {
    /// Create new status display
    pub fn new(lang: Language) -> Self {
        Self {
            phase: Arc::new(AtomicU8::new(Phase::Idle as u8)),
            running: Arc::new(AtomicBool::new(false)),
            lang,
            handle: None,
        }
    }

    /// Start the status display
    pub fn start(&mut self) {
        if self.running.load(Ordering::SeqCst) {
            return;
        }

        self.running.store(true, Ordering::SeqCst);
        self.phase.store(Phase::Connecting as u8, Ordering::SeqCst);

        let phase = Arc::clone(&self.phase);
        let running = Arc::clone(&self.running);
        let lang = self.lang;

        self.handle = Some(std::thread::spawn(move || {
            let spinners = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut idx = 0;

            while running.load(Ordering::SeqCst) {
                let current_phase = Phase::from_u8(phase.load(Ordering::SeqCst));

                if current_phase == Phase::Idle || current_phase == Phase::Done {
                    break;
                }

                let (icon, msg) = Self::phase_display(current_phase, lang);
                let spinner = spinners[idx % spinners.len()];

                // Clear line and print status
                print!("\r\x1b[K\x1b[90m{} {} {}\x1b[0m", spinner, icon, msg);
                io::stdout().flush().ok();

                idx += 1;
                std::thread::sleep(Duration::from_millis(80));
            }

            // Clear the status line
            print!("\r\x1b[K");
            io::stdout().flush().ok();
        }));
    }

    /// Set current phase
    pub fn set_phase(&self, phase: Phase) {
        self.phase.store(phase as u8, Ordering::SeqCst);
    }

    /// Stop the status display
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        self.phase.store(Phase::Done as u8, Ordering::SeqCst);

        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }

        // Ensure line is cleared
        print!("\r\x1b[K");
        io::stdout().flush().ok();
    }

    /// Get phase display info
    fn phase_display(phase: Phase, lang: Language) -> (&'static str, &'static str) {
        match lang {
            Language::Portuguese => match phase {
                Phase::Idle => ("", ""),
                Phase::Connecting => ("🔌", "Conectando ao modelo..."),
                Phase::Thinking => ("🤔", "Pensando..."),
                Phase::Generating => ("✨", "Gerando resposta..."),
                Phase::Streaming => ("📝", "Escrevendo..."),
                Phase::Done => ("✅", "Concluído"),
                Phase::Error => ("❌", "Erro"),
            },
            Language::English => match phase {
                Phase::Idle => ("", ""),
                Phase::Connecting => ("🔌", "Connecting to model..."),
                Phase::Thinking => ("🤔", "Thinking..."),
                Phase::Generating => ("✨", "Generating response..."),
                Phase::Streaming => ("📝", "Writing..."),
                Phase::Done => ("✅", "Done"),
                Phase::Error => ("❌", "Error"),
            },
        }
    }
}

impl Drop for StatusDisplay {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phase_from_u8() {
        assert_eq!(Phase::from_u8(0), Phase::Idle);
        assert_eq!(Phase::from_u8(1), Phase::Connecting);
        assert_eq!(Phase::from_u8(2), Phase::Thinking);
        assert_eq!(Phase::from_u8(99), Phase::Idle);
    }

    #[test]
    fn test_status_display_lifecycle() {
        let mut status = StatusDisplay::new(Language::English);
        status.start();
        status.set_phase(Phase::Thinking);
        std::thread::sleep(Duration::from_millis(100));
        status.stop();
    }
}
