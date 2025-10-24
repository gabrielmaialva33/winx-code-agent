use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{debug, warn};
use vte::{Parser, Perform}; // Replace std::sync::Mutex

// Import our enhanced ANSI code module
#[allow(unused_imports)]
use crate::state::ansi_codes;

/// Maximum number of lines to keep in the screen buffer
pub const MAX_SCREEN_LINES: usize = 10000;
/// Default maximum number of lines to keep in the screen buffer
pub const DEFAULT_MAX_SCREEN_LINES: usize = 500;
/// Maximum number of columns for the screen
const DEFAULT_COLUMNS: usize = 160;
/// Maximum output size in bytes to prevent excessive memory usage
pub const MAX_OUTPUT_SIZE: usize = 500_000;
/// Maximum cache entry lifetime in seconds
const CACHE_TTL: u64 = 300; // 5 minutes

/// Container for all possible character attributes
#[derive(Debug, Clone, Default)]
pub struct ScreenCellAttributes {
    /// Whether the character is bold
    pub bold: bool,
    /// Whether the character is underlined
    pub underline: bool,
    /// Whether the character is blinking
    pub blink: bool,
    /// Whether the character is reversed (foreground/background colors)
    pub reverse: bool,
    /// Foreground color
    pub fg_color: Option<TerminalColor>,
    /// Background color
    pub bg_color: Option<TerminalColor>,
    /// Whether the character is italic
    pub italic: bool,
    /// Whether the character is strikethrough
    pub strikethrough: bool,
    /// Whether the character is faint/dim
    pub dim: bool,
    /// Whether the character has double underline
    pub double_underline: bool,
    /// Whether the character is framed
    pub framed: bool,
    /// Whether the character is encircled
    pub encircled: bool,
    /// Whether the character is overlined
    pub overlined: bool,
    /// Whether the character uses fraktur font
    pub fraktur: bool,
    /// Whether the character is concealed
    pub conceal: bool,
    /// Whether the character is superscript
    pub superscript: bool,
    /// Whether the character is subscript
    pub subscript: bool,
    /// Whether the character is part of a hyperlink
    pub hyperlink: bool,
    /// URL for hyperlink, if applicable
    pub hyperlink_url: Option<String>,
    /// Font selection (0-9, where 0 is the primary font)
    pub font: u8,
}

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
    /// Foreground color (0-255 for 8-bit colors, RGB for 24-bit colors)
    pub fg_color: Option<TerminalColor>,
    /// Background color (0-255 for 8-bit colors, RGB for 24-bit colors)
    pub bg_color: Option<TerminalColor>,
    /// Whether the character is italic
    pub italic: bool,
    /// Whether the character is strikethrough
    pub strikethrough: bool,
    /// Whether the character is faint/dim
    pub dim: bool,
    /// Whether the character has double underline
    pub double_underline: bool,
    /// Whether the character is framed
    pub framed: bool,
    /// Whether the character is encircled
    pub encircled: bool,
    /// Whether the character is overlined
    pub overlined: bool,
    /// Whether the character uses fraktur font
    pub fraktur: bool,
    /// Whether the character is concealed
    pub conceal: bool,
    /// Whether the character is superscript
    pub superscript: bool,
    /// Whether the character is subscript
    pub subscript: bool,
    /// Whether the character is part of a hyperlink
    pub hyperlink: bool,
    /// URL for hyperlink, if applicable
    pub hyperlink_url: Option<String>,
    /// Font selection (0-9, where 0 is the primary font)
    pub font: u8,
}

/// Represents a terminal color
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalColor {
    /// Basic 8 colors (0-7)
    Basic(u8),
    /// Extended 8-bit color (0-255)
    Color256(u8),
    /// 24-bit RGB color
    TrueColor { r: u8, g: u8, b: u8 },
    /// Named color like "red", "blue", etc.
    Named(String),
}

