use std::{borrow::Cow, io, sync::Arc, time::Instant};

use super::Rect;

/// An immutable RGB framebuffer and the regions that changed.
///
/// [`Self::pixels`] always contains the complete current framebuffer, not only
/// the damaged regions. [`Self::damage`] identifies pixels changed relative to
/// the previously submitted state so a presenter can avoid retransmitting the
/// rest. An empty damage list means that pixel content is unchanged, although a
/// presenter may still initialize a new image or update its placement.
///
/// Presenters may clip or coalesce damage rectangles. When a pending
/// presentation is replaced in a [`crate::LatestFrameMailbox`], its frame's
/// damage is carried into the replacement so pixels changed by a skipped frame
/// are eventually presented.
/// Tilcayo's Kitty presenter sends a first frame or a framebuffer-size change
/// in full regardless of its damage list.
#[derive(Clone, Debug)]
pub struct Frame {
    serial: u64,
    width: u32,
    height: u32,
    stride: usize,
    pixels: Arc<[u8]>,
    damage: Vec<Rect>,
    pub(crate) produced_at: Instant,
}

impl Frame {
    /// Creates and validates a packed 24-bit RGB framebuffer.
    ///
    /// The buffer may contain row padding but must hold at least `height`
    /// complete rows. Width, height, and the usable row size must be nonzero.
    pub fn rgb(
        serial: u64,
        width: u32,
        height: u32,
        stride: usize,
        pixels: impl Into<Arc<[u8]>>,
        damage: Vec<Rect>,
    ) -> io::Result<Self> {
        let pixels = pixels.into();
        let row = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(3))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame row overflow"))?;
        if width == 0 || height == 0 || stride < row {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid RGB layout",
            ));
        }
        let preceding_rows = usize::try_from(height - 1)
            .ok()
            .and_then(|height| stride.checked_mul(height))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame size overflow"))?;
        let required = preceding_rows
            .checked_add(row)
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

    /// Returns the application-assigned sequence number.
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Returns the framebuffer width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Returns the framebuffer height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Returns `(width, height)` in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Returns the byte distance between starts of adjacent rows.
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// Returns the complete packed RGB pixel storage.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Returns pixel-space regions changed since the previous submitted state.
    pub fn damage(&self) -> &[Rect] {
        &self.damage
    }

    pub(crate) fn damage_mut(&mut self) -> &mut Vec<Rect> {
        &mut self.damage
    }

    /// Copies a clipped rectangle into tightly packed RGB storage.
    ///
    /// Returns an error when the rectangle does not overlap the framebuffer.
    pub fn region_rgb(&self, rect: Rect) -> io::Result<Vec<u8>> {
        self.region_rgb_data(rect).map(Cow::into_owned)
    }

    pub(crate) fn region_rgb_data(&self, rect: Rect) -> io::Result<Cow<'_, [u8]>> {
        let rect = rect.clip(self.width, self.height).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "damage is outside framebuffer")
        })?;
        let row_bytes = rect.width as usize * 3;
        let start = rect.y as usize * self.stride + rect.x as usize * 3;
        if rect.height == 1 || (rect.x == 0 && row_bytes == self.stride) {
            let len = row_bytes * rect.height as usize;
            return Ok(Cow::Borrowed(&self.pixels[start..start + len]));
        }

        let mut result = Vec::with_capacity(row_bytes * rect.height as usize);
        for y in rect.y..rect.y + rect.height {
            let start = y as usize * self.stride + rect.x as usize * 3;
            result.extend_from_slice(&self.pixels[start..start + row_bytes]);
        }
        Ok(Cow::Owned(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_storage_without_padding_after_the_final_row() {
        let frame = Frame::rgb(3, 2, 2, 8, vec![0; 14], vec![Rect::full(2, 2)]).unwrap();
        assert_eq!(frame.serial(), 3);
        assert_eq!(frame.size(), (2, 2));
        assert_eq!(frame.stride(), 8);
        assert_eq!(frame.pixels().len(), 14);
        assert_eq!(frame.damage(), [Rect::full(2, 2)]);
        assert!(Frame::rgb(3, 2, 2, 8, vec![0; 13], vec![]).is_err());
    }

    #[test]
    fn borrows_contiguous_regions_and_copies_strided_regions() {
        let packed = Frame::rgb(
            1,
            2,
            2,
            6,
            vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
            vec![],
        )
        .unwrap();
        assert!(matches!(
            packed.region_rgb_data(Rect::full(2, 2)).unwrap(),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            packed.region_rgb_data(Rect::new(1, 0, 1, 1)).unwrap(),
            Cow::Borrowed(&[3, 4, 5])
        ));
        assert!(matches!(
            packed.region_rgb_data(Rect::new(1, 0, 1, 2)).unwrap(),
            Cow::Owned(ref bytes) if bytes == &[3, 4, 5, 9, 10, 11]
        ));

        let padded = Frame::rgb(
            2,
            2,
            2,
            8,
            vec![0, 1, 2, 3, 4, 5, 99, 99, 6, 7, 8, 9, 10, 11, 99, 99],
            vec![],
        )
        .unwrap();
        assert!(matches!(
            padded.region_rgb_data(Rect::full(2, 2)).unwrap(),
            Cow::Owned(ref bytes) if bytes == &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
        ));
    }
}
