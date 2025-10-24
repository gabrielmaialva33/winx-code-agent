//! ANSI terminal code definitions and handlers
//!
//! This module provides constants and utilities for handling ANSI escape codes
//! commonly used in terminal output, focusing on rich text formatting including
//! 24-bit true color and extended attribute support.

use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;
use std::str::FromStr;

lazy_static! {
    /// Precompiled regex for ANSI escape sequences
    static ref ANSI_REGEX: Regex =
        Regex::new(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])").unwrap();
}

/// Basic ANSI control codes
pub mod control {
    /// Bell
    pub const BEL: &str = "\x07";
    /// Backspace
    pub const BS: &str = "\x08";
    /// Horizontal tab
    pub const HT: &str = "\x09";
    /// Line feed
    pub const LF: &str = "\x0A";
    /// Vertical tab
    pub const VT: &str = "\x0B";
    /// Form feed
    pub const FF: &str = "\x0C";
    /// Carriage return
    pub const CR: &str = "\x0D";
    /// Escape
    pub const ESC: &str = "\x1B";
    /// Delete
    pub const DEL: &str = "\x7F";
}

/// CSI (Control Sequence Introducer) sequences
pub mod csi {
    /// CSI sequence start
    pub const CSI: &str = "\x1B[";

    /// Cursor Up
    pub fn cursor_up(n: usize) -> String {
        format!("\x1B[{}A", n)
    }

    /// Cursor Down
    pub fn cursor_down(n: usize) -> String {
        format!("\x1B[{}B", n)
    }

    /// Cursor Forward
    pub fn cursor_forward(n: usize) -> String {
        format!("\x1B[{}C", n)
    }

    /// Cursor Back
    pub fn cursor_back(n: usize) -> String {
        format!("\x1B[{}D", n)
    }

    /// Cursor Next Line
    pub fn cursor_next_line(n: usize) -> String {
        format!("\x1B[{}E", n)
    }

    /// Cursor Previous Line
    pub fn cursor_prev_line(n: usize) -> String {
        format!("\x1B[{}F", n)
    }

    /// Cursor Horizontal Absolute
    pub fn cursor_horizontal(n: usize) -> String {
        format!("\x1B[{}G", n)
    }

    /// Cursor Position (row, column)
    pub fn cursor_position(row: usize, col: usize) -> String {
        format!("\x1B[{};{}H", row, col)
    }

    /// Erase in Display
    pub fn erase_in_display(n: usize) -> String {
        format!("\x1B[{}J", n)
    }

    /// Erase in Line
    pub fn erase_in_line(n: usize) -> String {
        format!("\x1B[{}K", n)
    }

    /// Scroll Up
    pub fn scroll_up(n: usize) -> String {
        format!("\x1B[{}S", n)
    }

    /// Scroll Down
    pub fn scroll_down(n: usize) -> String {
        format!("\x1B[{}T", n)
    }

    /// Request Cursor Position
    pub const REQUEST_CURSOR_POSITION: &str = "\x1B[6n";

    /// Save Cursor Position
    pub const SAVE_CURSOR_POSITION: &str = "\x1B[s";

    /// Restore Cursor Position
    pub const RESTORE_CURSOR_POSITION: &str = "\x1B[u";

    /// Hide Cursor
    pub const HIDE_CURSOR: &str = "\x1B[?25l";

    /// Show Cursor
    pub const SHOW_CURSOR: &str = "\x1B[?25h";

    /// Enable Alternative Screen Buffer
    pub const ENABLE_ALT_SCREEN: &str = "\x1B[?1049h";

    /// Disable Alternative Screen Buffer
    pub const DISABLE_ALT_SCREEN: &str = "\x1B[?1049l";
}

/// SGR (Select Graphic Rendition) for text styling
pub mod sgr {
    /// Reset all attributes
    pub const RESET: &str = "\x1B[0m";

    /// Bold
    pub const BOLD: &str = "\x1B[1m";

    /// Faint/Dim
    pub const DIM: &str = "\x1B[2m";

    /// Italic
    pub const ITALIC: &str = "\x1B[3m";

    /// Underline
    pub const UNDERLINE: &str = "\x1B[4m";

    /// Slow Blink
    pub const BLINK: &str = "\x1B[5m";