impl Default for ScreenCell {
    fn default() -> Self {
        Self {
            character: ' ',
            bold: false,
            underline: false,
            blink: false,
            reverse: false,
            fg_color: None,
            bg_color: None,
            italic: false,
            strikethrough: false,
            dim: false,
            double_underline: false,
            framed: false,
            encircled: false,
            overlined: false,
            fraktur: false,
            conceal: false,
            superscript: false,
            subscript: false,
            hyperlink: false,
            hyperlink_url: None,
            font: 0, // Primary font
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
    pub cursor_visible: bool,
    /// Maximum number of lines to keep
    pub max_lines: usize,
    /// Last time the screen was modified
    last_modified: Instant,
}

impl Default for Screen {
    fn default() -> Self {
        let mut lines = VecDeque::with_capacity(DEFAULT_MAX_SCREEN_LINES);
        lines.push_back(vec![ScreenCell::default(); DEFAULT_COLUMNS]);

        Self {
            lines,
            cursor_position: (0, 0),
            columns: DEFAULT_COLUMNS,
            cursor_visible: true,
            max_lines: DEFAULT_MAX_SCREEN_LINES,
            last_modified: Instant::now(),
        }
    }
}

impl Screen {
    /// Creates a new screen with specified dimensions
    pub fn new(columns: usize) -> Self {
        let mut lines = VecDeque::with_capacity(DEFAULT_MAX_SCREEN_LINES);
        lines.push_back(vec![ScreenCell::default(); columns]);

        Self {
            lines,
            cursor_position: (0, 0),
            columns,
            cursor_visible: true,
            max_lines: DEFAULT_MAX_SCREEN_LINES,
            last_modified: Instant::now(),
        }
    }

    /// Creates a new screen with specified dimensions and maximum lines
    pub fn new_with_max_lines(columns: usize, max_lines: usize) -> Self {
        let mut lines = VecDeque::with_capacity(max_lines.min(MAX_SCREEN_LINES));
        lines.push_back(vec![ScreenCell::default(); columns]);

        Self {
            lines,
            cursor_position: (0, 0),
            columns,
            cursor_visible: true,
            max_lines: max_lines.min(MAX_SCREEN_LINES),
            last_modified: Instant::now(),
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

        // Limit the number of lines to prevent memory growth
        while self.lines.len() > self.max_lines {
            self.lines.pop_front();

            // Adjust cursor position to account for the removed line
            if self.cursor_position.0 > 0 {
                self.cursor_position.0 -= 1;
            }
        }

        self.last_modified = Instant::now();
    }

    /// Ensure that the cursor position is valid
    fn ensure_cursor_position(&mut self) {
        self.ensure_line(self.cursor_position.0);

        // Ensure the cursor column is within bounds
        if self.cursor_position.1 >= self.columns {
            self.cursor_position.1 = self.columns - 1;
        }

        self.last_modified = Instant::now();
    }

    /// Put a character at the current cursor position and advance the cursor
    #[allow(clippy::too_many_arguments)]
    pub fn put_char(&mut self, c: char, attributes: ScreenCellAttributes) {
        self.ensure_cursor_position();

        // Get the current cursor position
        let row = self.cursor_position.0;
        let col = self.cursor_position.1;

        // Put the character at the cursor position
        if col < self.lines[row].len() {
            self.lines[row][col] = ScreenCell {
                character: c,
                bold: attributes.bold,
                underline: attributes.underline,
                blink: attributes.blink,
                reverse: attributes.reverse,
                fg_color: attributes.fg_color,
                bg_color: attributes.bg_color,
                italic: attributes.italic,
                strikethrough: attributes.strikethrough,
                dim: attributes.dim,
                double_underline: attributes.double_underline,
                framed: attributes.framed,
                encircled: attributes.encircled,
                overlined: attributes.overlined,
                fraktur: attributes.fraktur,
                conceal: attributes.conceal,
                superscript: attributes.superscript,
                subscript: attributes.subscript,
                hyperlink: attributes.hyperlink,
                hyperlink_url: attributes.hyperlink_url,
                font: attributes.font,
            };
        } else {
            // Add cells if needed
            while self.lines[row].len() <= col {
                self.lines[row].push(ScreenCell::default());
            }
            self.lines[row][col] = ScreenCell {
                character: c,
                bold: attributes.bold,
                underline: attributes.underline,
                blink: attributes.blink,
                reverse: attributes.reverse,
                fg_color: attributes.fg_color,
                bg_color: attributes.bg_color,
                italic: attributes.italic,
                strikethrough: attributes.strikethrough,
                dim: attributes.dim,
                double_underline: attributes.double_underline,
                framed: attributes.framed,
                encircled: attributes.encircled,
                overlined: attributes.overlined,
                fraktur: attributes.fraktur,
                conceal: attributes.conceal,
                superscript: attributes.superscript,
                subscript: attributes.subscript,
                hyperlink: attributes.hyperlink,
                hyperlink_url: attributes.hyperlink_url,
                font: attributes.font,
            };
        }

        // Advance the cursor
        self.cursor_position.1 += 1;
        if self.cursor_position.1 >= self.columns {
            self.cursor_position.1 = 0;
            self.cursor_position.0 += 1;
            self.ensure_cursor_position();
        }

        self.last_modified = Instant::now();
    }

    /// Put a character at the current cursor position with basic attributes
    #[allow(clippy::too_many_arguments)]
    pub fn put_char_basic(
        &mut self,
        c: char,
        bold: bool,
        underline: bool,
        blink: bool,
        reverse: bool,
        fg_color: Option<TerminalColor>,
        bg_color: Option<TerminalColor>,
        italic: bool,
        strikethrough: bool,
    ) {
        let attributes = ScreenCellAttributes {
            bold,
            underline,
            blink,
            reverse,
            fg_color,
            bg_color,
            italic,
            strikethrough,
            ..Default::default()
        };

        self.put_char(c, attributes);
    }

    /// Move the cursor to a specific position
    pub fn move_cursor(&mut self, row: usize, col: usize) {
        self.cursor_position = (row, col);
        self.ensure_cursor_position();
        self.last_modified = Instant::now();
    }

    /// Add a new line at the cursor position
    pub fn linefeed(&mut self) {
        self.cursor_position.0 += 1;
        self.ensure_cursor_position();
        self.last_modified = Instant::now();
    }

    /// Return the cursor to the first column
    pub fn carriage_return(&mut self) {
        self.cursor_position.1 = 0;
        self.last_modified = Instant::now();
    }

    /// Clear the screen
    pub fn clear(&mut self) {
        self.lines.clear();
        self.lines
            .push_back(vec![ScreenCell::default(); self.columns]);
        self.cursor_position = (0, 0);
        self.last_modified = Instant::now();
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
        self.last_modified = Instant::now();
    }

    /// Clear the current line
    pub fn clear_line(&mut self) {
        let row = self.cursor_position.0;
        if row < self.lines.len() {
            self.lines[row] = vec![ScreenCell::default(); self.columns];
        }
        self.last_modified = Instant::now();
    }

    /// Scroll the screen up by one line
    pub fn scroll_up(&mut self) {
        if !self.lines.is_empty() {
            self.lines.pop_front();
            self.ensure_line(self.cursor_position.0);
        }
        self.last_modified = Instant::now();
    }

    /// Smart truncate the screen buffer to keep it within reasonable limits
    pub fn smart_truncate(&mut self, max_size: usize) {
        let current_size = self.lines.len();

        if current_size <= max_size {
            return;
        }

        // Calculate how many lines to remove
        let to_remove = current_size - max_size;

        // Keep a reasonable amount at the beginning
        let beginning_lines = max_size / 10; // 10% of max size

        if to_remove <= beginning_lines {
            // Simple case: just remove from the beginning
            for _ in 0..to_remove {
                self.lines.pop_front();
            }
        } else {
            // Complex case: keep beginning and end, with a marker in the middle
            let end_lines = max_size - beginning_lines - 1; // -1 for truncation marker

            // Save important parts
            let beginning: VecDeque<Vec<ScreenCell>> = self
                .lines
                .drain(0..beginning_lines.min(self.lines.len()))
                .collect();

            let end_start_index = self.lines.len().saturating_sub(end_lines);
            let end: VecDeque<Vec<ScreenCell>> = self.lines.drain(end_start_index..).collect();

            // Clear and rebuild with beginning + marker + end
            self.lines.clear();

            // Add beginning
            for line in beginning {
                self.lines.push_back(line);
            }

            // Add truncation marker
            let mut marker_line = vec![ScreenCell::default(); self.columns];
            let marker_text = " [... TRUNCATED OUTPUT ...] ";

            for (i, c) in marker_text.chars().enumerate() {
                if i < self.columns {
                    marker_line[i] = ScreenCell {
                        character: c,
                        bold: true,
                        reverse: true,
                        ..ScreenCell::default()
                    };
                }
            }

            self.lines.push_back(marker_line);

            // Add end
            for line in end {
                self.lines.push_back(line);
            }
        }

        // Adjust cursor position if necessary
        if self.cursor_position.0 >= self.lines.len() {
            self.cursor_position.0 = self.lines.len().saturating_sub(1);
        }

        self.last_modified = Instant::now();
    }

    /// Get the screen as plain text
    pub fn to_plain_text(&self) -> String {
        let mut result = String::with_capacity(self.lines.len() * self.columns);

        for line in &self.lines {
            let line_text: String = line.iter().map(|cell| cell.character).collect();
            result.push_str(&line_text);
            result.push('\n');
        }

        result
    }

    /// Get the screen as a vector of strings, with each string representing a line
    pub fn display(&self) -> Vec<String> {
        let mut result = Vec::with_capacity(self.lines.len());

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

    /// Returns the last time the screen was modified
    pub fn last_modified(&self) -> Instant {
        self.last_modified
    }

    /// Time since last modification in seconds
    pub fn time_since_last_modified(&self) -> f64 {
        self.last_modified.elapsed().as_secs_f64()
    }
}

/// Terminal state performer that handles VTE events
#[derive(Clone)]
pub struct TerminalPerformer {
    /// The screen state
    screen: Arc<Mutex<Screen>>,
    /// Current text attributes
    attributes: ScreenCellAttributes,
    /// SGR parameters cache for optimization
    sgr_state: HashMap<u16, bool>,
    /// Active hyperlink ID, if any
    current_hyperlink_id: Option<String>,
    /// Active hyperlink URL, if any
    current_hyperlink_url: Option<String>,
    /// Current OSC parameters being parsed
    osc_params: Vec<std::borrow::Cow<'static, str>>,
}

// Custom debug implementation to avoid using the one from VTE
impl std::fmt::Debug for TerminalPerformer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalPerformer")
            .field("attributes", &self.attributes)
            .field("hyperlink_id", &self.current_hyperlink_id)
            .field("hyperlink_url", &self.current_hyperlink_url)
            .finish()
    }
}

impl TerminalPerformer {
    /// Creates a new terminal performer
    pub fn new(screen: Arc<Mutex<Screen>>) -> Self {
        Self {
            screen,
            attributes: ScreenCellAttributes::default(),
            sgr_state: HashMap::new(),
            current_hyperlink_id: None,
            current_hyperlink_url: None,
            osc_params: Vec::new(),
        }
    }

    /// Get a reference to the screen
    pub fn screen(&self) -> &Arc<Mutex<Screen>> {
        &self.screen
    }

    /// Reset all text attributes
    fn reset_attributes(&mut self) {
        self.attributes = ScreenCellAttributes::default();
        self.sgr_state.clear();
    }

    /// Reset hyperlink state
    fn reset_hyperlink(&mut self) {
        self.current_hyperlink_id = None;
        self.current_hyperlink_url = None;
        self.attributes.hyperlink = false;
        self.attributes.hyperlink_url = None;
    }

    /// Parse and handle SGR (Select Graphic Rendition) parameters
    fn handle_sgr_params(&mut self, params: &vte::Params) {
        if params.is_empty() {
            // Reset attributes if no parameters
            self.reset_attributes();
            return;
        }

        for param_values in params.iter().flatten() {
            let param = *param_values;
            match param {
                0 => {
                    // Reset all attributes
                    self.reset_attributes();
                }
                1 => {
                    // Bold
                    self.attributes.bold = true;
                    self.sgr_state.insert(1, true);
                }
                2 => {
                    // Faint/dim
                    self.attributes.dim = true;
                    self.sgr_state.insert(2, true);
                }
                3 => {
                    // Italic
                    self.attributes.italic = true;
                    self.sgr_state.insert(3, true);
                }
                4 => {
                    // Underline
                    self.attributes.underline = true;
                    self.attributes.double_underline = false; // Single underline, not double
                    self.sgr_state.insert(4, true);
                }
                5 | 6 => {
                    // Blink (slow or rapid)
                    self.attributes.blink = true;
                    self.sgr_state.insert(param, true);
                }
                7 => {
                    // Reverse
                    self.attributes.reverse = true;
                    self.sgr_state.insert(7, true);
                }
                8 => {
                    // Conceal/Hidden
                    self.attributes.conceal = true;
                    self.sgr_state.insert(8, true);
                }
                9 => {
                    // Strikethrough
                    self.attributes.strikethrough = true;
                    self.sgr_state.insert(9, true);
                }
                10 => {
                    // Primary (default) font
                    self.attributes.font = 0;
                    self.sgr_state.insert(10, true);
                }
                11..=19 => {
                    // Alternative fonts (1-9)
                    self.attributes.font = (param - 10) as u8;
                    self.sgr_state.insert(param, true);
                }
                20 => {
                    // Fraktur (Gothic)
                    self.attributes.fraktur = true;
                    self.sgr_state.insert(20, true);
                }
                21 => {
                    // Double underline
                    self.attributes.underline = true;
                    self.attributes.double_underline = true;
                    self.sgr_state.insert(21, true);
                }
                22 => {
                    // Normal intensity (not bold and not faint)
                    self.attributes.bold = false;
                    self.attributes.dim = false;
                    self.sgr_state.remove(&1);
                    self.sgr_state.remove(&2);
                }
                23 => {
                    // Not italic, not fraktur
                    self.attributes.italic = false;
                    self.attributes.fraktur = false;
                    self.sgr_state.remove(&3);
                    self.sgr_state.remove(&20);
                }
                24 => {
                    // Not underlined (single or double)
                    self.attributes.underline = false;
                    self.attributes.double_underline = false;
                    self.sgr_state.remove(&4);
                    self.sgr_state.remove(&21);
                }
                25 => {
                    // Not blinking
                    self.attributes.blink = false;
                    self.sgr_state.remove(&5);
                    self.sgr_state.remove(&6);
                }
                26 => {
                    // Reserved - Proportional spacing control - not implemented
                }
                27 => {
                    // Not reversed
                    self.attributes.reverse = false;
                    self.sgr_state.remove(&7);
                }
                28 => {
                    // Reveal (not concealed)
                    self.attributes.conceal = false;
                    self.sgr_state.remove(&8);
                }
                29 => {
                    // Not strikethrough
                    self.attributes.strikethrough = false;
                    self.sgr_state.remove(&9);
                }
                30..=37 => {
                    // Basic foreground color
                    self.attributes.fg_color = Some(TerminalColor::Basic(param as u8 - 30));
                }
                38 => {
                    // Extended foreground color - handled in the SGR dispatch
                    // We can't handle it here with flattened params, as we need access to
                    // subsequent parameters which will come as separate items
                }
                39 => {
                    // Default foreground color
                    self.attributes.fg_color = None;
                }
                40..=47 => {
                    // Basic background color
                    self.attributes.bg_color = Some(TerminalColor::Basic(param as u8 - 40));
                }
                48 => {
                    // Extended background color - handled in the SGR dispatch
                    // We can't handle it here with flattened params, as we need access to
                    // subsequent parameters which will come as separate items
                }
                49 => {
                    // Default background color
                    self.attributes.bg_color = None;
                }
                51 => {
                    // Framed
                    self.attributes.framed = true;
                    self.attributes.encircled = false;
                    self.sgr_state.insert(51, true);
                }
                52 => {
                    // Encircled
                    self.attributes.framed = false;
                    self.attributes.encircled = true;
                    self.sgr_state.insert(52, true);
                }
                53 => {
                    // Overlined
                    self.attributes.overlined = true;
                    self.sgr_state.insert(53, true);
                }
                54 => {
                    // Not framed, not encircled
                    self.attributes.framed = false;
                    self.attributes.encircled = false;
                    self.sgr_state.remove(&51);
                    self.sgr_state.remove(&52);
                }
                55 => {
                    // Not overlined
                    self.attributes.overlined = false;
                    self.sgr_state.remove(&53);
                }
                60..=65 => {
                    // Ideogram attributes (not implemented, but tracked)
                    self.sgr_state.insert(param, true);
                }
                73 => {
                    // Superscript
                    self.attributes.superscript = true;
                    self.attributes.subscript = false;
                    self.sgr_state.insert(73, true);
                }
                74 => {
                    // Subscript
                    self.attributes.subscript = true;
                    self.attributes.superscript = false;
                    self.sgr_state.insert(74, true);
                }
                75 => {
                    // Neither superscript nor subscript
                    self.attributes.superscript = false;
                    self.attributes.subscript = false;
                    self.sgr_state.remove(&73);
                    self.sgr_state.remove(&74);
                }
                90..=97 => {
                    // Bright foreground color
                    self.attributes.fg_color = Some(TerminalColor::Basic(param as u8 - 90 + 8));
                }
                100..=107 => {
                    // Bright background color
                    self.attributes.bg_color = Some(TerminalColor::Basic(param as u8 - 100 + 8));
                }
                _ => {
                    // Ignore unsupported SGR codes but trace for debugging
                    debug!("Unsupported SGR parameter: {}", param);
                }
            }
        }
    }
}

// Additional methods for TerminalPerformer outside of the Perform trait
impl TerminalPerformer {
    /// Handle SGR (Select Graphic Rendition) parameters and extended color sequences
    fn handle_sgr_dispatch(&mut self, params: &vte::Params) {
        // Process the basic SGR parameters
        self.handle_sgr_params(params);

        // Handle extended color params (38, 48) manually since they require sequences
        let param_arrays: Vec<Vec<u16>> = params.iter().map(|p| p.to_vec()).collect();

        if param_arrays.len() >= 3 {
            let mut i = 0;
            while i < param_arrays.len() {
                if param_arrays[i].len() == 1 {
                    if param_arrays[i][0] == 38 && i + 2 < param_arrays.len() {
                        // Extended foreground color
                        if param_arrays[i + 1].len() == 1
                            && param_arrays[i + 1][0] == 5
                            && param_arrays[i + 2].len() == 1
                        {
                            // 8-bit color (256 colors)
                            let color = param_arrays[i + 2][0] as u8;
                            self.attributes.fg_color = Some(TerminalColor::Color256(color));
                            i += 3;
                            continue;
                        } else if param_arrays[i + 1].len() == 1
                            && param_arrays[i + 1][0] == 2
                            && i + 4 < param_arrays.len()
                            && param_arrays[i + 2].len() == 1
                            && param_arrays[i + 3].len() == 1
                            && param_arrays[i + 4].len() == 1
                        {
                            // 24-bit RGB color
                            let r = param_arrays[i + 2][0] as u8;
                            let g = param_arrays[i + 3][0] as u8;
                            let b = param_arrays[i + 4][0] as u8;
                            self.attributes.fg_color = Some(TerminalColor::TrueColor { r, g, b });
                            i += 5;
                            continue;
                        }
                    } else if param_arrays[i][0] == 48 && i + 2 < param_arrays.len() {
                        // Extended background color
                        if param_arrays[i + 1].len() == 1
                            && param_arrays[i + 1][0] == 5
                            && param_arrays[i + 2].len() == 1
                        {
                            // 8-bit color (256 colors)
                            let color = param_arrays[i + 2][0] as u8;
                            self.attributes.bg_color = Some(TerminalColor::Color256(color));
                            i += 3;
                            continue;
                        } else if param_arrays[i + 1].len() == 1
                            && param_arrays[i + 1][0] == 2
                            && i + 4 < param_arrays.len()
                            && param_arrays[i + 2].len() == 1
                            && param_arrays[i + 3].len() == 1
                            && param_arrays[i + 4].len() == 1
                        {
                            // 24-bit RGB color
                            let r = param_arrays[i + 2][0] as u8;
                            let g = param_arrays[i + 3][0] as u8;
                            let b = param_arrays[i + 4][0] as u8;
                            self.attributes.bg_color = Some(TerminalColor::TrueColor { r, g, b });
                            i += 5;
                            continue;
                        }
                    }
                }
                i += 1;
            }
        }
    }

    /// Handle OSC (Operating System Command) sequences
    fn handle_osc_params(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }

        // Convert the params to strings for easier handling
        let param_strings: Vec<std::borrow::Cow<'_, str>> =
            params.iter().map(|p| String::from_utf8_lossy(p)).collect();

        if param_strings.is_empty() {
            return;
        }

        // Handle known OSC sequences
        if param_strings[0] == "8" && param_strings.len() >= 3 {
            // OSC 8: Hyperlink
            // Format: OSC 8 ; params ; URI ST

            // Get hyperlink parameters and URL
            let params = if param_strings.len() > 1 {
                param_strings[1].to_string()
            } else {
                String::new()
            };

            let url = if param_strings.len() > 2 {
                param_strings[2].to_string()
            } else {
                String::new()
            };

            // Parse parameters (id=value format)
            let mut hyperlink_id = None;
            for param in params.split(':') {
                let parts: Vec<&str> = param.split('=').collect();
                if parts.len() == 2 && parts[0] == "id" {
                    hyperlink_id = Some(parts[1].to_string());
                }
            }

            // Handle hyperlinks
            if url.is_empty() {
                // Empty URL means end of hyperlink
                self.reset_hyperlink();
            } else {
                // Start of hyperlink
                self.attributes.hyperlink = true;
                self.attributes.hyperlink_url = Some(url.clone());
                self.current_hyperlink_url = Some(url);

                if let Some(id) = hyperlink_id {
                    self.current_hyperlink_id = Some(id);
                }
            }
        }
        // Add support for other OSC sequences here (window title, color definitions, etc.)
    }
}

// Implement the VTE Perform trait
impl Perform for TerminalPerformer {
    fn print(&mut self, c: char) {
        let screen = self.screen.clone();
        let attributes = self.attributes.clone();

        tokio::spawn(async move {
            let mut screen_guard = screen.lock().unwrap();
            let screen = &mut *screen_guard;
            screen.put_char(c, attributes);
        });
    }

