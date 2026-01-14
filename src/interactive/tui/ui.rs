//! UI - chat.md style (plain text hacker aesthetic)
//!
//! Reference: <https://github.com/rusiaaman/chat.md>
//! Philosophy: looks like a markdown file, not an "application"

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
    Frame,
};

use super::app::{App, AppMode, MessageRole};
use crate::interactive::status::Phase;

// Minimal 4-color palette
mod colors {
    use ratatui::style::Color;
    pub const GREEN: Color = Color::Rgb(0, 180, 0);      // Terminal green
    pub const RED: Color = Color::Rgb(180, 0, 0);        // Error red
    pub const DIM: Color = Color::DarkGray;              // Delimiters
    pub const TEXT: Color = Color::White;                // Content
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Simple layout: chat takes all space except last line for input
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // chat (all available)
            Constraint::Length(1), // input line
        ])
        .split(area);

    render_chat(frame, chunks[0], app);
    render_input(frame, chunks[1], app);
}

fn render_chat(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();

    for msg in &app.messages {
        // chat.md style delimiter: # %% role
        let delimiter = match msg.role {
            MessageRole::User => "# %% user",
            MessageRole::Assistant => "# %% assistant",
            MessageRole::System => "# %% system",
        };

        lines.push(Line::styled(delimiter, Style::default().fg(colors::DIM)));

        // Content - plain text, no decoration
        for content_line in msg.content.lines() {
            lines.push(Line::styled(content_line, Style::default().fg(colors::TEXT)));
        }

        lines.push(Line::raw("")); // blank line between messages
    }

    // Status indicator at end (only when active)
    if app.phase.is_active() {
        let status = match app.phase {
            Phase::Connecting => "...connecting",
            Phase::Thinking => "...thinking",
            Phase::Generating => "...generating",
            Phase::Streaming => "...",
            _ => "",
        };
        if !status.is_empty() {
            lines.push(Line::styled(status, Style::default().fg(colors::DIM)));
        }
    }

    let visible = area.height as usize;
    let total = lines.len();

    // Auto-scroll to bottom unless user scrolled up
    let scroll_pos = if app.scroll == 0 {
        total.saturating_sub(visible)
    } else {
        total.saturating_sub(visible).saturating_sub(app.scroll as usize)
    };

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((scroll_pos as u16, 0)),
        area,
    );
}

fn render_input(frame: &mut Frame, area: Rect, app: &App) {
    // Build input line: # %% user (for new input) or status
    let line = if app.mode == AppMode::Streaming {
        // During streaming, show minimal status
        let status = format!(
            "[{}] {}:{}",
            if app.phase.is_active() { "..." } else { "ok" },
            app.provider,
            app.model
        );
        Line::styled(status, Style::default().fg(colors::DIM))
    } else if app.input.is_empty() {
        // Empty input - show prompt
        let mode = match app.mode {
            AppMode::Normal => "[N]",
            AppMode::Insert => "[I]",
            _ => "",
        };
        Line::from(vec![
            Span::styled(mode, Style::default().fg(colors::DIM)),
            Span::styled(" > ", Style::default().fg(colors::GREEN)),
            Span::styled(
                format!("{}:{}", app.provider, app.model),
                Style::default().fg(colors::DIM),
            ),
        ])
    } else {
        // Has input - show it
        Line::from(vec![
            Span::styled("> ", Style::default().fg(colors::GREEN)),
            Span::styled(&app.input, Style::default().fg(colors::TEXT)),
        ])
    };

    frame.render_widget(Paragraph::new(line), area);

    // Cursor position
    if app.mode == AppMode::Insert {
        let cursor_x = if app.input.is_empty() {
            // After "> "
            area.x + 2
        } else {
            // After "> " + input
            area.x + 2 + app.cursor as u16
        };
        frame.set_cursor_position((cursor_x.min(area.x + area.width - 1), area.y));
    }
}

#[cfg(test)]
mod tests {
    use super::colors::*;

    #[test]
    fn test_colors_minimal() {
        // Only 4 colors
        let _ = (GREEN, RED, DIM, TEXT);
    }
}
