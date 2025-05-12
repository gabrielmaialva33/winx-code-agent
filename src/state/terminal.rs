use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tracing::{debug, warn};
use vte::{Parser, Perform};

/// Maximum number of lines to keep in the screen buffer
#[allow(dead_code)]
const MAX_SCREEN_LINES: usize = 500;
/// Maximum number of columns for the screen
const DEFAULT_COLUMNS: usize = 160;

/// Represents a character with attributes in the terminal
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenCell {
    /// The character to display
    pub character: char,
    /// Whether the character is bold
    pub bold: bool,
    /// Whether the character is underlined
    pub underline: bool,
    /// Whether the character is blinking
    pub blink: bool,
    /// Whether the character is reversed (foreground/background colors)
    pub reverse: bool,
}

impl Default for ScreenCell {
    fn default() -> Self {
        Self {
            character: ' ',
            bold: false,
            underline: false,
            blink: false,
            reverse: false,
        }
    }
}

/// Represents the current state of a terminal screen
#[derive(Debug, Clone)]
pub struct Screen {
    /// Lines of characters with attributes
    pub lines: VecDeque<Vec<ScreenCell>>,
    /// Current cursor position (row, column)
    pub cursor_position: (usize, usize),
    /// Number of columns in the screen
    pub columns: usize,
    /// Whether the cursor should be visible
    #[allow(dead_code)]
    pub cursor_visible: bool,
}

impl Default for Screen {
    fn default() -> Self {
        let mut lines = VecDeque::new();
        lines.push_back(vec![ScreenCell::default(); DEFAULT_COLUMNS]);

        Self {
            lines,
            cursor_position: (0, 0),
            columns: DEFAULT_COLUMNS,
            cursor_visible: true,
        }
    }
}

impl Screen {
    /// Creates a new screen with specified dimensions
    pub fn new(columns: usize) -> Self {
        let mut lines = VecDeque::new();
        lines.push_back(vec![ScreenCell::default(); columns]);

        Self {
            lines,
            cursor_position: (0, 0),
            columns,
            cursor_visible: true,
        }
    }

    /// Get the current cursor row
    pub fn cursor_row(&self) -> usize {
        self.cursor_position.0
    }

    /// Get the current cursor column
    pub fn cursor_col(&self) -> usize {
        self.cursor_position.1
    }

    /// Ensure that a line exists at the specified index
    fn ensure_line(&mut self, line_idx: usize) {
        // Add new lines if needed
        while self.lines.len() <= line_idx {
            self.lines
                .push_back(vec![ScreenCell::default(); self.columns]);
        }
    }

    /// Ensure that the cursor position is valid
    fn ensure_cursor_position(&mut self) {
        self.ensure_line(self.cursor_position.0);

        // Ensure the cursor column is within bounds
        if self.cursor_position.1 >= self.columns {
            self.cursor_position.1 = self.columns - 1;
        }
    }

    /// Put a character at the current cursor position and advance the cursor
    pub fn put_char(&mut self, c: char, bold: bool, underline: bool, blink: bool, reverse: bool) {
        self.ensure_cursor_position();

        // Get the current cursor position
        let row = self.cursor_position.0;
        let col = self.cursor_position.1;

        // Put the character at the cursor position
        if col < self.lines[row].len() {
            self.lines[row][col] = ScreenCell {
                character: c,
                bold,
                underline,
                blink,
                reverse,
            };
        } else {
            // Add cells if needed
            while self.lines[row].len() <= col {
                self.lines[row].push(ScreenCell::default());
            }
            self.lines[row][col] = ScreenCell {
                character: c,
                bold,
                underline,
                blink,
                reverse,
            };
        }

        // Advance the cursor
        self.cursor_position.1 += 1;
        if self.cursor_position.1 >= self.columns {
            self.cursor_position.1 = 0;
            self.cursor_position.0 += 1;
            self.ensure_cursor_position();
        }
    }

    /// Move the cursor to a specific position
    pub fn move_cursor(&mut self, row: usize, col: usize) {
        self.cursor_position = (row, col);
        self.ensure_cursor_position();
    }

