use std::{io, sync::Arc, time::Instant};

use super::Rect;

/// Immutable, tightly-packed RGB framebuffer submitted to a presenter.
#[derive(Clone, Debug)]
pub struct Frame {
    pub serial: u64,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub pixels: Arc<[u8]>,
    pub damage: Vec<Rect>,
    pub(crate) produced_at: Instant,
}

impl Frame {
    pub fn rgb(
        serial: u64,
        width: u32,
        height: u32,
        stride: usize,
        pixels: impl Into<Arc<[u8]>>,
        damage: Vec<Rect>,
    ) -> io::Result<Self> {
        let pixels = pixels.into();
        let row = width as usize * 3;
        if width == 0 || height == 0 || stride < row {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid RGB layout",
            ));
        }
        let required = stride
            .checked_mul(height as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame size overflow"))?;
        if pixels.len() < required {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "short RGB framebuffer",
            ));
        }
        Ok(Self {
            serial,
            width,
            height,
            stride,
            pixels,
            damage,
            produced_at: Instant::now(),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn region_rgb(&self, rect: Rect) -> io::Result<Vec<u8>> {
        let rect = rect.clip(self.width, self.height).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "damage is outside framebuffer")
        })?;
        let row_bytes = rect.width as usize * 3;
        let mut result = Vec::with_capacity(row_bytes * rect.height as usize);
        for y in rect.y..rect.y + rect.height {
            let start = y as usize * self.stride + rect.x as usize * 3;
            result.extend_from_slice(&self.pixels[start..start + row_bytes]);
        }
        Ok(result)
    }
}
