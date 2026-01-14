//! Rasterizer trait for converting canvas to terminal output

use super::canvas::Canvas;
use super::caps::TerminalCaps;

/// Output from rasterizer
#[derive(Debug, Clone)]
pub enum RasterOutput {
    /// Text cells with styles (for ratatui)
    Cells(Vec<StyledCell>),
    /// Raw escape sequence (for Kitty/Sixel)
    Escape(String),
    /// Lines of styled text (for simple output)
    Lines(Vec<StyledLine>),
}

/// A single terminal cell with foreground and background colors
#[derive(Debug, Clone, Copy)]
pub struct StyledCell {
    /// Character to display
    pub ch: char,
    /// Foreground color
    pub fg: ratatui::style::Color,
    /// Background color
    pub bg: ratatui::style::Color,
}

impl Default for StyledCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: ratatui::style::Color::White,
            bg: ratatui::style::Color::Black,
        }
    }
}

/// A line of styled text
#[derive(Debug, Clone)]
pub struct StyledLine {
    pub cells: Vec<StyledCell>,
}

/// Trait for rasterizers that convert canvas to terminal output
pub trait Rasterizer: Send + Sync {
    /// Convert canvas to terminal output
    fn rasterize(&self, canvas: &Canvas, caps: &TerminalCaps) -> RasterOutput;

    /// Effective resolution in "subpixels" for this rasterizer
    fn resolution_multiplier(&self) -> (u32, u32);

    /// Name of this rasterizer
    fn name(&self) -> &'static str;
}

/// Select the best rasterizer for given terminal capabilities
pub fn select_rasterizer(
    caps: &TerminalCaps,
) -> Box<dyn Rasterizer> {
    use super::caps::GraphicsProtocol;
    use super::halfblock::HalfBlockRasterizer;
    use super::kitty::KittyRasterizer;

    match caps.graphics {
        GraphicsProtocol::Kitty => Box::new(KittyRasterizer::new()),
        GraphicsProtocol::ITerm2 => Box::new(KittyRasterizer::new()), // Similar protocol
        GraphicsProtocol::Sixel => Box::new(HalfBlockRasterizer::new()), // TODO: SixelRasterizer
        GraphicsProtocol::None => Box::new(HalfBlockRasterizer::new()),
    }
}