    /// Add a new line at the cursor position
    pub fn linefeed(&mut self) {
        self.cursor_position.0 += 1;
        self.ensure_cursor_position();
    }

    /// Return the cursor to the first column
    pub fn carriage_return(&mut self) {
        self.cursor_position.1 = 0;
    }

    /// Clear the screen
    pub fn clear(&mut self) {
        self.lines.clear();
        self.lines
            .push_back(vec![ScreenCell::default(); self.columns]);
        self.cursor_position = (0, 0);
    }

    /// Clear from the cursor to the end of the line
    pub fn clear_line_forward(&mut self) {
        let row = self.cursor_position.0;
        let col = self.cursor_position.1;

        if row < self.lines.len() {
            for i in col..self.lines[row].len() {
                self.lines[row][i] = ScreenCell::default();
            }
        }
    }

    /// Clear the current line
    pub fn clear_line(&mut self) {
        let row = self.cursor_position.0;
        if row < self.lines.len() {
            self.lines[row] = vec![ScreenCell::default(); self.columns];
        }
    }

    /// Scroll the screen up by one line
    #[allow(dead_code)]
    pub fn scroll_up(&mut self) {
        if !self.lines.is_empty() {
            self.lines.pop_front();
            self.ensure_line(self.cursor_position.0);
        }

        // Limit the number of lines to prevent memory growth
        while self.lines.len() > MAX_SCREEN_LINES {
            self.lines.pop_front();
        }
    }

    /// Get the screen as plain text
    #[allow(dead_code)]
    pub fn to_plain_text(&self) -> String {
        let mut result = String::new();

        for line in &self.lines {
            let line_text: String = line.iter().map(|cell| cell.character).collect();
            result.push_str(&line_text);
            result.push('\n');
        }

        result
    }

    /// Get the screen as a vector of strings, with each string representing a line
    pub fn display(&self) -> Vec<String> {
        let mut result = Vec::new();

        for line in &self.lines {
            let line_text: String = line.iter().map(|cell| cell.character).collect();

            // Trim trailing spaces
            let trimmed = line_text.trim_end();
            result.push(trimmed.to_string());
        }

        // Remove empty lines from the end
        while let Some(last) = result.last() {
            if last.is_empty() {
                result.pop();
            } else {
                break;
            }
        }

        result
    }
}

/// Terminal state performer that handles VTE events
#[derive(Clone)]
pub struct TerminalPerformer {
    /// The screen state
    screen: Arc<Mutex<Screen>>,
    /// Current text attributes
    bold: bool,
    /// Current underline state
    underline: bool,
    /// Current blink state
    blink: bool,
    /// Current reverse state
    reverse: bool,
}

// Custom debug implementation to avoid using the one from VTE
impl std::fmt::Debug for TerminalPerformer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalPerformer")
            .field("bold", &self.bold)
            .field("underline", &self.underline)
            .field("blink", &self.blink)
            .field("reverse", &self.reverse)
            .finish()
    }
}

impl TerminalPerformer {
    /// Creates a new terminal performer
    pub fn new(screen: Arc<Mutex<Screen>>) -> Self {
        Self {
            screen,
            bold: false,
            underline: false,
            blink: false,
            reverse: false,
        }
    }

    /// Get a reference to the screen
    #[allow(dead_code)]
    pub fn screen(&self) -> &Arc<Mutex<Screen>> {
        &self.screen
    }
}

impl Perform for TerminalPerformer {
    fn print(&mut self, c: char) {
        if let Ok(mut screen) = self.screen.lock() {
            screen.put_char(c, self.bold, self.underline, self.blink, self.reverse);
        } else {
            warn!("Failed to lock screen for print");
        }
    }

