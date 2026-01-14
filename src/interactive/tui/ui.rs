//! UI - compact, minimal, functional
//!
//! Design: htop/vim style - dense, no fluff, ASCII only

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use super::app::{App, AppMode, MessageRole};
use crate::interactive::status::Phase;

mod colors {
    use ratatui::style::Color;
    // Horror movie palette - intense green/red
    pub const GREEN: Color = Color::Rgb(0, 255, 65);     // Matrix/toxic green
    pub const RED: Color = Color::Rgb(255, 0, 50);       // Blood red
    pub const YELLOW: Color = Color::Rgb(255, 200, 0);   // Warning
    pub const CYAN: Color = Color::Rgb(0, 200, 200);     // Cold
    pub const PURPLE: Color = Color::Rgb(150, 50, 255);  // Eerie
    pub const DIM: Color = Color::Rgb(60, 65, 70);       // Shadow
    pub const TEXT: Color = Color::Rgb(200, 200, 200);   // Ghost white
    pub const BG: Color = Color::Rgb(10, 10, 12);        // Void black
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Layout: status(1) | chat(flex) | input(3) | keys(1)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // status line
            Constraint::Min(3),    // chat
            Constraint::Length(3), // input
            Constraint::Length(1), // keys
        ])
        .split(area);

    render_status(frame, chunks[0], app);
    render_chat(frame, chunks[1], app);
    render_input(frame, chunks[2], app);
    render_keys(frame, chunks[3], app);

    if app.mode == AppMode::Help {
        render_help(frame, area);
    }
}

fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    let (phase_sym, phase_color) = match app.phase {
        Phase::Idle => (".", colors::DIM),
        Phase::Connecting => ("~", colors::YELLOW),
        Phase::Thinking => ("*", colors::PURPLE),
        Phase::Generating | Phase::Streaming => (">", colors::CYAN),
        Phase::Done => ("+", colors::GREEN),
        Phase::Error => ("!", colors::RED),
    };

    let mode_str = match app.mode {
        AppMode::Normal => "NOR",
        AppMode::Insert => "INS",
        AppMode::Streaming => "STR",
        AppMode::Help => "HLP",
    };

    let mode_color = match app.mode {
        AppMode::Normal => colors::YELLOW,
        AppMode::Insert => colors::GREEN,
        AppMode::Streaming => colors::PURPLE,
        AppMode::Help => colors::CYAN,
    };

    let line = Line::from(vec![
        Span::styled(format!("[{}]", mode_str), Style::default().fg(mode_color)),
        Span::styled(" winx ", Style::default().fg(colors::GREEN).add_modifier(Modifier::BOLD)),
        Span::styled("|", Style::default().fg(colors::DIM)),
        Span::styled(format!(" {}:{} ", app.provider, app.model), Style::default().fg(colors::DIM)),
        Span::styled("|", Style::default().fg(colors::DIM)),
        Span::styled(format!(" {} ", app.messages.len()), Style::default().fg(colors::DIM)),
        Span::styled("|", Style::default().fg(colors::DIM)),
        Span::styled(format!(" {}", phase_sym), Style::default().fg(phase_color)),
    ]);

    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(colors::BG)),
        area,
    );
}

fn render_chat(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();

    for msg in &app.messages {
        let (prefix, style) = match msg.role {
            MessageRole::User => ("> ", Style::default().fg(colors::GREEN)),
            MessageRole::Assistant => ("< ", Style::default().fg(colors::CYAN)),
            MessageRole::System => ("# ", Style::default().fg(colors::DIM)),
        };

        for (i, content_line) in msg.content.lines().enumerate() {
            let pfx = if i == 0 { prefix } else { "  " };
            lines.push(Line::from(vec![
                Span::styled(pfx, style),
                Span::styled(content_line, Style::default().fg(colors::TEXT)),
            ]));
        }
        lines.push(Line::raw(""));
    }

    let visible = area.height.saturating_sub(2) as usize;
    let total = lines.len();
    let scroll = if total > visible {
        (total - visible).saturating_sub(app.scroll as usize)
    } else {
        0
    };

    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(colors::DIM));

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0)),
        area,
    );
}

fn render_input(frame: &mut Frame, area: Rect, app: &App) {
    let border_color = match app.mode {
        AppMode::Insert => colors::GREEN,
        AppMode::Streaming => colors::PURPLE,
        _ => colors::DIM,
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let text = if app.input.is_empty() && app.mode == AppMode::Insert {
        Span::styled("...", Style::default().fg(colors::DIM))
    } else {
        Span::styled(&app.input, Style::default().fg(colors::TEXT))
    };

    frame.render_widget(
        Paragraph::new(Line::from(text))
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );

    if app.mode == AppMode::Insert {
        let x = area.x + 1 + app.cursor as u16;
        let y = area.y + 1;
        frame.set_cursor_position((x.min(area.x + area.width - 2), y));
    }
}

fn render_keys(frame: &mut Frame, area: Rect, app: &App) {
    let keys = match app.mode {
        AppMode::Normal => "i:ins j/k:scroll y:copy r:regen ?:help q:quit",
        AppMode::Insert => "RET:send S-RET:nl ESC:normal",
        AppMode::Streaming => "C-c:cancel",
        AppMode::Help => "ESC/q:close",
    };

    frame.render_widget(
        Paragraph::new(Span::styled(keys, Style::default().fg(colors::DIM))),
        area,
    );
}

fn render_help(frame: &mut Frame, area: Rect) {
    let w = 40.min(area.width.saturating_sub(4));
    let h = 14.min(area.height.saturating_sub(4));
    let x = (area.width - w) / 2;
    let y = (area.height - h) / 2;
    let popup = Rect::new(x, y, w, h);

    let help = vec![
        Line::styled("KEYS", Style::default().fg(colors::CYAN).add_modifier(Modifier::BOLD)),
        Line::raw(""),
        Line::from(vec![
            Span::styled("i/a    ", Style::default().fg(colors::GREEN)),
            Span::raw("insert mode"),
        ]),
        Line::from(vec![
            Span::styled("j/k    ", Style::default().fg(colors::GREEN)),
            Span::raw("scroll"),
        ]),
        Line::from(vec![
            Span::styled("G/g    ", Style::default().fg(colors::GREEN)),
            Span::raw("end/start"),
        ]),
        Line::from(vec![
            Span::styled("y      ", Style::default().fg(colors::GREEN)),
            Span::raw("copy response"),
        ]),
        Line::from(vec![
            Span::styled("r      ", Style::default().fg(colors::GREEN)),
            Span::raw("regenerate"),
        ]),
        Line::from(vec![
            Span::styled("q      ", Style::default().fg(colors::RED)),
            Span::raw("quit"),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled("RET    ", Style::default().fg(colors::GREEN)),
            Span::raw("send"),
        ]),
        Line::from(vec![
            Span::styled("S-RET  ", Style::default().fg(colors::GREEN)),
            Span::raw("newline"),
        ]),
        Line::from(vec![
            Span::styled("ESC    ", Style::default().fg(colors::YELLOW)),
            Span::raw("normal mode"),
        ]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(colors::CYAN))
        .style(Style::default().bg(colors::BG));

    frame.render_widget(Paragraph::new(help).block(block), popup);
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_colors_defined() {
        use super::colors::*;
        let _ = (GREEN, RED, YELLOW, CYAN, PURPLE, DIM, TEXT, BG);
    }
}
