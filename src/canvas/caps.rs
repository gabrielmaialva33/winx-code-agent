//! Terminal capability detection
//!
//! Auto-detects:
//! - Graphics protocol support (Kitty, Sixel, iTerm2)
//! - Unicode level (Sextant, Braille, HalfBlock)
//! - Color depth (TrueColor, 256, 16)
//! - Cell dimensions

use std::env;

/// Graphics protocol supported by terminal
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraphicsProtocol {
    /// Kitty Graphics Protocol - best quality, binary PNG
    Kitty,
    /// Sixel graphics - DEC standard, widely supported
    Sixel,
    /// iTerm2 inline images
    ITerm2,
    /// No graphics support - use Unicode fallback
    #[default]
    None,
}

/// Unicode graphics level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnicodeLevel {
    /// Sextant characters (Unicode 13+) - 2x3 per cell = 6 subpixels
    Sextant,
    /// Braille patterns - 2x4 per cell = 8 subpixels (but 1-bit)
    Braille,
    /// Half-block characters ▀▄ - 1x2 per cell with 2 colors
    #[default]
    HalfBlock,
    /// Quarter-block ▖▗▘▙ - 2x2 per cell with 2 colors
    QuarterBlock,
    /// ASCII only - no Unicode graphics
    Ascii,
}

/// Color depth
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorDepth {
    /// 24-bit RGB (16 million colors)
    #[default]
    TrueColor,
    /// 256 color palette
    Color256,
    /// 16 ANSI colors
    Color16,
    /// Monochrome
    Mono,
}

/// Terminal capabilities
#[derive(Debug, Clone)]
pub struct TerminalCaps {
    /// Terminal columns
    pub cols: u16,
    /// Terminal rows
    pub rows: u16,
    /// Pixels per cell (width)
    pub cell_width: u8,
    /// Pixels per cell (height)
    pub cell_height: u8,
    /// Graphics protocol support
    pub graphics: GraphicsProtocol,
    /// Unicode graphics level
    pub unicode: UnicodeLevel,
    /// Color depth
    pub colors: ColorDepth,
    /// Terminal name/type
    pub term_name: String,
}

impl Default for TerminalCaps {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
            graphics: GraphicsProtocol::None,
            unicode: UnicodeLevel::HalfBlock,
            colors: ColorDepth::TrueColor,
            term_name: String::new(),
        }
    }
}

impl TerminalCaps {
    /// Detect terminal capabilities
    pub fn detect() -> Self {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

        let mut caps = Self {
            cols,
            rows,
            cell_width: 8,  // Default, would need escape sequence query for actual
            cell_height: 16,
            graphics: detect_graphics_protocol(),
            unicode: detect_unicode_level(),
            colors: detect_color_depth(),
            term_name: env::var("TERM").unwrap_or_default(),
        };

        // Estimate cell size from common terminals
        caps.estimate_cell_size();

        caps
    }

    /// Effective resolution in pixels (for graphics protocols)
    pub fn pixel_resolution(&self) -> (u32, u32) {
        (
            self.cols as u32 * self.cell_width as u32,
            self.rows as u32 * self.cell_height as u32,
        )
    }

    /// Effective resolution in "subpixels" for Unicode graphics
    pub fn subpixel_resolution(&self) -> (u32, u32) {
        match self.unicode {
            UnicodeLevel::Sextant => (self.cols as u32 * 2, self.rows as u32 * 3),
            UnicodeLevel::Braille => (self.cols as u32 * 2, self.rows as u32 * 4),
            UnicodeLevel::HalfBlock => (self.cols as u32, self.rows as u32 * 2),
            UnicodeLevel::QuarterBlock => (self.cols as u32 * 2, self.rows as u32 * 2),
            UnicodeLevel::Ascii => (self.cols as u32, self.rows as u32),
        }
    }

    fn estimate_cell_size(&mut self) {
        // Common terminal cell sizes
        match self.term_name.as_str() {
            t if t.contains("kitty") => {
                self.cell_width = 10;
                self.cell_height = 20;
            }
            t if t.contains("alacritty") => {
                self.cell_width = 9;
                self.cell_height = 18;
            }
            t if t.contains("wezterm") => {
                self.cell_width = 9;
                self.cell_height = 18;
            }
            _ => {
                // Default monospace assumption
                self.cell_width = 8;
                self.cell_height = 16;
            }
        }
    }
}

/// Detect graphics protocol support
fn detect_graphics_protocol() -> GraphicsProtocol {
    // Check Kitty
    if env::var("KITTY_WINDOW_ID").is_ok() {
        return GraphicsProtocol::Kitty;
    }

    // Check iTerm2
    if env::var("ITERM_SESSION_ID").is_ok() {
        return GraphicsProtocol::ITerm2;
    }

    // Check WezTerm (supports Kitty protocol)
    if env::var("WEZTERM_PANE").is_ok() {
        return GraphicsProtocol::Kitty;
    }

    // Check TERM for hints
    if let Ok(term) = env::var("TERM") {
        if term.contains("kitty") {
            return GraphicsProtocol::Kitty;
        }
        // foot, xterm-256color with sixel, etc
        if term.contains("foot") || term.contains("xterm") {
            // Would need actual query for Sixel support
            // For now, assume HalfBlock fallback
        }
    }

    // Check TERM_PROGRAM
    if let Ok(prog) = env::var("TERM_PROGRAM") {
        match prog.as_str() {
            "iTerm.app" => return GraphicsProtocol::ITerm2,
            "WezTerm" => return GraphicsProtocol::Kitty,
            "Ghostty" => return GraphicsProtocol::Kitty,
            _ => {}
        }
    }

    GraphicsProtocol::None
}

/// Detect Unicode graphics level
fn detect_unicode_level() -> UnicodeLevel {
    // Check locale for UTF-8
    let locale = env::var("LANG").unwrap_or_default();
    let lc_all = env::var("LC_ALL").unwrap_or_default();

    if !locale.to_uppercase().contains("UTF") && !lc_all.to_uppercase().contains("UTF") {
        return UnicodeLevel::Ascii;
    }

    // Most modern terminals support HalfBlock well
    // Sextant (Unicode 13+) is newer and less supported
    // Default to HalfBlock as safe choice
    UnicodeLevel::HalfBlock
}

/// Detect color depth
fn detect_color_depth() -> ColorDepth {
    // Check COLORTERM
    if let Ok(ct) = env::var("COLORTERM") {
        if ct == "truecolor" || ct == "24bit" {
            return ColorDepth::TrueColor;
        }
    }

    // Check TERM
    if let Ok(term) = env::var("TERM") {
        if term.contains("256color") || term.contains("24bit") || term.contains("truecolor") {
            return ColorDepth::TrueColor;
        }
        if term.contains("color") {
            return ColorDepth::Color256;
        }
    }

    // Most modern terminals support truecolor
    ColorDepth::TrueColor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_caps() {
        let caps = TerminalCaps::default();
        assert_eq!(caps.cols, 80);
        assert_eq!(caps.rows, 24);
    }

    #[test]
    fn test_subpixel_resolution() {
        let mut caps = TerminalCaps::default();
        caps.cols = 80;
        caps.rows = 24;

        caps.unicode = UnicodeLevel::HalfBlock;
        assert_eq!(caps.subpixel_resolution(), (80, 48));

        caps.unicode = UnicodeLevel::Braille;
        assert_eq!(caps.subpixel_resolution(), (160, 96));
    }
}