    fn execute(&mut self, byte: u8) {
        if let Ok(mut screen) = self.screen.lock() {
            match byte {
                b'\r' => screen.carriage_return(),
                b'\n' => {
                    screen.carriage_return();
                    screen.linefeed();
                }
                b'\t' => {
                    // Handle tab - advance to next 8-char boundary
                    let current_col = screen.cursor_col();
                    let new_col = (current_col + 8) & !7;
                    // Get the current row first to avoid multiple borrows
                    let current_row = screen.cursor_row();
                    screen.move_cursor(current_row, new_col);
                }
                b'\x08' => {
                    // Backspace
                    if screen.cursor_col() > 0 {
                        let current_row = screen.cursor_row();
                        let new_col = screen.cursor_col() - 1;
                        screen.move_cursor(current_row, new_col);
                    }
                }
                b'\x0C' => {
                    // Form feed - clear screen
                    screen.clear();
                }
                b'\x07' => { // Bell - ignore
                }
                _ => {
                    debug!("Unhandled execute: {:?}", byte);
                }
            }
        } else {
            warn!("Failed to lock screen for execute");
        }
    }

    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _c: char) {
        // Not implemented
    }

    fn put(&mut self, _byte: u8) {
        // Not implemented
    }

    fn unhook(&mut self) {
        // Not implemented
    }

    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {
        // Not implemented
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        c: char,
    ) {
        if let Ok(mut screen) = self.screen.lock() {
            match c {
                'A' => {
                    // Cursor Up
                    let n = params
                        .iter()
                        .next()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(1) as usize;
                    let current_row = screen.cursor_row();
                    let new_row = current_row.saturating_sub(n);
                    let current_col = screen.cursor_col();
                    screen.move_cursor(new_row, current_col);
                }
                'B' => {
                    // Cursor Down
                    let n = params
                        .iter()
                        .next()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(1) as usize;
                    let current_row = screen.cursor_row();
                    let current_col = screen.cursor_col();
                    screen.move_cursor(current_row + n, current_col);
                }
                'C' => {
                    // Cursor Forward
                    let n = params
                        .iter()
                        .next()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(1) as usize;
                    let current_row = screen.cursor_row();
                    let current_col = screen.cursor_col();
                    screen.move_cursor(current_row, current_col + n);
                }
                'D' => {
                    // Cursor Back
                    let n = params
                        .iter()
                        .next()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(1) as usize;
                    let current_row = screen.cursor_row();
                    let current_col = screen.cursor_col();
                    let new_col = current_col.saturating_sub(n);
                    screen.move_cursor(current_row, new_col);
                }
                'H' | 'f' => {
                    // Cursor Position
                    let row = params
                        .iter()
                        .next()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(1) as usize;
                    let col = params
                        .iter()
                        .nth(1)
                        .and_then(|p| p.first().copied())
                        .unwrap_or(1) as usize;
                    // Convert 1-based to 0-based
                    let row = row.saturating_sub(1);
                    let col = col.saturating_sub(1);
                    screen.move_cursor(row, col);
                }
                'J' => {
                    // Erase in Display
                    let n = params
                        .iter()
                        .next()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(0);
                    match n {
                        0 => {
                            // Clear from cursor to end of screen
                            screen.clear_line_forward();
                            // Clear all lines below cursor
                            let row = screen.cursor_row();
                            if row + 1 < screen.lines.len() {
                                for i in row + 1..screen.lines.len() {
                                    screen.lines[i] = vec![ScreenCell::default(); screen.columns];
                                }
                            }
                        }
                        2 => {
                            // Clear entire screen
                            screen.clear();
                        }
                        _ => debug!("Unhandled erase in display: {}", n),
                    }
                }
                'K' => {
                    // Erase in Line
                    let n = params
                        .iter()
                        .next()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(0);
                    match n {
                        0 => screen.clear_line_forward(),
                        2 => screen.clear_line(),
                        _ => debug!("Unhandled erase in line: {}", n),
                    }
                }
                'm' => {
                    // SGR - Select Graphic Rendition
                    if params.is_empty() {
                        // Reset attributes
                        self.bold = false;
                        self.underline = false;
                        self.blink = false;
                        self.reverse = false;
                    } else {
                        for param in params.iter().flatten() {
                            match *param {
                                0 => {
                                    // Reset
                                    self.bold = false;
                                    self.underline = false;
                                    self.blink = false;
                                    self.reverse = false;
                                }
                                1 => self.bold = true,
                                4 => self.underline = true,
                                5 | 6 => self.blink = true,
                                7 => self.reverse = true,
                                22 => self.bold = false,
                                24 => self.underline = false,
                                25 => self.blink = false,
                                27 => self.reverse = false,
                                _ => {} // Ignore unsupported SGR codes
                            }
                        }
                    }
                }
                _ => {
                    debug!("Unhandled CSI: {:?} {:?}", params, c);
                }
            }
        } else {
            warn!("Failed to lock screen for csi_dispatch");
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {
        // Not implemented
    }
}

/// Terminal emulator that processes input and maintains screen state
#[derive(Clone)]
pub struct TerminalEmulator {
    /// The performer that handles terminal events
    performer: TerminalPerformer,
    /// The shared screen state
    screen: Arc<Mutex<Screen>>,
}

// Custom debug implementation to avoid issues with Parser
impl std::fmt::Debug for TerminalEmulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalEmulator")
            .field("performer", &self.performer)
            .finish()
    }
}

