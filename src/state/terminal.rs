use regex::Regex;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tracing::{debug, warn};
use vte::{Parser, Perform};

// Import our enhanced ANSI code module
#[allow(unused_imports)]
use crate::state::ansi_codes;

/// Maximum number of lines to keep in the screen buffer
pub const MAX_SCREEN_LINES: usize = 10000;
/// Default maximum number of lines to keep in the screen buffer
pub const DEFAULT_MAX_SCREEN_LINES: usize = 500;
/// Maximum number of columns for the screen. Must match the PTY width
/// (`pty::DEFAULT_COLS`) so emulator-rendered scrollback wraps exactly where the
/// real terminal did, instead of silently re-wrapping long lines at a narrower
/// width.
const DEFAULT_COLUMNS: usize = 200;
/// Maximum output size in bytes to prevent excessive memory usage
pub const MAX_OUTPUT_SIZE: usize = 500_000;
/// Maximum cache entry lifetime in seconds
const CACHE_TTL: u64 = 300; // 5 minutes

/// Compact bitset for terminal text styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellStyle(u32);

impl CellStyle {
    pub const BOLD: Self = Self(1 << 0);
    pub const UNDERLINE: Self = Self(1 << 1);
    pub const BLINK: Self = Self(1 << 2);
    pub const REVERSE: Self = Self(1 << 3);
    pub const ITALIC: Self = Self(1 << 4);
    pub const STRIKETHROUGH: Self = Self(1 << 5);
    pub const DIM: Self = Self(1 << 6);
    pub const DOUBLE_UNDERLINE: Self = Self(1 << 7);
    pub const FRAMED: Self = Self(1 << 8);
    pub const ENCIRCLED: Self = Self(1 << 9);
    pub const OVERLINED: Self = Self(1 << 10);
    pub const FRAKTUR: Self = Self(1 << 11);
    pub const CONCEAL: Self = Self(1 << 12);
    pub const SUPERSCRIPT: Self = Self(1 << 13);
    pub const SUBSCRIPT: Self = Self(1 << 14);
    pub const HYPERLINK: Self = Self(1 << 15);

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 != 0
    }

    pub fn set(&mut self, flag: Self, enabled: bool) {
        if enabled {
            self.0 |= flag.0;
        } else {
            self.0 &= !flag.0;
        }
    }
}