    fn execute(&mut self, byte: u8) {
        let screen = self.screen.clone();
        tokio::spawn(async move {
            let mut screen_guard = screen.lock().unwrap();
            let screen = &mut *screen_guard;
            match byte {
                b'\r' => screen.carriage_return(),
                b'\n' => {
                    screen.carriage_return();
                    screen.linefeed();
                }
                b'\t' => {
                    let current_col = screen.cursor_col();
                    let new_col = (current_col + 8) & !7;
                    let current_row = screen.cursor_row();
                    screen.move_cursor(current_row, new_col);
                }
                b'\x08' => {
                    if screen.cursor_col() > 0 {
                        let current_row = screen.cursor_row();
                        let new_col = screen.cursor_col() - 1;
                        screen.move_cursor(current_row, new_col);
                    }
                }
                b'\x0C' => {
                    screen.clear();
                }
                b'\x07' => {}
                _ => {
                    debug!("Unhandled execute: {:?}", byte);
                }
            }
        });
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        c: char,
    ) {
        if c == 'm' {
            self.handle_sgr_dispatch(params);
            return;
        }

        let screen = self.screen.clone(); // Clone the Arc<Mutex<Screen>> to move into the async block
        // Extract parameter values before spawning to avoid lifetime issues
        let param_values: Vec<Vec<u16>> = params.iter().map(|p| p.to_vec()).collect();

        tokio::spawn(async move {
            let mut screen_guard = screen.lock().unwrap();
            let screen = &mut *screen_guard;
            match c {
                'A' => {
                    // Cursor Up
                    let n = param_values
                        .first()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(1) as usize;
                    let current_row = screen.cursor_row();
                    let new_row = current_row.saturating_sub(n);
                    let current_col = screen.cursor_col();
                    screen.move_cursor(new_row, current_col);
                }
                'B' => {
                    // Cursor Down
                    let n = param_values
                        .first()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(1) as usize;
                    let current_row = screen.cursor_row();
                    let current_col = screen.cursor_col();
                    screen.move_cursor(current_row + n, current_col);
                }
                'C' => {
                    // Cursor Forward
                    let n = param_values
                        .first()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(1) as usize;
                    let current_row = screen.cursor_row();
                    let current_col = screen.cursor_col();
                    screen.move_cursor(current_row, current_col + n);
                }
                'D' => {
                    // Cursor Back
                    let n = param_values
                        .first()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(1) as usize;
                    let current_row = screen.cursor_row();
                    let current_col = screen.cursor_col();
                    let new_col = current_col.saturating_sub(n);
                    screen.move_cursor(current_row, new_col);
                }
                'H' | 'f' => {
                    // Cursor Position
                    let row = param_values
                        .first()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(1) as usize;
                    let col = param_values
                        .get(1)
                        .and_then(|p| p.first().copied())
                        .unwrap_or(1) as usize;
                    // Convert 1-based to 0-based
                    let row = row.saturating_sub(1);
                    let col = col.saturating_sub(1);
                    screen.move_cursor(row, col);
                }
                'J' => {
                    // Erase in Display
                    let n = param_values
                        .first()
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
                        1 => {
                            // Clear from beginning of screen to cursor
                            let row = screen.cursor_row();
                            let col = screen.cursor_col();

                            // Clear current line up to cursor
                            if row < screen.lines.len() {
                                for i in 0..=col.min(screen.lines[row].len().saturating_sub(1)) {
                                    screen.lines[row][i] = ScreenCell::default();
                                }
                            }

                            // Clear all lines above cursor
                            for i in 0..row {
                                if i < screen.lines.len() {
                                    screen.lines[i] = vec![ScreenCell::default(); screen.columns];
                                }
                            }
                        }
                        2 => {
                            // Clear entire screen
                            screen.clear();
                        }
                        3 => {
                            // Clear entire screen and delete scrollback buffer
                            screen.clear();
                            // In a real terminal, this would also clear scrollback
                        }
                        _ => debug!("Unhandled erase in display: {}", n),
                    }
                }
                'K' => {
                    // Erase in Line
                    let n = param_values
                        .first()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(0);
                    match n {
                        0 => screen.clear_line_forward(),
                        1 => {
                            // Clear from start of line to cursor
                            let row = screen.cursor_row();
                            let col = screen.cursor_col();

                            if row < screen.lines.len() {
                                for i in 0..=col.min(screen.lines[row].len().saturating_sub(1)) {
                                    screen.lines[row][i] = ScreenCell::default();
                                }
                            }
                        }
                        2 => screen.clear_line(),
                        _ => debug!("Unhandled erase in line: {}", n),
                    }
                }
                'S' => {
                    // Scroll up
                    let n = param_values
                        .first()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(1) as usize;

                    for _ in 0..n {
                        screen.scroll_up();
                    }
                }
                'T' => {
                    // Scroll down
                    let n = param_values
                        .first()
                        .and_then(|p| p.first().copied())
                        .unwrap_or(1) as usize;

                    // Implement scroll down by adding empty lines at the top
                    let columns = screen.columns; // Copy the columns value

                    for _ in 0..n {
                        screen
                            .lines
                            .push_front(vec![ScreenCell::default(); columns]);
                        if screen.lines.len() > screen.max_lines {
                            screen.lines.pop_back();
                        }
                    }

                    // Adjust cursor position
                    let new_row = screen.cursor_row() + n;
                    let cursor_col = screen.cursor_col(); // Copy the cursor column
                    screen.move_cursor(new_row, cursor_col);
                }
                // 'm' case is now handled separately before acquiring the screen lock
                _ => {
                    debug!("Unhandled CSI: {:?} {:?}", param_values, c);
                }
            }
        });
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if intermediates.is_empty() {
            match byte {
                b'c' => {
                    // RIS - Reset to Initial State
                    self.reset_attributes();
                    let screen = self.screen.clone();
                    tokio::spawn(async move {
                        let mut screen_guard = screen.lock().unwrap();
                        screen_guard.clear();
                    });
                }
                b'7' => {
                    // DECSC - Save Cursor
                    // Not implemented yet
                }
                b'8' => {
                    // DECRC - Restore Cursor
                    // Not implemented yet
                }
                _ => debug!("Unhandled ESC dispatch: {:?}", byte),
            }
        }
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