    /// Rapid Blink
    pub const RAPID_BLINK: &str = "\x1B[6m";

    /// Reverse Video
    pub const REVERSE: &str = "\x1B[7m";

    /// Conceal/Hide
    pub const CONCEAL: &str = "\x1B[8m";

    /// Crossed-out/Strike
    pub const STRIKE: &str = "\x1B[9m";

    /// Primary/Default Font
    pub const PRIMARY_FONT: &str = "\x1B[10m";

    /// Alternative Font 1-9
    pub fn alt_font(n: usize) -> String {
        if !(1..=9).contains(&n) {
            return "".to_string();
        }
        format!("\x1B[{}m", 10 + n)
    }

    /// Fraktur (Gothic)
    pub const FRAKTUR: &str = "\x1B[20m";

    /// Double Underline
    pub const DOUBLE_UNDERLINE: &str = "\x1B[21m";

    /// Normal Intensity (not bold and not faint)
    pub const NORMAL_INTENSITY: &str = "\x1B[22m";

    /// Not Italic, Not Fraktur
    pub const NO_ITALIC: &str = "\x1B[23m";

    /// Not Underlined
    pub const NO_UNDERLINE: &str = "\x1B[24m";

    /// Not Blinking
    pub const NO_BLINK: &str = "\x1B[25m";

    /// Proportional Spacing
    pub const PROPORTIONAL_SPACING: &str = "\x1B[26m";

    /// Not Reversed
    pub const NO_REVERSE: &str = "\x1B[27m";

    /// Reveal (Not Concealed)
    pub const REVEAL: &str = "\x1B[28m";

    /// Not Crossed Out
    pub const NO_STRIKE: &str = "\x1B[29m";

    /// Foreground Color (30-37 for basic colors, 90-97 for bright)
    pub fn fg_color(n: usize) -> String {
        format!("\x1B[{}m", n)
    }

    /// Background Color (40-47 for basic colors, 100-107 for bright)
    pub fn bg_color(n: usize) -> String {
        format!("\x1B[{}m", n)
    }

    /// 8-bit Foreground Color (0-255)
    pub fn fg_color_256(n: u8) -> String {
        format!("\x1B[38;5;{}m", n)
    }

    /// 8-bit Background Color (0-255)
    pub fn bg_color_256(n: u8) -> String {
        format!("\x1B[48;5;{}m", n)
    }

    /// 24-bit Foreground Color (RGB)
    pub fn fg_color_rgb(r: u8, g: u8, b: u8) -> String {
        format!("\x1B[38;2;{};{};{}m", r, g, b)
    }

    /// 24-bit Background Color (RGB)
    pub fn bg_color_rgb(r: u8, g: u8, b: u8) -> String {
        format!("\x1B[48;2;{};{};{}m", r, g, b)
    }

    /// Default Foreground Color
    pub const DEFAULT_FG: &str = "\x1B[39m";

    /// Default Background Color
    pub const DEFAULT_BG: &str = "\x1B[49m";

    /// Disable Proportional Spacing
    pub const NO_PROPORTIONAL_SPACING: &str = "\x1B[50m";

    /// Framed
    pub const FRAMED: &str = "\x1B[51m";

    /// Encircled
    pub const ENCIRCLED: &str = "\x1B[52m";

    /// Overlined
    pub const OVERLINED: &str = "\x1B[53m";

    /// Not Framed, Not Encircled
    pub const NO_FRAMED: &str = "\x1B[54m";

    /// Not Overlined
    pub const NO_OVERLINED: &str = "\x1B[55m";

    /// Ideogram Underline
    pub const IDEOGRAM_UNDERLINE: &str = "\x1B[60m";

    /// Ideogram Double Underline
    pub const IDEOGRAM_DOUBLE_UNDERLINE: &str = "\x1B[61m";

    /// Ideogram Overline
    pub const IDEOGRAM_OVERLINE: &str = "\x1B[62m";

    /// Ideogram Double Overline
    pub const IDEOGRAM_DOUBLE_OVERLINE: &str = "\x1B[63m";

    /// Ideogram Stress Marking
    pub const IDEOGRAM_STRESS: &str = "\x1B[64m";

