//! Syntax highlighting for winx input

use nu_ansi_term::{Color, Style};
use reedline::{Highlighter, StyledText};

/// Winx input highlighter
pub struct WinxHighlighter {
    command_style: Style,
    arg_style: Style,
    text_style: Style,
}

impl WinxHighlighter {
    /// Create new highlighter
    pub fn new() -> Self {
        Self {
            command_style: Style::new().fg(Color::Yellow).bold(),
            arg_style: Style::new().fg(Color::Cyan),
            text_style: Style::new().fg(Color::White),
        }
    }
}

impl Default for WinxHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter for WinxHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut styled = StyledText::new();

        if line.starts_with('.') {
            // Command mode
            let parts: Vec<&str> = line.splitn(2, ' ').collect();

            // Command
            styled.push((self.command_style, parts[0].to_string()));

            // Args
            if parts.len() > 1 {
                styled.push((Style::new(), " ".to_string()));
                styled.push((self.arg_style, parts[1].to_string()));
            }
        } else if line.starts_with(":::") {
            // Multiline block
            styled.push((Style::new().fg(Color::DarkGray), line.to_string()));
        } else {
            // Regular text
            styled.push((self.text_style, line.to_string()));
        }

        styled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_highlighter() {
        let highlighter = WinxHighlighter::new();

        // Command
        let styled = highlighter.highlight(".help", 0);
        assert!(!styled.buffer.is_empty());

        // Command with args
        let styled = highlighter.highlight(".model nvidia:qwen3", 0);
        assert!(styled.buffer.len() >= 2);

        // Regular text
        let styled = highlighter.highlight("hello world", 0);
        assert_eq!(styled.buffer.len(), 1);
    }
}
