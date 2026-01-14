//! Geometric shapes for canvas drawing

use super::Color;

/// 2D point with f64 coordinates (subpixel precision)
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    #[inline]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    #[inline]
    pub fn distance(&self, other: &Point) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

impl std::ops::Add for Point {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl std::ops::Sub for Point {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y)
    }
}

/// Axis-aligned rectangle
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    #[inline]
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    #[inline]
    pub fn left(&self) -> f64 {
        self.x
    }
    #[inline]
    pub fn right(&self) -> f64 {
        self.x + self.width
    }
    #[inline]
    pub fn top(&self) -> f64 {
        self.y
    }
    #[inline]
    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }

    #[inline]
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.left() && p.x <= self.right() && p.y >= self.top() && p.y <= self.bottom()
    }
}

/// Shape trait for drawable primitives
pub trait Shape {
    /// Draw the shape onto a pixel buffer
    fn rasterize(&self, width: u32, height: u32, pixels: &mut [Color]);

    /// Bounding box of the shape
    fn bounds(&self) -> Rect;
}

/// Line segment
#[derive(Debug, Clone, Copy)]
pub struct Line {
    pub start: Point,
    pub end: Point,
    pub color: Color,
    pub width: f64,
}

impl Line {
    pub fn new(start: Point, end: Point, color: Color) -> Self {
        Self {
            start,
            end,
            color,
            width: 1.0,
        }
    }

    pub fn with_width(mut self, width: f64) -> Self {
        self.width = width;
        self
    }
}

impl Shape for Line {
    fn rasterize(&self, width: u32, height: u32, pixels: &mut [Color]) {
        // Xiaolin Wu's line algorithm for anti-aliased lines
        let mut x0 = self.start.x;
        let mut y0 = self.start.y;
        let mut x1 = self.end.x;
        let mut y1 = self.end.y;

        let steep = (y1 - y0).abs() > (x1 - x0).abs();

        if steep {
            std::mem::swap(&mut x0, &mut y0);
            std::mem::swap(&mut x1, &mut y1);
        }
        if x0 > x1 {
            std::mem::swap(&mut x0, &mut x1);
            std::mem::swap(&mut y0, &mut y1);
        }

        let dx = x1 - x0;
        let dy = y1 - y0;
        let gradient = if dx.abs() < 0.0001 { 1.0 } else { dy / dx };

        // Helper to plot a pixel with intensity
        let plot = |pixels: &mut [Color], x: i32, y: i32, intensity: f32| {
            if x >= 0 && x < width as i32 && y >= 0 && y < height as i32 {
                let idx = (y as u32 * width + x as u32) as usize;
                if idx < pixels.len() {
                    let mut c = self.color;
                    c.a *= intensity;
                    pixels[idx] = c.blend_over(&pixels[idx]);
                }
            }
        };

        // Main loop
        let mut y = y0;
        for x in (x0.floor() as i32)..=(x1.ceil() as i32) {
            let intensity = 1.0 - (y - y.floor()) as f32;
            if steep {
                plot(pixels, y.floor() as i32, x, intensity);
                plot(pixels, y.floor() as i32 + 1, x, 1.0 - intensity);
            } else {
                plot(pixels, x, y.floor() as i32, intensity);
                plot(pixels, x, y.floor() as i32 + 1, 1.0 - intensity);
            }
            y += gradient;
        }
    }

    fn bounds(&self) -> Rect {
        let min_x = self.start.x.min(self.end.x);
        let min_y = self.start.y.min(self.end.y);
        let max_x = self.start.x.max(self.end.x);
        let max_y = self.start.y.max(self.end.y);
        Rect::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }
}

/// Circle
#[derive(Debug, Clone, Copy)]
pub struct Circle {
    pub center: Point,
    pub radius: f64,
    pub color: Color,
    pub filled: bool,
}

impl Circle {
    pub fn new(center: Point, radius: f64, color: Color) -> Self {
        Self {
            center,
            radius,
            color,
            filled: false,
        }
    }

    pub fn filled(mut self) -> Self {
        self.filled = true;
        self
    }
}

impl Shape for Circle {
    fn rasterize(&self, width: u32, height: u32, pixels: &mut [Color]) {
        let cx = self.center.x;
        let cy = self.center.y;
        let r = self.radius;

        // Bounding box
        let min_x = ((cx - r - 1.0).max(0.0)) as u32;
        let max_x = ((cx + r + 1.0).min(width as f64 - 1.0)) as u32;
        let min_y = ((cy - r - 1.0).max(0.0)) as u32;
        let max_y = ((cy + r + 1.0).min(height as f64 - 1.0)) as u32;

        for py in min_y..=max_y {
            for px in min_x..=max_x {
                let dist = ((px as f64 - cx).powi(2) + (py as f64 - cy).powi(2)).sqrt();

                let intensity = if self.filled {
                    if dist <= r - 0.5 {
                        1.0
                    } else if dist <= r + 0.5 {
                        (r + 0.5 - dist) as f32
                    } else {
                        0.0
                    }
                } else {
                    // Outline only
                    let edge_dist = (dist - r).abs();
                    if edge_dist <= 1.0 {
                        (1.0 - edge_dist) as f32
                    } else {
                        0.0
                    }
                };

                if intensity > 0.0 {
                    let idx = (py * width + px) as usize;
                    if idx < pixels.len() {
                        let mut c = self.color;
                        c.a *= intensity;
                        pixels[idx] = c.blend_over(&pixels[idx]);
                    }
                }
            }
        }
    }

    fn bounds(&self) -> Rect {
        Rect::new(
            self.center.x - self.radius,
            self.center.y - self.radius,
            self.radius * 2.0,
            self.radius * 2.0,
        )
    }
}

/// Collection of points (for scatter plots, drawing trails)
#[derive(Debug, Clone)]
pub struct Points {
    pub coords: Vec<Point>,
    pub color: Color,
}

impl Points {
    pub fn new(coords: Vec<Point>, color: Color) -> Self {
        Self { coords, color }
    }
}

impl Shape for Points {
    fn rasterize(&self, width: u32, height: u32, pixels: &mut [Color]) {
        for p in &self.coords {
            let x = p.x.round() as i32;
            let y = p.y.round() as i32;
            if x >= 0 && x < width as i32 && y >= 0 && y < height as i32 {
                let idx = (y as u32 * width + x as u32) as usize;
                if idx < pixels.len() {
                    pixels[idx] = self.color.blend_over(&pixels[idx]);
                }
            }
        }
    }

    fn bounds(&self) -> Rect {
        if self.coords.is_empty() {
            return Rect::default();
        }
        let mut min_x = f64::MAX;
        let mut min_y = f64::MAX;
        let mut max_x = f64::MIN;
        let mut max_y = f64::MIN;

        for p in &self.coords {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }

        Rect::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_point_distance() {
        let a = Point::new(0.0, 0.0);
        let b = Point::new(3.0, 4.0);
        assert!((a.distance(&b) - 5.0).abs() < 0.001);
    }

    #[test]
    fn test_rect_contains() {
        let r = Rect::new(10.0, 10.0, 20.0, 20.0);
        assert!(r.contains(Point::new(15.0, 15.0)));
        assert!(!r.contains(Point::new(5.0, 5.0)));
    }
}
