//! A BGRA video frame buffer.

/// A tightly packed 8-bit BGRA frame (premultiplied alpha is NOT used: straight alpha).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes, order B, G, R, A.
    pub bgra: Vec<u8>,
}

impl Frame {
    /// A frame filled with one colour (b, g, r, a).
    pub fn solid(width: u32, height: u32, b: u8, g: u8, r: u8, a: u8) -> Frame {
        let n = (width as usize).saturating_mul(height as usize);
        let mut bgra = Vec::with_capacity(n.saturating_mul(4));
        for _ in 0..n {
            bgra.extend_from_slice(&[b, g, r, a]);
        }
        Frame {
            width,
            height,
            bgra,
        }
    }

    /// Pixel (b, g, r, a) at x, y; None when out of bounds.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y as usize) * (self.width as usize) + x as usize) * 4;
        let s = self.bgra.get(i..i + 4)?;
        Some([*s.first()?, *s.get(1)?, *s.get(2)?, *s.get(3)?])
    }
}
