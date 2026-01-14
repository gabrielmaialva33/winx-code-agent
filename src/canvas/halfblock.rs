//! `HalfBlock` rasterizer - Universal fallback using ▀▄█ characters
//!
//! Each terminal cell represents 2 vertical "pixels":
//! - Top pixel = foreground color
//! - Bottom pixel = background color
//!
//! Characters used:
//! - ▀ (upper half block): top = fg, bottom = bg
//! - ▄ (lower half block): top = bg, bottom = fg
//! - █ (full block): both = fg
//! - ' ' (space): both = bg

use super::canvas::Canvas;
use super::caps::TerminalCaps;
use super::color::Color;
use super::rasterizer::{RasterOutput, Rasterizer, StyledCell, StyledLine};

/// Half-block rasterizer
///
/// Renders 2 vertical pixels per cell using ▀▄█ characters.
/// Universal support - works on any Unicode terminal.
#[derive(Debug, Default)]
pub struct HalfBlockRasterizer;

impl HalfBlockRasterizer {
    pub fn new() -> Self {
        Self
    }

    /// Convert canvas to half-block grid
    fn rasterize_to_cells(&self, canvas: &Canvas, cols: u16, rows: u16) -> Vec<Vec<StyledCell>> {
        let mut result = Vec::with_capacity(rows as usize);

        // Scale factors
        let scale_x = f64::from(canvas.width) / f64::from(cols);
        let scale_y = f64::from(canvas.height) / (f64::from(rows) * 2.0); // 2 subpixels per row

        for row in 0..rows {
            let mut line = Vec::with_capacity(cols as usize);

            for col in 0..cols {
                // Sample top and bottom pixels
                let top_x = (f64::from(col) * scale_x) as u32;
                let top_y = (f64::from(row) * 2.0 * scale_y) as u32;
                let bot_y = ((f64::from(row) * 2.0 + 1.0) * scale_y) as u32;

                let top_color = canvas
                    .get_pixel(top_x.min(canvas.width - 1), top_y.min(canvas.height - 1))
                    .unwrap_or(Color::BLACK);
                let bot_color = canvas
                    .get_pixel(top_x.min(canvas.width - 1), bot_y.min(canvas.height - 1))
                    .unwrap_or(Color::BLACK);

                // Choose character based on colors
                let (ch, fg, bg) = self.select_halfblock(&top_color, &bot_color);

                line.push(StyledCell { ch, fg, bg });
            }

            result.push(line);
        }

        result
    }

    /// Select the best half-block character and colors for a top/bottom pair
    fn select_halfblock(
        &self,
        top: &Color,
        bot: &Color,
    ) -> (char, ratatui::style::Color, ratatui::style::Color) {
        let top_lum = top.luminance();
        let bot_lum = bot.luminance();

        // If colors are very similar, use full block or space
        if top.distance(bot) < 0.1 {
            // Use the average color
            let avg = Color::rgb(
                (top.r + bot.r) / 2.0,
                (top.g + bot.g) / 2.0,
                (top.b + bot.b) / 2.0,
            );

            if avg.luminance() < 0.1 {
                // Very dark - use space with black bg
                return (' ', ratatui::style::Color::Black, avg.to_ratatui());
            }
            // Use full block
            return ('█', avg.to_ratatui(), ratatui::style::Color::Black);
        }

        // Different colors - use half blocks
        // Choose which way gives better color accuracy
        if top_lum >= bot_lum {
            // Top is lighter - use ▀ with top as fg, bottom as bg
            ('▀', top.to_ratatui(), bot.to_ratatui())
        } else {
            // Bottom is lighter - use ▄ with bottom as fg, top as bg
            ('▄', bot.to_ratatui(), top.to_ratatui())
        }
    }
}

impl Rasterizer for HalfBlockRasterizer {
    fn rasterize(&self, canvas: &Canvas, caps: &TerminalCaps) -> RasterOutput {
        let cells = self.rasterize_to_cells(canvas, caps.cols, caps.rows);
        let lines = cells
            .into_iter()
            .map(|row| StyledLine { cells: row })
            .collect();
        RasterOutput::Lines(lines)
    }

    fn resolution_multiplier(&self) -> (u32, u32) {
        (1, 2) // 1x horizontal, 2x vertical
    }

    fn name(&self) -> &'static str {
        "HalfBlock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_halfblock_similar_colors() {
        let rasterizer = HalfBlockRasterizer::new();
        let top = Color::rgb(0.5, 0.5, 0.5);
        let bot = Color::rgb(0.51, 0.51, 0.51);
        let (ch, _, _) = rasterizer.select_halfblock(&top, &bot);
        // Similar colors should use full block or space
        assert!(ch == '█' || ch == ' ');
    }

    #[test]
    fn test_halfblock_different_colors() {
        let rasterizer = HalfBlockRasterizer::new();
        let top = Color::WHITE;
        let bot = Color::BLACK;
        let (ch, _, _) = rasterizer.select_halfblock(&top, &bot);
        // Different colors should use half block
        assert!(ch == '▀' || ch == '▄');
    }
}