    /// No Ideogram Attributes
    pub const NO_IDEOGRAM: &str = "\x1B[65m";

    /// Superscript
    pub const SUPERSCRIPT: &str = "\x1B[73m";

    /// Subscript
    pub const SUBSCRIPT: &str = "\x1B[74m";

    /// Neither Superscript nor Subscript
    pub const NO_SCRIPT: &str = "\x1B[75m";
}

/// OSC (Operating System Command) sequences
pub mod osc {
    /// Set window title
    pub fn set_title(title: &str) -> String {
        format!("\x1B]0;{}\x07", title)
    }

    /// Set window and icon title
    pub fn set_window_icon_title(title: &str) -> String {
        format!("\x1B]2;{}\x07", title)
    }

    /// Set icon title
    pub fn set_icon_title(title: &str) -> String {
        format!("\x1B]1;{}\x07", title)
    }

    /// Set color definition
    pub fn set_color(num: u8, rgb: &str) -> String {
        format!("\x1B]4;{};{}\x07", num, rgb)
    }

    /// Hyperlink
    pub fn hyperlink(url: &str, text: &str) -> String {
        format!("\x1B]8;;{}\x07{}\x1B]8;;\x07", url, text)
    }
}

/// Mouse reporting modes
pub mod mouse {
    /// Enable normal mouse tracking
    pub const NORMAL_TRACKING: &str = "\x1B[?1000h";

    /// Disable normal mouse tracking
    pub const NO_NORMAL_TRACKING: &str = "\x1B[?1000l";

    /// Enable highlight mouse tracking
    pub const HIGHLIGHT_TRACKING: &str = "\x1B[?1001h";

    /// Disable highlight mouse tracking
    pub const NO_HIGHLIGHT_TRACKING: &str = "\x1B[?1001l";

    /// Enable button-event tracking
    pub const BUTTON_EVENT_TRACKING: &str = "\x1B[?1002h";

    /// Disable button-event tracking
    pub const NO_BUTTON_EVENT_TRACKING: &str = "\x1B[?1002l";

    /// Enable any-event tracking
    pub const ANY_EVENT_TRACKING: &str = "\x1B[?1003h";

    /// Disable any-event tracking
    pub const NO_ANY_EVENT_TRACKING: &str = "\x1B[?1003l";

    /// Enable focus tracking
    pub const FOCUS_TRACKING: &str = "\x1B[?1004h";

    /// Disable focus tracking
    pub const NO_FOCUS_TRACKING: &str = "\x1B[?1004l";

    /// Enable extended mouse coordinates
    pub const EXTENDED_COORDINATES: &str = "\x1B[?1006h";

    /// Disable extended mouse coordinates
    pub const NO_EXTENDED_COORDINATES: &str = "\x1B[?1006l";

    /// Enable SGR mouse coordinates
    pub const SGR_COORDINATES: &str = "\x1B[?1016h";

    /// Disable SGR mouse coordinates
    pub const NO_SGR_COORDINATES: &str = "\x1B[?1016l";
}

/// Mode switching sequences
pub mod modes {
    /// Application Cursor Keys (DECCKM)
    pub const APPLICATION_CURSOR_KEYS: &str = "\x1B[?1h";

    /// Normal Cursor Keys
    pub const NORMAL_CURSOR_KEYS: &str = "\x1B[?1l";

    /// ANSI Mode (vs VT52)
    pub const ANSI_MODE: &str = "\x1B[?2h";

    /// VT52 Mode
    pub const VT52_MODE: &str = "\x1B[?2l";

    /// 132 Column Mode (DECCOLM)
    pub const MODE_132_COLUMN: &str = "\x1B[?3h";

    /// 80 Column Mode
    pub const MODE_80_COLUMN: &str = "\x1B[?3l";

    /// Smooth Scroll (DECSCLM)
    pub const SMOOTH_SCROLL: &str = "\x1B[?4h";

    /// Jump Scroll
    pub const JUMP_SCROLL: &str = "\x1B[?4l";

    /// Reverse Screen (DECSCNM)
    pub const REVERSE_SCREEN: &str = "\x1B[?5h";

    /// Normal Screen
    pub const NORMAL_SCREEN: &str = "\x1B[?5l";

