//! Virtual canvas with float coordinates and RGBA pixels

use super::color::Color;
use super::shapes::Shape;

/// Virtual framebuffer canvas
///
/// Coordinates are continuous (f64), not discrete.
/// Pixels are RGBA with alpha compositing.
#[derive(Debug, Clone)]
pub struct Canvas {
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
    /// Pixel data (row-major, RGBA)
    pixels: Vec<Color>,
    /// Background color
    bg_color: Color,
    /// Dirty flag for incremental rendering
    dirty: bool,
}

impl Canvas {
    /// Create a new canvas with given dimensions
    pub fn new(width: u32, height: u32) -> Self {
        let size = (width * height) as usize;
        Self {
            width,
            height,
            pixels: vec![Color::TRANSPARENT; size],
            bg_color: Color::BLACK,
            dirty: true,
        }
    }

    /// Create canvas with background color
    pub fn with_background(width: u32, height: u32, bg: Color) -> Self {
        let size = (width * height) as usize;
        Self {
            width,
            height,
            pixels: vec![bg; size],
            bg_color: bg,
            dirty: true,
        }
    }

    /// Clear canvas to background color
    pub fn clear(&mut self) {
        self.pixels.fill(self.bg_color);
        self.dirty = true;
    }

    /// Set background color
    pub fn set_background(&mut self, color: Color) {
        self.bg_color = color;
    }

    /// Get pixel at coordinates
    #[inline]
    pub fn get_pixel(&self, x: u32, y: u32) -> Option<Color> {
        if x < self.width && y < self.height {
            Some(self.pixels[(y * self.width + x) as usize])
        } else {
            None
        }
    }

    /// Set pixel at coordinates
    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, color: Color) {
        if x < self.width && y < self.height {
            self.pixels[(y * self.width + x) as usize] = color;
            self.dirty = true;
        }
    }

    /// Set pixel with alpha blending
    #[inline]
    pub fn blend_pixel(&mut self, x: u32, y: u32, color: Color) {
        if x < self.width && y < self.height {
            let idx = (y * self.width + x) as usize;
            self.pixels[idx] = color.blend_over(&self.pixels[idx]);
            self.dirty = true;
        }
    }

    /// Draw a shape onto the canvas
    pub fn draw<S: Shape>(&mut self, shape: &S) {
        shape.rasterize(self.width, self.height, &mut self.pixels);
        self.dirty = true;
    }

    /// Get raw pixel data
    pub fn pixels(&self) -> &[Color] {
        &self.pixels
    }

    /// Get mutable pixel data
    pub fn pixels_mut(&mut self) -> &mut [Color] {
        self.dirty = true;
        &mut self.pixels
    }

    /// Check if canvas is dirty
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark canvas as clean
    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    /// Sample pixel at float coordinates (bilinear interpolation)
    pub fn sample(&self, x: f64, y: f64) -> Color {
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let x1 = x0 + 1;
        let y1 = y0 + 1;

        let fx = (x - x0 as f64) as f32;
        let fy = (y - y0 as f64) as f32;

        let get = |px: i32, py: i32| -> Color {
            if px >= 0 && px < self.width as i32 && py >= 0 && py < self.height as i32 {
                self.pixels[(py as u32 * self.width + px as u32) as usize]
            } else {
                self.bg_color
            }
        };

        let c00 = get(x0, y0);
        let c10 = get(x1, y0);
        let c01 = get(x0, y1);
        let c11 = get(x1, y1);

        // Bilinear interpolation
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;

        Color {
            r: lerp(lerp(c00.r, c10.r, fx), lerp(c01.r, c11.r, fx), fy),
            g: lerp(lerp(c00.g, c10.g, fx), lerp(c01.g, c11.g, fx), fy),
            b: lerp(lerp(c00.b, c10.b, fx), lerp(c01.b, c11.b, fx), fy),
            a: lerp(lerp(c00.a, c10.a, fx), lerp(c01.a, c11.a, fx), fy),
        }
    }

    /// Resize canvas (resamples content)
    pub fn resize(&self, new_width: u32, new_height: u32) -> Self {
        let mut new_canvas = Canvas::with_background(new_width, new_height, self.bg_color);

        let scale_x = self.width as f64 / new_width as f64;
        let scale_y = self.height as f64 / new_height as f64;

        for y in 0..new_height {
            for x in 0..new_width {
                let src_x = x as f64 * scale_x;
                let src_y = y as f64 * scale_y;
                new_canvas.set_pixel(x, y, self.sample(src_x, src_y));
            }
        }

        new_canvas
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_canvas() {
        let canvas = Canvas::new(100, 50);
        assert_eq!(canvas.width, 100);
        assert_eq!(canvas.height, 50);
        assert_eq!(canvas.pixels.len(), 5000);
    }

    #[test]
    fn test_set_get_pixel() {
        let mut canvas = Canvas::new(10, 10);
        canvas.set_pixel(5, 5, Color::RED);
        assert_eq!(canvas.get_pixel(5, 5), Some(Color::RED));
    }

    #[test]
    fn test_blend_pixel() {
        let mut canvas = Canvas::with_background(10, 10, Color::BLUE);
        canvas.blend_pixel(5, 5, Color::rgba(1.0, 0.0, 0.0, 0.5));
        let blended = canvas.get_pixel(5, 5).unwrap();
        // Should be purplish (red over blue)
        assert!(blended.r > 0.4);
        assert!(blended.b > 0.4);
    }
}
