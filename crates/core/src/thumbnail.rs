//! A VM's console thumbnail as the frontends see it (design 5.13).
//!
//! Every backend turns whatever Hyper-V (or a stand-in) answers into this one
//! shape, so the frontends never deal with a pixel format.

use std::fmt;

/// A thumbnail: tightly packed 8-bit RGBA, row by row, top to bottom.
#[derive(Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, RGBA in that order.
    pub rgba: Vec<u8>,
}

impl Thumbnail {
    /// Decodes tightly packed little-endian RGB565 pixels (two bytes each, R in
    /// the high five bits, G in the middle six, B in the low five) into RGBA.
    ///
    /// Returns `None` when the buffer is not exactly `width * height * 2` bytes.
    /// Hyper-V's `ImageData` format past its four-byte length prefix (M13-V:
    /// `docs/hypervctl-m13-findings.md`).
    pub fn from_rgb565(width: u32, height: u32, pixels: &[u8]) -> Option<Self> {
        let count = width as usize * height as usize;
        if pixels.len() != count * 2 {
            return None;
        }
        let mut rgba = Vec::with_capacity(count * 4);
        for pixel in pixels.chunks_exact(2) {
            let value = u16::from_le_bytes([pixel[0], pixel[1]]);
            let r = (value >> 11) & 0x1F;
            let g = (value >> 5) & 0x3F;
            let b = value & 0x1F;
            // Replicate the high bits so full scale becomes 0xFF, not 0xF8.
            rgba.push(((r << 3) | (r >> 2)) as u8);
            rgba.push(((g << 2) | (g >> 4)) as u8);
            rgba.push(((b << 3) | (b >> 2)) as u8);
            rgba.push(0xFF);
        }
        Some(Self {
            width,
            height,
            rgba,
        })
    }
}

/// The pixel buffer is left out: it is large and never worth printing.
impl fmt::Debug for Thumbnail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Thumbnail")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.rgba.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_rgb565_to_rgba() {
        // Pure red (0xF800), green (0x07E0), blue (0x001F), white (0xFFFF).
        let pixels = [0x00, 0xF8, 0xE0, 0x07, 0x1F, 0x00, 0xFF, 0xFF];
        let image = match Thumbnail::from_rgb565(4, 1, &pixels) {
            Some(image) => image,
            None => panic!("four pixels of two bytes each must decode"),
        };
        assert_eq!(image.width, 4);
        assert_eq!(image.height, 1);
        assert_eq!(
            image.rgba,
            vec![
                0xFF, 0x00, 0x00, 0xFF, // red
                0x00, 0xFF, 0x00, 0xFF, // green
                0x00, 0x00, 0xFF, 0xFF, // blue
                0xFF, 0xFF, 0xFF, 0xFF, // white
            ]
        );
    }

    #[test]
    fn the_high_bits_are_replicated() {
        // 0x0821 = R=1, G=1, B=1: each channel must scale to its own maximum
        // (0x08/0x04/0x08), not just be shifted.
        let pixels = [0x21, 0x08];
        let image = match Thumbnail::from_rgb565(1, 1, &pixels) {
            Some(image) => image,
            None => panic!("one pixel must decode"),
        };
        assert_eq!(image.rgba, vec![0x08, 0x04, 0x08, 0xFF]);
    }

    #[test]
    fn a_buffer_of_the_wrong_length_is_rejected() {
        assert!(Thumbnail::from_rgb565(2, 2, &[0, 0, 0, 0]).is_none());
    }
}