    /// Creates a new terminal emulator with specified maximum lines
    pub fn new_with_max_lines(columns: usize, max_lines: usize) -> Self {
        let screen = Arc::new(Mutex::new(Screen::new_with_max_lines(columns, max_lines)));
        let performer = TerminalPerformer::new(screen.clone());

        Self { performer, screen }
    }

    /// Process input and update screen state
    pub fn process(&mut self, data: &str) {
        let mut parser = Parser::new();

        // Process data in chunks to avoid excessive locking
        let chunk_size = 4096;
        let data_bytes = data.as_bytes();

        for chunk in data_bytes.chunks(chunk_size) {
            parser.advance(&mut self.performer, chunk);
        }
    }

    /// Process input with limited buffer (for large outputs)
    pub fn process_with_limited_buffer(&mut self, data: &str, max_lines: usize) {
        if let Ok(mut guard) = self.screen.lock() {
            // Update max_lines setting
            guard.max_lines = max_lines.min(MAX_SCREEN_LINES);
        }

        self.process(data);

        // After processing, check if we need to smart truncate
        if let Ok(mut guard) = self.screen.lock()
            && guard.lines.len() > max_lines
        {
            guard.smart_truncate(max_lines);
        }
    }

    /// Get the current screen state
    pub fn get_screen(&self) -> Arc<Mutex<Screen>> {
        self.screen.clone()
    }