/// Container for all possible character attributes
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScreenCellAttributes {
    /// Text style flags such as bold, underline, reverse, and blink.
    pub style: CellStyle,
    /// Foreground color
    pub fg_color: Option<TerminalColor>,
    /// Background color
    pub bg_color: Option<TerminalColor>,
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
    /// Text style flags such as bold, underline, reverse, and blink.
    pub style: CellStyle,
    /// Foreground color (0-255 for 8-bit colors, RGB for 24-bit colors)
    pub fg_color: Option<TerminalColor>,
    /// Background color (0-255 for 8-bit colors, RGB for 24-bit colors)
    pub bg_color: Option<TerminalColor>,
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

impl ScreenCell {
    fn new(character: char, attributes: ScreenCellAttributes) -> Self {
        Self {
            character,
            style: attributes.style,
            fg_color: attributes.fg_color,
            bg_color: attributes.bg_color,
            hyperlink_url: attributes.hyperlink_url,
            font: attributes.font,
        }
    }
}

impl Default for ScreenCell {
    fn default() -> Self {
        Self {
            character: ' ',
            style: CellStyle::default(),
            fg_color: None,
            bg_color: None,
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
        // A 0-width screen makes `ensure_cursor_position` compute `columns - 1`,
        // which underflows (panic in debug, `usize::MAX` in release). Floor at 1.
        let columns = columns.max(1);
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
        // See `new`: a 0-width screen underflows in `ensure_cursor_position`.
        let columns = columns.max(1);
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
            self.lines.push_back(vec![ScreenCell::default(); self.columns]);
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
    pub fn put_char(&mut self, c: char, attributes: ScreenCellAttributes) {
        self.ensure_cursor_position();

        // Get the current cursor position
        let row = self.cursor_position.0;
        let col = self.cursor_position.1;

        // Put the character at the cursor position
        if col < self.lines[row].len() {
            self.lines[row][col] = ScreenCell::new(c, attributes);
        } else {
            // Add cells if needed
            while self.lines[row].len() <= col {
                self.lines[row].push(ScreenCell::default());
            }
            self.lines[row][col] = ScreenCell::new(c, attributes);
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
        style: CellStyle,
        fg_color: Option<TerminalColor>,
        bg_color: Option<TerminalColor>,
    ) {
        let attributes = ScreenCellAttributes { style, fg_color, bg_color, ..Default::default() };

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
        self.lines.push_back(vec![ScreenCell::default(); self.columns]);
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
            let beginning: VecDeque<Vec<ScreenCell>> =
                self.lines.drain(0..beginning_lines.min(self.lines.len())).collect();

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
                        style: CellStyle::BOLD.union(CellStyle::REVERSE),
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
    osc_params: Vec<String>,
}

// Custom debug implementation to avoid using the one from VTE
impl std::fmt::Debug for TerminalPerformer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalPerformer")
            .field("attributes", &self.attributes)
            .field("hyperlink_id", &self.current_hyperlink_id)
            .field("hyperlink_url", &self.current_hyperlink_url)
            .finish_non_exhaustive()
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
        self.attributes.style.set(CellStyle::HYPERLINK, false);
        self.attributes.hyperlink_url = None;
    }

    fn track_sgr(&mut self, param: u16) {
        self.sgr_state.insert(param, true);
    }

    fn untrack_sgr(&mut self, params: &[u16]) {
        for param in params {
            self.sgr_state.remove(param);
        }
    }

    /// Parse and handle SGR (Select Graphic Rendition) parameters
    fn handle_sgr_params(&mut self, params: &vte::Params) {
        if params.is_empty() {
            self.reset_attributes();
            return;
        }

        for param_values in params.iter().flatten() {
            self.handle_sgr_param(*param_values);
        }
    }

    fn handle_sgr_param(&mut self, param: u16) {
        if self.handle_basic_sgr_style(param)
            || self.handle_font_sgr(param)
            || self.handle_color_sgr(param)
            || self.handle_frame_sgr(param)
            || self.handle_script_sgr(param)
        {
            return;
        }

        debug!("Unsupported SGR parameter: {}", param);
    }

    fn handle_basic_sgr_style(&mut self, param: u16) -> bool {
        match param {
            0 => self.reset_attributes(),
            1 => self.attributes.style.set(CellStyle::BOLD, true),
            2 => self.attributes.style.set(CellStyle::DIM, true),
            3 => self.attributes.style.set(CellStyle::ITALIC, true),
            4 => {
                self.attributes.style.set(CellStyle::UNDERLINE, true);
                self.attributes.style.set(CellStyle::DOUBLE_UNDERLINE, false);
            }
            5 | 6 => self.attributes.style.set(CellStyle::BLINK, true),
            7 => self.attributes.style.set(CellStyle::REVERSE, true),
            8 => self.attributes.style.set(CellStyle::CONCEAL, true),
            9 => self.attributes.style.set(CellStyle::STRIKETHROUGH, true),
            20 => self.attributes.style.set(CellStyle::FRAKTUR, true),
            21 => {
                self.attributes.style.set(CellStyle::UNDERLINE, true);
                self.attributes.style.set(CellStyle::DOUBLE_UNDERLINE, true);
            }
            22 => {
                self.attributes.style.set(CellStyle::BOLD, false);
                self.attributes.style.set(CellStyle::DIM, false);
                self.untrack_sgr(&[1, 2]);
                return true;
            }
            23 => {
                self.attributes.style.set(CellStyle::ITALIC, false);
                self.attributes.style.set(CellStyle::FRAKTUR, false);
                self.untrack_sgr(&[3, 20]);
                return true;
            }
            24 => {
                self.attributes.style.set(CellStyle::UNDERLINE, false);
                self.attributes.style.set(CellStyle::DOUBLE_UNDERLINE, false);
                self.untrack_sgr(&[4, 21]);
                return true;
            }
            25 => {
                self.attributes.style.set(CellStyle::BLINK, false);
                self.untrack_sgr(&[5, 6]);
                return true;
            }
            27 => self.attributes.style.set(CellStyle::REVERSE, false),
            28 => self.attributes.style.set(CellStyle::CONCEAL, false),
            29 => self.attributes.style.set(CellStyle::STRIKETHROUGH, false),
            _ => return false,
        }

        self.track_sgr(param);
        true
    }

    fn handle_font_sgr(&mut self, param: u16) -> bool {
        match param {
            10 => self.attributes.font = 0,
            11..=19 => self.attributes.font = (param - 10) as u8,
            _ => return false,
        }

        self.track_sgr(param);
        true
    }

    fn handle_color_sgr(&mut self, param: u16) -> bool {
        match param {
            26 | 38 | 48 => {}
            30..=37 => self.attributes.fg_color = Some(TerminalColor::Basic(param as u8 - 30)),
            39 => self.attributes.fg_color = None,
            40..=47 => self.attributes.bg_color = Some(TerminalColor::Basic(param as u8 - 40)),
            49 => self.attributes.bg_color = None,
            90..=97 => self.attributes.fg_color = Some(TerminalColor::Basic(param as u8 - 90 + 8)),
            100..=107 => {
                self.attributes.bg_color = Some(TerminalColor::Basic(param as u8 - 100 + 8));
            }
            _ => return false,
        }

        true
    }

    fn handle_frame_sgr(&mut self, param: u16) -> bool {
        match param {
            51 => {
                self.attributes.style.set(CellStyle::FRAMED, true);
                self.attributes.style.set(CellStyle::ENCIRCLED, false);
            }
            52 => {
                self.attributes.style.set(CellStyle::FRAMED, false);
                self.attributes.style.set(CellStyle::ENCIRCLED, true);
            }
            53 => self.attributes.style.set(CellStyle::OVERLINED, true),
            54 => {
                self.attributes.style.set(CellStyle::FRAMED, false);
                self.attributes.style.set(CellStyle::ENCIRCLED, false);
                self.untrack_sgr(&[51, 52]);
                return true;
            }
            55 => {
                self.attributes.style.set(CellStyle::OVERLINED, false);
                self.untrack_sgr(&[53]);
                return true;
            }
            60..=65 => {}
            _ => return false,
        }

        self.track_sgr(param);
        true
    }

    fn handle_script_sgr(&mut self, param: u16) -> bool {
        match param {
            73 => {
                self.attributes.style.set(CellStyle::SUPERSCRIPT, true);
                self.attributes.style.set(CellStyle::SUBSCRIPT, false);
            }
            74 => {
                self.attributes.style.set(CellStyle::SUBSCRIPT, true);
                self.attributes.style.set(CellStyle::SUPERSCRIPT, false);
            }
            75 => {
                self.attributes.style.set(CellStyle::SUPERSCRIPT, false);
                self.attributes.style.set(CellStyle::SUBSCRIPT, false);
                self.untrack_sgr(&[73, 74]);
                return true;
            }
            _ => return false,
        }

        self.track_sgr(param);
        true
    }
}

// Additional methods for TerminalPerformer outside of the Perform trait
impl TerminalPerformer {
    /// Handle SGR (Select Graphic Rendition) parameters and extended color sequences
    fn handle_sgr_dispatch(&mut self, params: &vte::Params) {
        // Process the basic SGR parameters
        self.handle_sgr_params(params);

        // Handle extended color params (38, 48) manually since they require sequences
        let param_arrays: Vec<Vec<u16>> = params.iter().map(<[u16]>::to_vec).collect();

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
        let param_strings: Vec<String> =
            params.iter().map(|p| String::from_utf8_lossy(p).to_string()).collect();

        if param_strings.is_empty() {
            return;
        }

        // Handle known OSC sequences
        if param_strings[0] == "8" && param_strings.len() >= 3 {
            // OSC 8: Hyperlink
            // Format: OSC 8 ; params ; URI ST

            // Get hyperlink parameters and URL
            let params =
                if param_strings.len() > 1 { param_strings[1].clone() } else { String::new() };

            let url =
                if param_strings.len() > 2 { param_strings[2].clone() } else { String::new() };

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
                self.attributes.style.set(CellStyle::HYPERLINK, true);
                self.attributes.hyperlink_url = Some(url.clone());
                self.current_hyperlink_url = Some(url);

                if let Some(id) = hyperlink_id {
                    self.current_hyperlink_id = Some(id);
                }
            }
        }
        // Add support for other OSC sequences here (window title, color definitions, etc.)
    }

    fn csi_param(params: &vte::Params, index: usize, default: u16) -> u16 {
        params.iter().nth(index).and_then(|p| p.first().copied()).unwrap_or(default)
    }

    fn handle_cursor_csi(screen: &mut Screen, params: &vte::Params, c: char) -> bool {
        let n = usize::from(Self::csi_param(params, 0, 1));
        let current_row = screen.cursor_row();
        let current_col = screen.cursor_col();

        match c {
            'A' => screen.move_cursor(current_row.saturating_sub(n), current_col),
            'B' => screen.move_cursor(current_row + n, current_col),
            'C' => screen.move_cursor(current_row, current_col + n),
            'D' => screen.move_cursor(current_row, current_col.saturating_sub(n)),
            'H' | 'f' => {
                let row = usize::from(Self::csi_param(params, 0, 1)).saturating_sub(1);
                let col = usize::from(Self::csi_param(params, 1, 1)).saturating_sub(1);
                screen.move_cursor(row, col);
            }
            _ => return false,
        }

        true
    }

    fn clear_line_to_cursor(screen: &mut Screen) {
        let row = screen.cursor_row();
        let col = screen.cursor_col();

        if row < screen.lines.len() {
            for i in 0..=col.min(screen.lines[row].len().saturating_sub(1)) {
                screen.lines[row][i] = ScreenCell::default();
            }
        }
    }

    fn handle_erase_csi(screen: &mut Screen, params: &vte::Params, c: char) -> bool {
        match c {
            'J' => Self::handle_erase_display(screen, Self::csi_param(params, 0, 0)),
            'K' => Self::handle_erase_line(screen, Self::csi_param(params, 0, 0)),
            _ => return false,
        }

        true
    }

    fn handle_erase_display(screen: &mut Screen, mode: u16) {
        match mode {
            0 => {
                screen.clear_line_forward();
                let row = screen.cursor_row();
                if row + 1 < screen.lines.len() {
                    for i in row + 1..screen.lines.len() {
                        screen.lines[i] = vec![ScreenCell::default(); screen.columns];
                    }
                }
            }
            1 => {
                Self::clear_line_to_cursor(screen);
                for i in 0..screen.cursor_row() {
                    if i < screen.lines.len() {
                        screen.lines[i] = vec![ScreenCell::default(); screen.columns];
                    }
                }
            }
            2 | 3 => screen.clear(),
            _ => debug!("Unhandled erase in display: {}", mode),
        }
    }

    fn handle_erase_line(screen: &mut Screen, mode: u16) {
        match mode {
            0 => screen.clear_line_forward(),
            1 => Self::clear_line_to_cursor(screen),
            2 => screen.clear_line(),
            _ => debug!("Unhandled erase in line: {}", mode),
        }
    }

    fn handle_scroll_csi(screen: &mut Screen, params: &vte::Params, c: char) -> bool {
        let n = usize::from(Self::csi_param(params, 0, 1));

        match c {
            'S' => {
                for _ in 0..n {
                    screen.scroll_up();
                }
            }
            'T' => {
                let columns = screen.columns;
                for _ in 0..n {
                    screen.lines.push_front(vec![ScreenCell::default(); columns]);
                    if screen.lines.len() > screen.max_lines {
                        screen.lines.pop_back();
                    }
                }
                screen.move_cursor(screen.cursor_row() + n, screen.cursor_col());
            }
            _ => return false,
        }

        true
    }
}

// Implement the VTE Perform trait
impl Perform for TerminalPerformer {
    fn print(&mut self, c: char) {
        if let Ok(mut screen) = self.screen.lock() {
            screen.put_char(c, self.attributes.clone());
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

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        // Implement OSC parameter handling
        self.handle_osc_params(params, bell_terminated);
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        c: char,
    ) {
        // Special case for SGR ('m') to avoid borrowing conflict
        if c == 'm' {
            self.handle_sgr_dispatch(params);
            return;
        }

        if let Ok(mut screen) = self.screen.lock() {
            if Self::handle_cursor_csi(&mut screen, params, c)
                || Self::handle_erase_csi(&mut screen, params, c)
                || Self::handle_scroll_csi(&mut screen, params, c)
            {
                return;
            }

            debug!("Unhandled CSI: {:?} {:?}", params, c);
        } else {
            warn!("Failed to lock screen for csi_dispatch");
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if intermediates.is_empty() {
            match byte {
                b'c' => {
                    // RIS - Reset to Initial State
                    if let Ok(mut screen) = self.screen.lock() {
                        screen.clear();
                    }
                    self.reset_attributes();
                }
                b'7' | b'8' => {
                    // DECSC/DECRC - Save/restore cursor
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
            .finish_non_exhaustive()
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
        if let Ok(mut screen) = self.screen.lock() {
            // Update max_lines setting
            screen.max_lines = max_lines.min(MAX_SCREEN_LINES);
        }

        self.process(data);

        // After processing, check if we need to smart truncate
        if let Ok(mut screen) = self.screen.lock() {
            if screen.lines.len() > max_lines {
                screen.smart_truncate(max_lines);
            }
        }
    }

    /// Get the current screen state
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
    pub fn to_plain_text(&self) -> String {
        if let Ok(screen) = self.screen.lock() {
            screen.to_plain_text()
        } else {
            warn!("Failed to lock screen for to_plain_text");
            String::new()
        }
    }

    /// Clear the screen
    pub fn clear(&mut self) {
        if let Ok(mut screen) = self.screen.lock() {
            screen.clear();
        } else {
            warn!("Failed to lock screen for clear");
        }
    }
}

/// Map from a 64-bit text hash to (rendered output, insertion time).
///
/// Keyed by a hash of the source text rather than the text itself: a terminal
/// dump can be hundreds of KB, and storing it as the map key (plus cloning it
/// on every eviction) was the dominant cost. A hash collision only causes a
/// stale render to be recomputed, never corruption.
type CacheEntryMap = HashMap<u64, (Vec<String>, Instant)>;

/// Hash a chunk of terminal text into the cache key space.
fn hash_terminal_text(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Inner cache state behind a single lock so the map and the eviction queue can
/// never drift out of sync.
#[derive(Debug, Default)]
struct CacheInner {
    map: CacheEntryMap,
    /// Keys in insertion order, for O(1) FIFO eviction (no O(n log n) sort).
    order: VecDeque<u64>,
}

/// Caching system for terminal output rendering
#[derive(Debug, Clone)]
struct TerminalCache {
    inner: Arc<RwLock<CacheInner>>,
    /// Maximum number of entries in the cache
    max_entries: usize,
    /// Time-to-live for cache entries in seconds
    ttl: u64,
}

impl TerminalCache {
    /// Create a new terminal cache
    fn new(max_entries: usize, ttl: u64) -> Self {
        Self { inner: Arc::new(RwLock::new(CacheInner::default())), max_entries, ttl }
    }

    /// Get a cached value if available and not expired
    fn get(&self, text: &str) -> Option<Vec<String>> {
        let key = hash_terminal_text(text);
        let inner = self.inner.read().ok()?;
        let (value, timestamp) = inner.map.get(&key)?;
        if timestamp.elapsed().as_secs() < self.ttl {
            Some(value.clone())
        } else {
            None
        }
    }

    /// Insert a value into the cache, evicting the oldest entries if over cap.
    fn insert(&self, text: &str, value: Vec<String>) {
        let key = hash_terminal_text(text);
        let Ok(mut inner) = self.inner.write() else {
            return;
        };
        if inner.map.insert(key, (value, Instant::now())).is_none() {
            inner.order.push_back(key);
        }
        while inner.order.len() > self.max_entries {
            let Some(old) = inner.order.pop_front() else {
                break;
            };
            inner.map.remove(&old);
        }
    }

    /// Clear expired entries from the cache
    fn cleanup(&self) {
        let Ok(mut inner) = self.inner.write() else {
            return;
        };
        let ttl = self.ttl;
        let CacheInner { map, order } = &mut *inner;
        map.retain(|_, (_, timestamp)| timestamp.elapsed().as_secs() < ttl);
        order.retain(|key| map.contains_key(key));
    }
}

// Initialize the global terminal cache
lazy_static::lazy_static! {
    static ref TERMINAL_CACHE: TerminalCache = TerminalCache::new(100, CACHE_TTL);
}

/// Terminal output difference detector
#[derive(Debug, Clone)]
pub struct TerminalOutputDiff {
    /// Previous output lines
    previous_output: Vec<String>,
    /// Hash of previous output
    output_hash: String,
    /// Maximum number of lines to compare
    max_lines: usize,
}

impl Default for TerminalOutputDiff {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalOutputDiff {
    /// Create a new terminal output diff detector
    pub fn new() -> Self {
        Self { previous_output: Vec::new(), output_hash: String::new(), max_lines: 1000 }
    }

    /// Create a new terminal output diff detector with specified maximum lines
    pub fn new_with_max_lines(max_lines: usize) -> Self {
        Self { previous_output: Vec::new(), output_hash: String::new(), max_lines }
    }

    /// Detect changes between previous and new output
    pub fn detect_changes(&mut self, new_output: &[String]) -> Vec<String> {
        if self.previous_output.is_empty() {
            // First run, just return all lines
            self.previous_output = new_output.to_vec();
            self.output_hash = self.calculate_hash(new_output);
            return new_output.to_vec();
        }

        // Check if output is identical (fast path)
        let new_hash = self.calculate_hash(new_output);
        if new_hash == self.output_hash {
            return Vec::new(); // No changes
        }

        // Find differences
        let mut changes = Vec::new();

        // Find where new content starts
        let nold = self.previous_output.len().min(self.max_lines);
        let nnew = new_output.len().min(self.max_lines);

        // Try to find where old output ends and new output begins using a more efficient algorithm
        let mut matched_position = None;

        // Check if new output contains all of old output as a prefix
        let is_prefix = nold <= nnew && (0..nold).all(|i| self.previous_output[i] == new_output[i]);

        if is_prefix {
            // Simple case: new output is old output plus additions
            matched_position = Some(nold);
        } else {
            // More complex case: try to find the last matching block
            let mut best_match = 0;
            let mut best_position = 0;

            // Use sliding window approach to find largest match
            let window_size = 3.min(nold); // Use 3 lines as context for matching

            if window_size > 0 {
                for i in (0..=nnew.saturating_sub(window_size)).rev() {
                    // Try matching last window_size lines of old output with window at position i in new output
                    let mut match_count = 0;
                    for j in 0..window_size {
                        if i + j < nnew
                            && nold.saturating_sub(window_size) + j < nold
                            && new_output[i + j]
                                == self.previous_output[nold.saturating_sub(window_size) + j]
                        {
                            match_count += 1;
                        }
                    }

                    if match_count > best_match {
                        best_match = match_count;
                        best_position = i + window_size;

                        if best_match == window_size {
                            // Perfect match, no need to continue
                            break;
                        }
                    }
                }
            }

            if best_match >= window_size / 2 {
                // Found a reasonable match
                matched_position = Some(best_position);
            }
        }

        // Extract changes based on matched position
        if let Some(pos) = matched_position {
            if pos < nnew {
                changes = new_output[pos..].to_vec();

                // Check if first line of changes matches last line of previous output
                if !changes.is_empty()
                    && !self.previous_output.is_empty()
                    && changes[0] == self.previous_output[self.previous_output.len() - 1]
                {
                    changes.remove(0);
                }
            }
        } else {
            // Fallback: couldn't find a good match, show all new lines
            changes = new_output.to_vec();
        }

        // Update state for next comparison
        self.previous_output = new_output.to_vec();
        self.output_hash = new_hash;

        changes
    }

    /// Calculate a hash of the output lines for quick comparison
    fn calculate_hash(&self, lines: &[String]) -> String {
        // Simple hash function based on content
        // In a production setting, use a proper hash function
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for line in lines.iter().take(self.max_lines) {
            std::hash::Hash::hash(line, &mut hasher);
        }
        format!("{:x}", std::hash::Hasher::finish(&hasher))
    }

    /// Reset the diff detector
    pub fn reset(&mut self) {
        self.previous_output.clear();
        self.output_hash.clear();
    }
}

/// Render terminal output with line wrapping
pub fn render_terminal_output(text: &str) -> Vec<String> {
    // Check cache first
    if let Some(cached) = TERMINAL_CACHE.get(text) {
        return cached;
    }

    let mut terminal = TerminalEmulator::new(DEFAULT_COLUMNS);

    // Check if we need to limit processing for large outputs
    if text.len() > MAX_OUTPUT_SIZE {
        // For large outputs, use limited buffer mode
        terminal.process_with_limited_buffer(text, DEFAULT_MAX_SCREEN_LINES);
    } else {
        terminal.process(text);
    }

    let result = terminal.display();

    // Cache the result for future use (only if reasonably sized)
    if text.len() < MAX_OUTPUT_SIZE {
        TERMINAL_CACHE.insert(text, result.clone());
    }

    // Periodically clean up expired cache entries
    if rand::random::<u32>() % 100 == 0 {
        TERMINAL_CACHE.cleanup();
    }

    // IMPORTANT: Strip any remaining ANSI codes from the result lines to prevent JSON-RPC errors.
    // This is the core fix for "invalid character '\x1b'" errors.
    result.into_iter().map(|line| strip_ansi_codes(&line)).collect()
}

/// Get incremental text output by comparing old and new terminal states
pub fn incremental_text(text: &str, last_pending_output: &str) -> String {
    // Optimization: Quick check for empty input
    if text.is_empty() {
        return String::new();
    }

    // Optimization: If last output is empty, just process everything
    if last_pending_output.is_empty() {
        // First call, return all processed lines with leading/trailing whitespace trimmed
        let lines = render_terminal_output(text);
        return lines.join("\n").trim().to_string();
    }

    // Optimization: Handle case where new text is just appended to old text
    let is_append = text.starts_with(last_pending_output);

    if is_append && text.len() > last_pending_output.len() {
        // Incremental case - only process the new part
        let new_part = &text[last_pending_output.len()..];

        // Ensure we have enough context by including a bit more than just the new
        // part. Snap the start down to a char boundary: a raw `len - 200` offset
        // can land inside a multibyte code point and panic on the slice.
        let context_len = 200.min(last_pending_output.len());
        let full_context = if context_len > 0 {
            let start_pos = crate::utils::floor_char_boundary(
                last_pending_output,
                last_pending_output.len() - context_len,
            );
            format!("{}{}", &last_pending_output[start_pos..], new_part)
        } else {
            new_part.to_string()
        };

        // Process the combined output for context
        let previous_lines = render_terminal_output(last_pending_output);
        let combined_lines = render_terminal_output(&full_context);

        // Create a diff detector for efficient comparison
        let mut diff_detector = TerminalOutputDiff::new();
        diff_detector.previous_output = previous_lines;

        // Get just the changes
        let changes = diff_detector.detect_changes(&combined_lines);

        if changes.is_empty() {
            return String::new();
        }

        return changes.join("\n");
    }

    // Fallback for non-append cases:

    // Limit text size to prevent excessive memory usage
    let text_limit = if text.len() > MAX_OUTPUT_SIZE {
        let start_offset = text.len() - MAX_OUTPUT_SIZE;

        // Find the start of a line to avoid cutting in the middle
        let adjusted_offset =
            text[start_offset..].find('\n').map_or(start_offset, |pos| start_offset + pos + 1);

        &text[adjusted_offset..]
    } else {
        text
    };

    // Process both old and new output
    let previous_lines = render_terminal_output(last_pending_output);
    let new_lines = render_terminal_output(text_limit);

    // Create a diff detector for efficient comparison
    let mut diff_detector = TerminalOutputDiff::new();
    diff_detector.previous_output = previous_lines;

    // Get the incremental changes
    let changes = diff_detector.detect_changes(&new_lines);

    if changes.is_empty() {
        return String::new();
    }

    changes.join("\n")
}

/// Strip ANSI escape codes from a string using a robust regex
pub fn strip_ansi_codes(input: &str) -> String {
    static RE: std::sync::OnceLock<Option<Regex>> = std::sync::OnceLock::new();

    // Fast path: no ESC byte means nothing to strip.
    if !input.contains('\u{1b}') {
        return input.to_string();
    }

    // Cover the full escape grammar, not just SGR colors. Interactive programs
    // (python/node REPLs, psql, readline) emit far more than colors:
    //   - CSI: ESC [ <params> <intermediates> <final> (cursor moves, `?2004h`
    //          bracketed-paste, `2K` erase, `0m` reset, ...)
    //   - OSC: ESC ] ... (BEL | ST)                    (window-title sets)
    //   - 2-byte ESC sequences (ESC =, ESC >, ESC M, ...)
    // Without OSC/CSI-cursor coverage the scrollback leaked raw bracketed-paste
    // and `]0;user@host` noise into the model's view. Cached so we don't
    // recompile the pattern on every rendered line.
    let re = RE.get_or_init(|| {
        Regex::new(
            r"\x1b\[[0-9;:?<>=!]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[=>MNcD78]",
        )
        .ok()
    });
    let cleaned = match re {
        Some(re) => re.replace_all(input, "").into_owned(),
        None => input.to_string(),
    };
    // Defensive: drop any stray ESC the pattern didn't consume (partial seqs).
    cleaned.replace('\u{1b}', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_applies_cursor_left_overwrite() {
        // 10 Z's, move cursor 5 left, write 5 Q's -> the emulator must OVERWRITE
        // the last 5 Z's (this is what collapses readline echo in scrollback).
        let rendered = render_terminal_output("ZZZZZZZZZZ\u{1b}[5DQQQQQ").join("\n");
        assert!(rendered.contains("ZZZZZQQQQQ"), "cursor-left not applied; got {rendered:?}");
        // and crucially NOT the naive-strip result (escape removed, bytes kept)
        assert!(
            !rendered.contains("ZZZZZZZZZZQQQQQ"),
            "looks like a plain strip; got {rendered:?}"
        );
    }

    #[test]
    fn strips_csi_osc_and_cursor_sequences() {
        // SGR colors
        assert_eq!(strip_ansi_codes("\u{1b}[1;35mhi\u{1b}[0m"), "hi");
        // bracketed-paste + cursor move (the REPL noise we saw leak through)
        assert_eq!(strip_ansi_codes("\u{1b}[?2004h>>> \u{1b}[?2004l\u{1b}[4D42"), ">>> 42");
        // OSC window-title, BEL-terminated
        assert_eq!(strip_ansi_codes("\u{1b}]0;user@host\u{7}prompt$ "), "prompt$ ");
        // plain text is returned untouched (fast path)
        assert_eq!(strip_ansi_codes("no escapes here"), "no escapes here");
    }

    #[test]
    fn test_screen_basic_operations() {
        let mut screen = Screen::new(80);

        // Create default attributes
        let _attributes = ScreenCellAttributes::default();

        // Test putting characters
        screen.put_char_basic('H', CellStyle::default(), None, None);
        screen.put_char_basic('e', CellStyle::default(), None, None);
        screen.put_char_basic('l', CellStyle::default(), None, None);
        screen.put_char_basic('l', CellStyle::default(), None, None);
        screen.put_char_basic('o', CellStyle::default(), None, None);

        let display = screen.display();
        assert_eq!(display, vec!["Hello"]);

        // Test cursor movement
        screen.carriage_return();
        screen.linefeed();

        screen.put_char_basic('W', CellStyle::default(), None, None);
        screen.put_char_basic('o', CellStyle::default(), None, None);
        screen.put_char_basic('r', CellStyle::default(), None, None);
        screen.put_char_basic('l', CellStyle::default(), None, None);
        screen.put_char_basic('d', CellStyle::default(), None, None);

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
        let new = vec!["Line 1".to_string(), "Line 2".to_string(), "Line 3".to_string()];

        let mut diff_detector = TerminalOutputDiff::new();
        diff_detector.previous_output = old;

        let incremental = diff_detector.detect_changes(&new);
        assert_eq!(incremental, vec!["Line 3"]);

        // Test with completely different content
        let old = vec!["Line A".to_string(), "Line B".to_string()];
        let new = vec!["Line X".to_string(), "Line Y".to_string()];

        let mut diff_detector = TerminalOutputDiff::new();
        diff_detector.previous_output = old;

        let incremental = diff_detector.detect_changes(&new);
        assert_eq!(incremental, vec!["Line X", "Line Y"]);
    }

    #[test]
    fn test_render_terminal_output() {
        let text = "Hello\r\nWorld\r\n\x1b[31mRed\x1b[0m Text";
        let lines = render_terminal_output(text);
        assert_eq!(lines, vec!["Hello", "World", "Red Text"]);
    }

    #[test]
    fn test_smart_truncate() {
        let mut screen = Screen::new_with_max_lines(80, 20);

        // Add 30 lines of content
        for i in 0..30 {
            let line = format!("Line {i}");
            for c in line.chars() {
                screen.put_char(c, ScreenCellAttributes::default());
            }
            screen.carriage_return();
            screen.linefeed();
        }

        // Should have truncated to 20 lines
        assert_eq!(screen.lines.len(), 20);

        // Now test smart truncate
        screen.smart_truncate(10);

        // Should now have 10 lines
        assert_eq!(screen.lines.len(), 10);

        // One of the lines should be the truncation marker
        let has_truncation_marker = screen.lines.iter().any(|line| {
            let line_text: String = line.iter().map(|cell| cell.character).collect();
            line_text.contains("TRUNCATED")
        });

        assert!(has_truncation_marker);
    }

    #[test]
    fn test_terminal_cache() {
        let cache = TerminalCache::new(10, 60);

        // Insert a value
        cache.insert("test", vec!["line1".to_string(), "line2".to_string()]);

        // Should be able to retrieve it
        let retrieved = cache.get("test");
        assert_eq!(retrieved, Some(vec!["line1".to_string(), "line2".to_string()]));

        // Unknown key should return None
        let not_found = cache.get("unknown");
        assert_eq!(not_found, None);
    }

    #[test]
    fn test_incremental_text_append() {
        let old_text = "Line 1\nLine 2\n";
        let new_text = "Line 1\nLine 2\nLine 3\n";

        let incremental = incremental_text(new_text, old_text);
        assert_eq!(incremental, "Line 3");
    }

    #[test]
    fn test_terminal_color_handling() {
        let mut terminal = TerminalEmulator::new(80);

        // Test basic colors
        terminal.process("\x1b[31mRed\x1b[32mGreen\x1b[0mNormal");
        let display = terminal.display();
        assert_eq!(display, vec!["RedGreenNormal"]);

        // Test 256 colors
        terminal.clear();
        terminal.process("\x1b[38;5;208mOrange\x1b[0mNormal");
        let display = terminal.display();
        assert_eq!(display, vec!["OrangeNormal"]);
    }
}