    /// Application Keypad (DECNKM)
    pub const APPLICATION_KEYPAD: &str = "\x1B[?66h";

    /// Numeric Keypad
    pub const NUMERIC_KEYPAD: &str = "\x1B[?66l";

    /// Wraparound Mode (DECAWM)
    pub const WRAPAROUND: &str = "\x1B[?7h";

    /// No Wraparound
    pub const NO_WRAPAROUND: &str = "\x1B[?7l";

    /// Auto-repeat Keys (DECARM)
    pub const AUTOREPEAT_KEYS: &str = "\x1B[?8h";

    /// No Auto-repeat Keys
    pub const NO_AUTOREPEAT_KEYS: &str = "\x1B[?8l";

    /// Send Mouse X & Y on button press
    pub const MOUSE_TRACKING: &str = "\x1B[?9h";

    /// No Mouse Tracking
    pub const NO_MOUSE_TRACKING: &str = "\x1B[?9l";

    /// Show toolbar (rxvt)
    pub const SHOW_TOOLBAR: &str = "\x1B[?10h";

    /// Hide toolbar
    pub const HIDE_TOOLBAR: &str = "\x1B[?10l";

    /// Start Blinking Cursor
    pub const BLINKING_CURSOR: &str = "\x1B[?12h";

    /// Stop Blinking Cursor
    pub const NO_BLINKING_CURSOR: &str = "\x1B[?12l";

    /// Print Form Feed (DECPFF)
    pub const PRINT_FORM_FEED: &str = "\x1B[?18h";

    /// No Form Feed
    pub const NO_PRINT_FORM_FEED: &str = "\x1B[?18l";

    /// Set Print Screen (DECPEX)
    pub const PRINT_SCREEN: &str = "\x1B[?19h";

    /// No Print Screen
    pub const NO_PRINT_SCREEN: &str = "\x1B[?19l";

    /// Enable Linefeed/Newline Mode (LNM)
    pub const NEWLINE_MODE: &str = "\x1B[20h";

    /// Disable Linefeed/Newline Mode
    pub const NO_NEWLINE_MODE: &str = "\x1B[20l";
}

/// Terminal color code definitions
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermColor {
    /// Standard basic color (0-15)
    Basic(u8),

    /// 256-color mode (0-255)
    Color256(u8),

    /// 24-bit RGB color
    TrueColor {
        /// Red component (0-255)
        r: u8,
        /// Green component (0-255)
        g: u8,
        /// Blue component (0-255)
        b: u8,
    },
}

impl TermColor {
    /// Get the ANSI code for foreground color
    pub fn fg_code(&self) -> String {
        match self {
            TermColor::Basic(n) if *n < 8 => format!("\x1B[{}m", 30 + n),
            TermColor::Basic(n) if *n < 16 => format!("\x1B[{}m", 82 + n),
            TermColor::Basic(n) => sgr::fg_color_256(*n),
            TermColor::Color256(n) => sgr::fg_color_256(*n),
            TermColor::TrueColor { r, g, b } => sgr::fg_color_rgb(*r, *g, *b),
        }
    }

    /// Get the ANSI code for background color
    pub fn bg_code(&self) -> String {
        match self {
            TermColor::Basic(n) if *n < 8 => format!("\x1B[{}m", 40 + n),
            TermColor::Basic(n) if *n < 16 => format!("\x1B[{}m", 92 + n),
            TermColor::Basic(n) => sgr::bg_color_256(*n),
            TermColor::Color256(n) => sgr::bg_color_256(*n),
            TermColor::TrueColor { r, g, b } => sgr::bg_color_rgb(*r, *g, *b),
        }
    }
}

/// Parse ANSI escape sequences in text
///
/// This extracts all escape sequences based on their type and position
///
/// # Arguments
///
/// * `text` - The text containing ANSI escape sequences
///
/// # Returns
///
/// A vector of (position, sequence) tuples
pub fn parse_ansi_sequences(text: &str) -> Vec<(usize, String)> {
    ANSI_REGEX
        .find_iter(text)
        .map(|m| (m.start(), m.as_str().to_string()))
        .collect()
}