    /// Get the screen contents as a vector of strings
    pub fn display(&self) -> Vec<String> {
        if let Ok(guard) = self.screen.lock() {
            guard.display()
        } else {
            warn!("Failed to lock screen for display");
            vec![]
        }
    }

    /// Get the screen contents as plain text
    pub fn to_plain_text(&self) -> String {
        if let Ok(guard) = self.screen.lock() {
            guard.to_plain_text()
        } else {
            warn!("Failed to lock screen for to_plain_text");
            String::new()
        }
    }

    /// Clear the screen
    pub fn clear(&mut self) {
        if let Ok(mut guard) = self.screen.lock() {
            guard.clear();
        } else {
            warn!("Failed to lock screen for clear");
        }
    }
}

/// Type definition for cache entries to simplify complex types
type CacheEntryMap = HashMap<u64, (Vec<String>, Instant)>;

/// Caching system for terminal output rendering
#[derive(Debug, Clone)]
struct TerminalCache {
    /// Cache entries mapping text content to rendered output
    entries: Arc<RwLock<CacheEntryMap>>,
    /// Maximum number of entries in the cache
    max_entries: usize,
    /// Time-to-live for cache entries in seconds
    ttl: u64,
}

impl TerminalCache {
    /// Create a new terminal cache
    fn new(max_entries: usize, ttl: u64) -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            max_entries,
            ttl,
        }
    }

    /// Calculate hash of the key
    fn calculate_hash(&self, key: &str) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    /// Get a cached value if available and not expired
    async fn get(&self, key: &str) -> Option<Vec<String>> {
        let hash = self.calculate_hash(key);
        let entries = self.entries.read().await;
        if let Some((value, timestamp)) = entries.get(&hash)
            && timestamp.elapsed().as_secs() < self.ttl {
                return Some(value.clone());
            }
        None
    }

    /// Insert a value into the cache
    async fn insert(&self, key: &str, value: Vec<String>) {
        let hash = self.calculate_hash(key);
        let mut entries = self.entries.write().await;
        // Insert the new entry
        entries.insert(hash, (value, Instant::now()));

        // Clean up old entries if cache is too large
        if entries.len() > self.max_entries {
            // Remove expired entries first
            entries.retain(|_, (_, timestamp)| timestamp.elapsed().as_secs() < self.ttl);

            // If still too many entries, remove oldest
            if entries.len() > self.max_entries {
                let mut entries_vec: Vec<_> = entries.iter().collect();
                entries_vec.sort_by_key(|(_, (_, timestamp))| *timestamp);

                let to_remove = entries_vec.len() - self.max_entries;
                let keys_to_remove: Vec<u64> = entries_vec
                    .iter()
                    .take(to_remove)
                    .map(|(k, _)| **k)
                    .collect();

                for key in keys_to_remove {
                    entries.remove(&key);
                }
            }
        }
    }

    /// Clear expired entries from the cache
    async fn cleanup(&self) {
        let mut entries = self.entries.write().await;
        entries.retain(|_, (_, timestamp)| timestamp.elapsed().as_secs() < self.ttl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::runtime::Runtime;

    #[test]
    fn test_terminal_cache() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let cache = TerminalCache::new(100, 300);

            // Insert a value
            cache
                .insert("key1", vec!["line1".to_string(), "line2".to_string()])
                .await;

            // Retrieve the value
            let retrieved = cache.get("key1").await;
            assert_eq!(
                retrieved,
                Some(vec!["line1".to_string(), "line2".to_string()])
            );

            // Unknown key should return None
            let not_found = cache.get("unknown").await;
            assert_eq!(not_found, None);
        });
    }
}
