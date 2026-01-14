//! # `TermCanvas` - God Tier TUI Graphics Engine
//!
//! Hierarchical rendering:
//! 1. Kitty/Sixel Graphics (pixel perfect) - if terminal supports
//! 2. `HalfBlock` ▀▄ (2 colors per cell) - universal fallback
//! 3. Braille ⠀⠁⠂⠃ (8 subpixels) - sparklines only
//!
//! ## Architecture
//!
//! ```text
//!                 ┌─────────────────────────┐
//!                 │    Virtual Canvas       │
//!                 │  (float coords, RGBA)   │
//!                 └───────────┬─────────────┘
//!                             │
//!          ┌──────────────────┼──────────────────┐
//!          │                  │                  │
//!          ▼                  ▼                  ▼
//!    ┌───────────┐     ┌───────────┐     ┌───────────┐
//!    │  Kitty    │     │ HalfBlock │     │  Braille  │
//!    │  Graphics │     │   ▀▄█     │     │   ⠛⠛⠛    │
//!    └───────────┘     └───────────┘     └───────────┘
//! ```

mod caps;
mod canvas;
mod color;
mod halfblock;
mod kitty;
mod rasterizer;
mod shapes;

pub use canvas::Canvas;
pub use caps::{GraphicsProtocol, TerminalCaps, UnicodeLevel};
pub use color::Color;
pub use halfblock::HalfBlockRasterizer;
pub use kitty::KittyRasterizer;
pub use rasterizer::{RasterOutput, Rasterizer};
pub use shapes::{Circle, Line, Point, Points, Rect, Shape};