/// Color name to ANSI code mapping
pub fn color_name_to_code(name: &str) -> Option<TermColor> {
    lazy_static! {
        static ref BASIC_COLORS: HashMap<&'static str, u8> = {
            let mut m = HashMap::new();
            m.insert("black", 0);
            m.insert("red", 1);
            m.insert("green", 2);
            m.insert("yellow", 3);
            m.insert("blue", 4);
            m.insert("magenta", 5);
            m.insert("cyan", 6);
            m.insert("white", 7);
            m.insert("brightblack", 8);
            m.insert("brightred", 9);
            m.insert("brightgreen", 10);
            m.insert("brightyellow", 11);
            m.insert("brightblue", 12);
            m.insert("brightmagenta", 13);
            m.insert("brightcyan", 14);
            m.insert("brightwhite", 15);
            m
        };
    }

    // Try as a basic color name
    if let Some(code) = BASIC_COLORS.get(name.to_lowercase().as_str()) {
        return Some(TermColor::Basic(*code));
    }

    // Try as a color number first (before hex parsing)
    if let Ok(num) = u8::from_str(name) {
        return Some(TermColor::Color256(num));
    }

    // Try as a hex color (only if it starts with # or is clearly hex)
    if (name.starts_with('#') || (name.len() == 6 && name.chars().all(|c| c.is_ascii_hexdigit())))
        && let Some(color) = parse_hex_color(name)
    {
        return Some(color);
    }

    None
}

