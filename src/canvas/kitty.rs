//! Kitty Graphics Protocol rasterizer
//!
//! Sends actual PNG images to the terminal for pixel-perfect rendering.
//! Supported by: Kitty, WezTerm, Ghostty, iTerm2 (with adaptations)
//!
//! Protocol: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

use super::canvas::Canvas;
use super::caps::TerminalCaps;
use super::rasterizer::{RasterOutput, Rasterizer};

/// Kitty Graphics Protocol rasterizer
///
/// Sends PNG images via escape sequences for pixel-perfect graphics.
#[derive(Debug, Default)]
pub struct KittyRasterizer {
    /// Compression level (0-9, higher = smaller but slower)
    compression: u8,
}

impl KittyRasterizer {
    pub fn new() -> Self {
        Self { compression: 6 }
    }

    /// Encode canvas as PNG and create Kitty escape sequence
    fn encode_kitty(&self, canvas: &Canvas, _caps: &TerminalCaps) -> String {
        // Convert canvas to RGBA bytes
        let mut rgba_data = Vec::with_capacity((canvas.width * canvas.height * 4) as usize);

        for color in canvas.pixels() {
            let (r, g, b) = color.to_rgb8();
            let a = (color.a.clamp(0.0, 1.0) * 255.0) as u8;
            rgba_data.extend_from_slice(&[r, g, b, a]);
        }

        // Encode as PNG
        let png_data = match self.encode_png(&rgba_data, canvas.width, canvas.height) {
            Ok(data) => data,
            Err(_) => return String::new(),
        };

        // Base64 encode
        let b64 = BASE64.encode(&png_data);

        // Build Kitty Graphics escape sequence
        // a=T: transmit and display
        // f=100: PNG format
        // q=2: suppress response
        self.build_kitty_sequence(&b64, canvas.width, canvas.height)
    }

    /// Encode RGBA data as PNG
    fn encode_png(&self, rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
        // Simple PNG encoder (uncompressed for now)
        // In production, use the `png` crate
        let mut output = Vec::new();

        // PNG signature
        output.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

        // IHDR chunk
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.push(8); // bit depth
        ihdr.push(6); // RGBA
        ihdr.push(0); // compression
        ihdr.push(0); // filter
        ihdr.push(0); // interlace
        self.write_chunk(&mut output, b"IHDR", &ihdr);

        // IDAT chunk (image data)
        // For simplicity, use uncompressed zlib
        let mut idat = Vec::new();

        // Zlib header (no compression)
        idat.push(0x78); // CMF
        idat.push(0x01); // FLG

        // Build raw image data with filter bytes
        let mut raw_data = Vec::new();
        for y in 0..height {
            raw_data.push(0); // Filter type: None
            let row_start = (y * width * 4) as usize;
            let row_end = row_start + (width * 4) as usize;
            raw_data.extend_from_slice(&rgba[row_start..row_end]);
        }

        // Compress with deflate (stored blocks for simplicity)
        let compressed = self.deflate_store(&raw_data);
        idat.extend_from_slice(&compressed);

        // Adler-32 checksum
        let adler = self.adler32(&raw_data);
        idat.extend_from_slice(&adler.to_be_bytes());

        self.write_chunk(&mut output, b"IDAT", &idat);

        // IEND chunk
        self.write_chunk(&mut output, b"IEND", &[]);

        Ok(output)
    }

    /// Write PNG chunk
    fn write_chunk(&self, output: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
        // Length
        output.extend_from_slice(&(data.len() as u32).to_be_bytes());
        // Type
        output.extend_from_slice(chunk_type);
        // Data
        output.extend_from_slice(data);
        // CRC
        let mut crc_data = Vec::with_capacity(4 + data.len());
        crc_data.extend_from_slice(chunk_type);
        crc_data.extend_from_slice(data);
        let crc = self.crc32(&crc_data);
        output.extend_from_slice(&crc.to_be_bytes());
    }

    /// Simple CRC32 for PNG
    fn crc32(&self, data: &[u8]) -> u32 {
        let mut crc: u32 = 0xFFFFFFFF;
        for byte in data {
            crc ^= *byte as u32;
            for _ in 0..8 {
                if crc & 1 != 0 {
                    crc = (crc >> 1) ^ 0xEDB88320;
                } else {
                    crc >>= 1;
                }
            }
        }
        !crc
    }

    /// Adler-32 checksum for zlib
    fn adler32(&self, data: &[u8]) -> u32 {
        let mut a: u32 = 1;
        let mut b: u32 = 0;
        for byte in data {
            a = (a + *byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }

    /// Deflate with stored blocks (no compression)
    fn deflate_store(&self, data: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        let max_block = 65535; // Max stored block size

        let chunks: Vec<&[u8]> = data.chunks(max_block).collect();
        let num_chunks = chunks.len();

        for (i, chunk) in chunks.iter().enumerate() {
            let is_final = i == num_chunks - 1;
            let len = chunk.len() as u16;
            let nlen = !len;

            // Block header
            output.push(if is_final { 0x01 } else { 0x00 });
            output.extend_from_slice(&len.to_le_bytes());
            output.extend_from_slice(&nlen.to_le_bytes());
            output.extend_from_slice(chunk);
        }

        output
    }

    /// Build Kitty Graphics Protocol escape sequence
    fn build_kitty_sequence(&self, b64: &str, width: u32, height: u32) -> String {
        let mut result = String::new();

        // Split into chunks of 4096 bytes
        let chunk_size = 4096;
        let chunks: Vec<&str> = b64
            .as_bytes()
            .chunks(chunk_size)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();

        let num_chunks = chunks.len();

        for (i, chunk) in chunks.iter().enumerate() {
            let is_first = i == 0;
            let is_last = i == num_chunks - 1;
            let more = if is_last { 0 } else { 1 };

            result.push_str("\x1b_G");

            if is_first {
                // First chunk: include metadata
                result.push_str(&format!(
                    "a=T,f=100,s={},v={},m={};{}",
                    width, height, more, chunk
                ));
            } else {
                // Continuation chunk
                result.push_str(&format!("m={};{}", more, chunk));
            }

            result.push_str("\x1b\\");
        }

        result
    }
}

impl Rasterizer for KittyRasterizer {
    fn rasterize(&self, canvas: &Canvas, caps: &TerminalCaps) -> RasterOutput {
        let escape = self.encode_kitty(canvas, caps);
        RasterOutput::Escape(escape)
    }

    fn resolution_multiplier(&self) -> (u32, u32) {
        // Pixel-perfect - depends on cell size
        (8, 16) // Assuming 8x16 cell
    }

    fn name(&self) -> &'static str {
        "Kitty"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kitty_sequence_generation() {
        let rasterizer = KittyRasterizer::new();
        let canvas = Canvas::new(10, 10);
        let caps = TerminalCaps::default();

        let output = rasterizer.rasterize(&canvas, &caps);

        if let RasterOutput::Escape(seq) = output {
            // Should start with Kitty escape
            assert!(seq.starts_with("\x1b_G"));
            // Should end with string terminator
            assert!(seq.ends_with("\x1b\\"));
        } else {
            panic!("Expected Escape output");
        }
    }
}
