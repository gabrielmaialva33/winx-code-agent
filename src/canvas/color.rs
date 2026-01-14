//! Color types with alpha support

/// RGBA color with f32 components (0.0 - 1.0)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Self = Self::rgba(0.0, 0.0, 0.0, 0.0);
    pub const BLACK: Self = Self::rgb(0.0, 0.0, 0.0);
    pub const WHITE: Self = Self::rgb(1.0, 1.0, 1.0);
    pub const RED: Self = Self::rgb(1.0, 0.0, 0.0);
    pub const GREEN: Self = Self::rgb(0.0, 1.0, 0.0);
    pub const BLUE: Self = Self::rgb(0.0, 0.0, 1.0);
    pub const YELLOW: Self = Self::rgb(1.0, 1.0, 0.0);
    pub const CYAN: Self = Self::rgb(0.0, 1.0, 1.0);
    pub const MAGENTA: Self = Self::rgb(1.0, 0.0, 1.0);

    #[inline]
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    #[inline]
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// Create from 8-bit RGB values (0-255)
    #[inline]
    pub fn from_rgb8(r: u8, g: u8, b: u8) -> Self {
        Self::rgb(f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0)
    }

    /// Create from 8-bit RGBA values (0-255)
    #[inline]
    pub fn from_rgba8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self::rgba(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            f32::from(a) / 255.0,
        )
    }

    /// Create from hex color (e.g., 0xFF0000 for red)
    #[inline]
    pub fn from_hex(hex: u32) -> Self {
        Self::from_rgb8(
            ((hex >> 16) & 0xFF) as u8,
            ((hex >> 8) & 0xFF) as u8,
            (hex & 0xFF) as u8,
        )
    }

    /// Convert to 8-bit RGB tuple
    #[inline]
    pub fn to_rgb8(&self) -> (u8, u8, u8) {
        (
            (self.r.clamp(0.0, 1.0) * 255.0) as u8,
            (self.g.clamp(0.0, 1.0) * 255.0) as u8,
            (self.b.clamp(0.0, 1.0) * 255.0) as u8,
        )
    }

    /// Convert to ratatui Color
    #[inline]
    pub fn to_ratatui(&self) -> ratatui::style::Color {
        let (r, g, b) = self.to_rgb8();
        ratatui::style::Color::Rgb(r, g, b)
    }

    /// Blend this color over another (alpha compositing)
    #[inline]
    pub fn blend_over(&self, bg: &Color) -> Color {
        let a = self.a + bg.a * (1.0 - self.a);
        if a < 0.0001 {
            return Color::TRANSPARENT;
        }
        Color {
            r: (self.r * self.a + bg.r * bg.a * (1.0 - self.a)) / a,
            g: (self.g * self.a + bg.g * bg.a * (1.0 - self.a)) / a,
            b: (self.b * self.a + bg.b * bg.a * (1.0 - self.a)) / a,
            a,
        }
    }

    /// Compute luminance (perceived brightness)
    #[inline]
    pub fn luminance(&self) -> f32 {
        0.299 * self.r + 0.587 * self.g + 0.114 * self.b
    }

    /// Euclidean distance to another color (for clustering)
    #[inline]
    pub fn distance(&self, other: &Color) -> f32 {
        let dr = self.r - other.r;
        let dg = self.g - other.g;
        let db = self.b - other.b;
        (dr * dr + dg * dg + db * db).sqrt()
    }
}

impl Default for Color {
    fn default() -> Self {
        Self::BLACK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_hex() {
        let red = Color::from_hex(0xFF0000);
        assert!((red.r - 1.0).abs() < 0.01);
        assert!(red.g.abs() < 0.01);
        assert!(red.b.abs() < 0.01);
    }

    #[test]
    fn test_blend() {
        let fg = Color::rgba(1.0, 0.0, 0.0, 0.5); // 50% red
        let bg = Color::rgb(0.0, 0.0, 1.0); // solid blue
        let blended = fg.blend_over(&bg);

        // Should be purplish
        assert!(blended.r > 0.4);
        assert!(blended.b > 0.4);
    }
}