impl TerminalEmulator {
    /// Creates a new terminal emulator
    pub fn new(columns: usize) -> Self {
        let screen = Arc::new(Mutex::new(Screen::new(columns)));
        let performer = TerminalPerformer::new(screen.clone());

        Self { performer, screen }
    }

    /// Process input and update screen state
    pub fn process(&mut self, data: &str) {
        let mut parser = Parser::new();
        for byte in data.bytes() {
            // Create a slice with the single byte
            let byte_slice = &[byte];
            parser.advance(&mut self.performer, byte_slice);
        }
    }

    /// Get the current screen state
    #[allow(dead_code)]
    pub fn get_screen(&self) -> Arc<Mutex<Screen>> {
        self.screen.clone()
    }

    /// Get the screen contents as a vector of strings
    pub fn display(&self) -> Vec<String> {
        if let Ok(screen) = self.screen.lock() {
            screen.display()
        } else {
            warn!("Failed to lock screen for display");
            vec![]
        }
    }

    /// Get the screen contents as plain text
    #[allow(dead_code)]
    pub fn to_plain_text(&self) -> String {
        if let Ok(screen) = self.screen.lock() {
            screen.to_plain_text()
        } else {
            warn!("Failed to lock screen for to_plain_text");
            String::new()
        }
    }

    /// Clear the screen
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        if let Ok(mut screen) = self.screen.lock() {
            screen.clear();
        } else {
            warn!("Failed to lock screen for clear");
        }
    }
}

/// Get the incremental output by comparing old and new screens
#[allow(dead_code)]
pub fn get_incremental_output(old_lines: &[String], new_lines: &[String]) -> Vec<String> {
    if old_lines.is_empty() {
        return new_lines.to_vec();
    }

    let nold = old_lines.len();
    let nnew = new_lines.len();

    // Try to find where old output ends and new output begins
    for i in (0..nnew).rev() {
        if i < nold && new_lines[i] == old_lines[nold - 1] {
            // Found a match, now check if we have a continuous match going backwards
            let mut continuous_match = true;
            for j in 0..i.min(nold - 1) {
                let old_idx = nold - 2 - j;
                let new_idx = i - 1 - j;

                if old_lines[old_idx] != new_lines[new_idx] {
                    continuous_match = false;
                    break;
                }
            }

            if continuous_match {
                // Return the new lines that come after the matching section
                if i + 1 < nnew {
                    return new_lines[i + 1..].to_vec();
                } else {
                    return vec![];
                }
            }
        }
    }

    // If we couldn't find a good matching point, just return all new lines
    new_lines.to_vec()
}

/// Render terminal output with line wrapping
pub fn render_terminal_output(text: &str) -> Vec<String> {
    let mut terminal = TerminalEmulator::new(DEFAULT_COLUMNS);
    terminal.process(text);

    terminal.display()
}

