//! Markdown rendering for LLM responses
//!
//! Provides syntax highlighting for code blocks and basic markdown formatting.

use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

/// Markdown renderer with syntax highlighting
pub struct MarkdownRender {
    syntax_set: SyntaxSet,
    theme: Theme,
    in_code_block: bool,
    code_lang: Option<String>,
    code_buffer: String,
}

impl MarkdownRender {
    /// Create new markdown renderer
    pub fn new() -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        let theme = theme_set.themes["base16-ocean.dark"].clone();

        Self {
            syntax_set,
            theme,
            in_code_block: false,
            code_lang: None,
            code_buffer: String::new(),
        }
    }

    /// Render complete markdown text
    pub fn render(&mut self, text: &str) -> String {
        let mut output = String::new();

        for line in text.lines() {
            output.push_str(&self.render_line(line));
            output.push('\n');
        }

        output
    }

    /// Render incremental text (for streaming)
    pub fn render_incremental(&mut self, text: &str) -> String {
        let mut output = String::new();

        for ch in text.chars() {
            if ch == '\n' {
                // End of line, process accumulated content
                if self.in_code_block {
                    // Inside code block, accumulate
                    self.code_buffer.push(ch);
                } else {
                    output.push(ch);
                }
            } else if self.in_code_block {
                self.code_buffer.push(ch);

                // Check for closing ```
                if self.code_buffer.ends_with("```") {
                    // End code block
                    let code = self.code_buffer.trim_end_matches("```").to_string();
                    output.push_str(&self.highlight_code(&code));
                    output.push_str("\x1b[0m");

                    self.in_code_block = false;
                    self.code_lang = None;
                    self.code_buffer.clear();
                }
            } else {
                // Check for opening ```
                output.push(ch);

                // Detect code block start
                if output.ends_with("```") {
                    // Remove the ``` from output
                    output.truncate(output.len() - 3);
                    self.in_code_block = true;
                    self.code_buffer.clear();
                }
            }
        }

        // Apply basic formatting
        self.format_inline(&output)
    }

    /// Render single line
    fn render_line(&mut self, line: &str) -> String {
        let trimmed = line.trim();

        // Code block markers
        if trimmed.starts_with("```") {
            if self.in_code_block {
                // End code block
                let code = std::mem::take(&mut self.code_buffer);
                self.in_code_block = false;
                let lang = self.code_lang.take();
                return self.highlight_code_block(&code, lang.as_deref());
            } else {
                // Start code block
                self.in_code_block = true;
                self.code_lang = trimmed.strip_prefix("```").map(|s| s.to_string());
                self.code_buffer.clear();
                return String::new();
            }
        }

        if self.in_code_block {
            self.code_buffer.push_str(line);
            self.code_buffer.push('\n');
            return String::new();
        }

        // Headers
        if trimmed.starts_with("# ") {
            return format!("\x1b[1;35m{}\x1b[0m", trimmed);
        }
        if trimmed.starts_with("## ") {
            return format!("\x1b[1;34m{}\x1b[0m", trimmed);
        }
        if trimmed.starts_with("### ") {
            return format!("\x1b[1;36m{}\x1b[0m", trimmed);
        }

        // List items
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            return format!(
                "  \x1b[33m•\x1b[0m {}",
                &trimmed[2..]
            );
        }

        // Numbered list
        if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit()) {
            if rest.starts_with(". ") {
                return format!("  \x1b[33m{}\x1b[0m", trimmed);
            }
        }

        // Regular line with inline formatting
        self.format_inline(line)
    }

    /// Format inline markdown (bold, italic, code)
    fn format_inline(&self, text: &str) -> String {
        let mut result = text.to_string();

        // Inline code `code`
        result = self.replace_pattern(&result, "`", "\x1b[48;5;236m\x1b[37m", "\x1b[0m");

        // Bold **text**
        result = self.replace_pattern(&result, "**", "\x1b[1m", "\x1b[0m");

        // Italic *text* (careful not to match **)
        // Skip for now - conflicts with bold

        result
    }

    /// Replace markdown pattern with ANSI codes
    fn replace_pattern(&self, text: &str, marker: &str, start: &str, end: &str) -> String {
        let mut result = String::new();
        let mut in_marker = false;
        let mut i = 0;
        let chars: Vec<char> = text.chars().collect();

        while i < chars.len() {
            let remaining = &text[text.char_indices().nth(i).map(|(idx, _)| idx).unwrap_or(text.len())..];

            if remaining.starts_with(marker) {
                if in_marker {
                    result.push_str(end);
                    in_marker = false;
                } else {
                    result.push_str(start);
                    in_marker = true;
                }
                i += marker.len();
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }

        // Close unclosed marker
        if in_marker {
            result.push_str(end);
        }

        result
    }

    /// Highlight code block with syntect
    fn highlight_code_block(&self, code: &str, lang: Option<&str>) -> String {
        let mut output = String::new();

        // Find syntax
        let syntax = lang
            .and_then(|l| self.syntax_set.find_syntax_by_token(l))
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        output.push_str("\x1b[48;5;236m"); // Dark background

        use syntect::easy::HighlightLines;
        let mut highlighter = HighlightLines::new(syntax, &self.theme);

        for line in code.lines() {
            match highlighter.highlight_line(line, &self.syntax_set) {
                Ok(ranges) => {
                    for (style, text) in ranges {
                        let fg = style.foreground;
                        output.push_str(&format!(
                            "\x1b[38;2;{};{};{}m{}",
                            fg.r, fg.g, fg.b, text
                        ));
                    }
                    output.push('\n');
                }
                Err(_) => {
                    output.push_str(line);
                    output.push('\n');
                }
            }
        }

        output.push_str("\x1b[0m");
        output
    }

    /// Simple code highlighting for incremental rendering
    fn highlight_code(&self, code: &str) -> String {
        // For incremental, just use a simple background color
        format!("\x1b[48;5;236m{}\x1b[0m", code)
    }
}

impl Default for MarkdownRender {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_header() {
        let mut render = MarkdownRender::new();
        let output = render.render_line("# Hello");
        assert!(output.contains("Hello"));
        assert!(output.contains("\x1b[")); // ANSI codes
    }

    #[test]
    fn test_render_list() {
        let mut render = MarkdownRender::new();
        let output = render.render_line("- Item");
        assert!(output.contains("Item"));
        assert!(output.contains("•"));
    }

    #[test]
    fn test_inline_code() {
        let render = MarkdownRender::new();
        let output = render.format_inline("Use `code` here");
        assert!(output.contains("code"));
    }
}