/// Parse a hex color into TermColor
///
/// Supports formats like #RGB, #RRGGBB
fn parse_hex_color(hex: &str) -> Option<TermColor> {
    let hex = hex.trim_start_matches('#');

    match hex.len() {
        3 => {
            // #RGB format
            let r = u8::from_str_radix(&hex[0..1], 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()?;

            // Convert from 0-15 to 0-255 range
            let r = r * 17;
            let g = g * 17;
            let b = b * 17;

            Some(TermColor::TrueColor { r, g, b })
        }
        6 => {
            // #RRGGBB format
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;

            Some(TermColor::TrueColor { r, g, b })
        }
        _ => None,
    }
}

/// Format text with ANSI styling
///
/// This is a convenience function to easily add common ANSI styles
///
/// # Arguments
///
/// * `text` - The text to format
/// * `bold` - Whether to make the text bold
/// * `italic` - Whether to make the text italic
/// * `underline` - Whether to underline the text
/// * `fg_color` - Optional foreground color
/// * `bg_color` - Optional background color
///
/// # Returns
///
/// The formatted text with ANSI codes
pub fn format_ansi_text(
    text: &str,
    bold: bool,
    italic: bool,
    underline: bool,
    fg_color: Option<&TermColor>,
    bg_color: Option<&TermColor>,
) -> String {
    let mut result = String::new();

    // Add style codes
    if bold {
        result.push_str(sgr::BOLD);
    }
    if italic {
        result.push_str(sgr::ITALIC);
    }
    if underline {
        result.push_str(sgr::UNDERLINE);
    }

    // Add color codes
    if let Some(color) = fg_color {
        result.push_str(&color.fg_code());
    }
    if let Some(color) = bg_color {
        result.push_str(&color.bg_code());
    }

    // Add text and reset
    result.push_str(text);
    result.push_str(sgr::RESET);

    result
}

/// Strip all ANSI escape sequences from text
///
/// # Arguments
///
/// * `text` - The text containing ANSI escape sequences
///
/// # Returns
///
/// The text with all ANSI sequences removed
pub fn strip_ansi_codes(text: &str) -> String {
    ANSI_REGEX.replace_all(text, "").to_string()
}

/// Extract structured style information from ANSI-formatted text
///
/// # Arguments
///
/// * `text` - The text with ANSI escape sequences
///
/// # Returns
///
/// A vector of (position, styles) where styles is a map of active styles
pub fn extract_ansi_styles(text: &str) -> Vec<(usize, HashMap<String, String>)> {
    let mut result = Vec::new();
    let mut current_styles = HashMap::new();
    let mut pos = 0;

    for (idx, seq) in parse_ansi_sequences(text) {
        // Adjust position for any actual text content
        if idx > pos {
            result.push((pos, current_styles.clone()));
            // No need to update pos here as it's updated below
        }

        // Process the sequence and update styles
        if seq == sgr::RESET {
            current_styles.clear();
        } else if seq == sgr::BOLD {
            current_styles.insert("weight".to_string(), "bold".to_string());
        } else if seq == sgr::ITALIC {
            current_styles.insert("style".to_string(), "italic".to_string());
        } else if seq == sgr::UNDERLINE {
            current_styles.insert("text-decoration".to_string(), "underline".to_string());
        } else if seq.starts_with("\x1B[38;") {
            current_styles.insert("color".to_string(), extract_color_from_seq(&seq));
        } else if seq.starts_with("\x1B[48;") {
            current_styles.insert("background-color".to_string(), extract_color_from_seq(&seq));
        }

        // Move position past the sequence
        pos = idx + seq.len();
    }

    // Add final segment if needed
    if pos < text.len() {
        result.push((pos, current_styles));
    }

    result
}

/// Extract color information from an SGR color sequence
fn extract_color_from_seq(seq: &str) -> String {
    if seq.starts_with("\x1B[38;5;") || seq.starts_with("\x1B[48;5;") {
        // 8-bit color
        let parts: Vec<&str> = seq.split(';').collect();
        if parts.len() >= 3
            && let Some(color_part) = parts[2].strip_suffix('m')
        {
            return format!("color-{}", color_part);
        }
    } else if seq.starts_with("\x1B[38;2;") || seq.starts_with("\x1B[48;2;") {
        // 24-bit color
        let parts: Vec<&str> = seq.split(';').collect();
        if parts.len() >= 5 {
            let r = parts[2];
            let g = parts[3];
            let b = if let Some(stripped) = parts[4].strip_suffix('m') {
                stripped
            } else {
                parts[4]
            };
            return format!("rgb({},{},{})", r, g, b);
        }
    }

    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_term_color() {
        let red = TermColor::Basic(1);
        assert_eq!(red.fg_code(), "\x1B[31m");
        assert_eq!(red.bg_code(), "\x1B[41m");

        let color256 = TermColor::Color256(128);
        assert_eq!(color256.fg_code(), "\x1B[38;5;128m");
        assert_eq!(color256.bg_code(), "\x1B[48;5;128m");

        let true_color = TermColor::TrueColor {
            r: 255,
            g: 128,
            b: 64,
        };
        assert_eq!(true_color.fg_code(), "\x1B[38;2;255;128;64m");
        assert_eq!(true_color.bg_code(), "\x1B[48;2;255;128;64m");
    }

    #[test]
    fn test_color_name_to_code() {
        assert_eq!(color_name_to_code("red"), Some(TermColor::Basic(1)));
        assert_eq!(color_name_to_code("brightblue"), Some(TermColor::Basic(12)));
        assert_eq!(color_name_to_code("123"), Some(TermColor::Color256(123)));

        if let Some(TermColor::TrueColor { r, g, b }) = color_name_to_code("#ff00ff") {
            assert_eq!((r, g, b), (255, 0, 255));
        } else {
            panic!("Failed to parse hex color");
        }

        if let Some(TermColor::TrueColor { r, g, b }) = color_name_to_code("#f0f") {
            assert_eq!((r, g, b), (255, 0, 255));
        } else {
            panic!("Failed to parse short hex color");
        }
    }

    #[test]
    fn test_format_ansi_text() {
        let text = format_ansi_text("Hello", true, false, true, Some(&TermColor::Basic(1)), None);
        assert_eq!(text, "\x1B[1m\x1B[4m\x1B[31mHello\x1B[0m");
    }

    #[test]
    fn test_strip_ansi_codes() {
        let input = "\x1B[1m\x1B[31mHello\x1B[0m \x1B[32mWorld\x1B[0m";
        let output = strip_ansi_codes(input);
        assert_eq!(output, "Hello World");
    }

    #[test]
    fn test_parse_ansi_sequences() {
        let input = "Normal \x1B[1mBold\x1B[0m Normal";
        let sequences = parse_ansi_sequences(input);
        assert_eq!(sequences.len(), 2);
        assert_eq!(sequences[0], (7, "\x1B[1m".to_string()));
        assert_eq!(sequences[1], (15, "\x1B[0m".to_string()));
    }
}