/// Get incremental text output by comparing old and new terminal states
pub fn incremental_text(text: &str, last_pending_output: &str) -> String {
    // Limit text size to prevent excessive memory usage
    let text_limit = if text.len() > 100_000 {
        &text[text.len() - 100_000..]
    } else {
        text
    };

    // Process the entire text
    let processed_lines = render_terminal_output(text_limit);

    if last_pending_output.is_empty() {
        // First call, return all processed lines with leading/trailing whitespace trimmed
        return processed_lines.join("\n").trim().to_string();
    }

    // Process the last pending output
    let last_rendered_lines = render_terminal_output(last_pending_output);

    // If the last output was empty, handle specially
    if last_rendered_lines.is_empty() {
        return processed_lines.join("\n").trim().to_string();
    }

    // If new content is actually new text appended to previous text,
    // only process the new text for efficiency
    let text_increment = if text_limit.len() > last_pending_output.len() {
        &text_limit[last_pending_output.len()..]
    } else {
        ""
    };

    if !text_increment.is_empty() {
        // Process the combined output for context
        let combined = format!("{}\n{}", last_rendered_lines.join("\n"), text_increment);
        let combined_lines = render_terminal_output(&combined);

        // Get the incremental output
        let mut incremental_lines = get_incremental_output(&last_rendered_lines, &combined_lines);

        // If first line of incremental matches last line of previous output, skip it
        if !incremental_lines.is_empty()
            && !last_rendered_lines.is_empty()
            && incremental_lines[0] == last_rendered_lines[last_rendered_lines.len() - 1]
        {
            incremental_lines.remove(0);
        }

        return incremental_lines.join("\n");
    }

    // Get the incremental output
    let incremental_lines = get_incremental_output(&last_rendered_lines, &processed_lines);

    incremental_lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_screen_basic_operations() {
        let mut screen = Screen::new(80);

        // Test putting characters
        screen.put_char('H', false, false, false, false);
        screen.put_char('e', false, false, false, false);
        screen.put_char('l', false, false, false, false);
        screen.put_char('l', false, false, false, false);
        screen.put_char('o', false, false, false, false);

        let display = screen.display();
        assert_eq!(display, vec!["Hello"]);

        // Test cursor movement
        screen.carriage_return();
        screen.linefeed();

        screen.put_char('W', false, false, false, false);
        screen.put_char('o', false, false, false, false);
        screen.put_char('r', false, false, false, false);
        screen.put_char('l', false, false, false, false);
        screen.put_char('d', false, false, false, false);

        let display = screen.display();
        assert_eq!(display, vec!["Hello", "World"]);

        // Test clearing line
        screen.clear_line();
        let display = screen.display();
        assert_eq!(display, vec!["Hello"]);
    }

    #[test]
    fn test_terminal_emulator_basic() {
        let mut terminal = TerminalEmulator::new(80);

        // Test simple text processing
        terminal.process("Hello\r\nWorld");
        let display = terminal.display();
        assert_eq!(display, vec!["Hello", "World"]);

        // Test escape sequences
        terminal.clear();
        terminal.process("Normal \x1b[1mBold\x1b[0m Normal");
        let display = terminal.display();
        assert_eq!(display, vec!["Normal Bold Normal"]);

        // Test cursor movement
        terminal.clear();
        terminal.process("Hello\x1b[5D_\x1b[1C_\x1b[1C_");
        let display = terminal.display();
        assert_eq!(display, vec!["_e_l_"]);
    }

    #[test]
    fn test_incremental_output() {
        let old = vec!["Line 1".to_string(), "Line 2".to_string()];
        let new = vec![
            "Line 1".to_string(),
            "Line 2".to_string(),
            "Line 3".to_string(),
        ];

        let incremental = get_incremental_output(&old, &new);
        assert_eq!(incremental, vec!["Line 3"]);

        // Test with completely different content
        let old = vec!["Line A".to_string(), "Line B".to_string()];
        let new = vec!["Line X".to_string(), "Line Y".to_string()];

        let incremental = get_incremental_output(&old, &new);
        assert_eq!(incremental, vec!["Line X", "Line Y"]);
    }

    #[test]
    fn test_render_terminal_output() {
        let text = "Hello\r\nWorld\r\n\x1b[31mRed\x1b[0m Text";
        let lines = render_terminal_output(text);
        assert_eq!(lines, vec!["Hello", "World", "Red Text"]);
    }
}
