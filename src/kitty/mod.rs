mod probe;
mod transport;

use std::io::{self, Write};

use crate::{plan_damage, DamagePolicy, Frame, Rect};
pub use probe::{
    probe, probe_terminal, probe_terminal_with_events, probe_with_events, wait_for_ack,
    GraphicsCapabilities, TerminalProbe,
};
pub use transport::{
    GraphicsTransport, KittyTransmitter, TransferMedium, TransferOptions, TransferStats, ZlibPolicy,
};

/// A Kitty image placement in terminal character cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Placement {
    /// Zero-based destination column.
    pub column: u16,
    /// Zero-based destination row.
    pub row: u16,
    /// Placement width in cells.
    pub columns: u16,
    /// Placement height in cells.
    pub rows: u16,
}

impl Placement {
    /// Creates a nonempty image placement.
    pub fn new(column: u16, row: u16, columns: u16, rows: u16) -> io::Result<Self> {
        if columns == 0 || rows == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty Kitty placement",
            ));
        }
        Ok(Self {
            column,
            row,
            columns,
            rows,
        })
    }
}

/// Statistics for one successfully written frame presentation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PresentStats {
    /// Serial copied from the presented [`Frame`].
    pub serial: u64,
    /// Number of framebuffer regions transferred.
    pub regions: usize,
    /// Total number of pixels transferred.
    pub pixels: u64,
    /// Actual bytes written through the supplied writer.
    pub wire_bytes: usize,
    /// Whether the entire framebuffer was transferred.
    pub full_frame: bool,
    /// Transfer medium used by the final region, or `None` if none was sent.
    pub medium: Option<TransferMedium>,
}

/// Stateful damage-aware presenter for a stable Kitty image.
#[derive(Debug)]
pub struct KittyPresenter {
    image_id: u32,
    placement_id: u32,
    initialized_size: Option<(u32, u32)>,
    placement: Option<Placement>,
    animation: bool,
    transient: bool,
    transmitter: KittyTransmitter,
    transfer_options: TransferOptions,
    damage_policy: DamagePolicy,
}

impl KittyPresenter {
    /// Creates a presenter with animation updates enabled.
    ///
    /// `local_media` enables shared-memory and temporary-file transports;
    /// callers should only set it when the terminal shares the local host and
    /// filesystem namespace. Image identifier zero is normalized to one.
    pub fn new(image_id: u32, local_media: bool) -> Self {
        Self {
            image_id: image_id.max(1),
            placement_id: 1,
            initialized_size: None,
            placement: None,
            animation: true,
            transient: false,
            transmitter: KittyTransmitter::new(local_media),
            transfer_options: TransferOptions::default(),
            damage_policy: DamagePolicy::default(),
        }
    }

    /// Creates a presenter using conservative environment-based transport detection.
    pub fn detected(image_id: u32) -> Self {
        let mut value = Self::new(image_id, false);
        value.transmitter = KittyTransmitter::detect();
        value
    }

    pub(crate) fn probed(
        image_id: u32,
        animation: bool,
        transient: bool,
        shared_memory: bool,
        temporary_file: bool,
    ) -> Self {
        let mut value = Self::new(image_id, false);
        value.animation = animation;
        value.transient = transient;
        value.transmitter = KittyTransmitter::probed(shared_memory, temporary_file);
        value
    }

    /// Replaces the options used for subsequent payload transfers.
    pub fn set_transfer_options(&mut self, options: TransferOptions) {
        self.transfer_options = options;
    }

    /// Enables or disables Kitty's transient image usage hint.
    ///
    /// Enable this only after [`GraphicsCapabilities::transient`] has been
    /// actively verified. Older terminals may reject entire graphics commands
    /// containing an unknown `N` key.
    pub fn set_transient_hint(&mut self, enabled: bool) {
        self.transient = enabled;
    }

    /// Presents a frame at `placement`, transferring only planned damage when possible.
    ///
    /// Any write or flush failure invalidates cached terminal state, forcing the
    /// next presentation to retransmit the full framebuffer.
    pub fn present(
        &mut self,
        writer: &mut impl Write,
        frame: &Frame,
        placement: Placement,
    ) -> io::Result<PresentStats> {
        let size_changed = self.initialized_size != Some(frame.size());
        let (damage, initialize) = self.planned_damage(frame, size_changed);
        if damage.is_empty() && self.placement == Some(placement) {
            return Ok(PresentStats {
                serial: frame.serial(),
                ..PresentStats::default()
            });
        }

        let result = {
            let mut writer = CountingWriter::new(writer);
            (|| {
                writer.write_all(b"\x1b[s")?;
                let mut stats =
                    self.present_inner(&mut writer, frame, placement, initialize, &damage)?;
                writer.write_all(b"\x1b[u")?;
                writer.flush()?;
                stats.wire_bytes = writer.written();
                Ok(stats)
            })()
        };
        if result.is_ok() {
            self.initialized_size = Some(frame.size());
            self.placement = Some(placement);
        } else {
            // Any failed write or flush leaves the terminal's image state
            // uncertain. Force a complete reinitialization on the next call.
            self.invalidate();
        }
        result
    }

    fn planned_damage(&self, frame: &Frame, size_changed: bool) -> (Vec<Rect>, bool) {
        let mut damage = plan_damage(
            frame.width(),
            frame.height(),
            frame.damage().iter().copied(),
            size_changed,
            self.damage_policy,
        );
        // Baseline graphics has no in-place pixel update. Re-transmit and
        // replace the stable image whenever damaged pixels must change.
        let initialize = size_changed || (!self.animation && !damage.is_empty());
        if initialize {
            damage = vec![Rect::full(frame.width(), frame.height())];
        } else if damage.len() > 1 && self.transmitter.uses_local_media(self.transfer_options) {
            // Kitty reconstructs the complete animation frame after each edit.
            // With local media, one larger edit avoids repeated reconstruction
            // without increasing terminal-stream payload.
            damage = vec![damage
                .iter()
                .copied()
                .reduce(Rect::union)
                .expect("multiple damage rectangles are nonempty")];
        }
        (damage, initialize)
    }

    fn present_inner(
        &mut self,
        writer: &mut impl Write,
        frame: &Frame,
        placement: Placement,
        initialize: bool,
        damage: &[Rect],
    ) -> io::Result<PresentStats> {
        let mut stats = PresentStats {
            serial: frame.serial(),
            ..PresentStats::default()
        };
        if initialize {
            let full = Rect::full(frame.width(), frame.height());
            let rgb = frame.region_rgb_data(full)?;
            write!(
                writer,
                "\x1b[{};{}H",
                placement.row + 1,
                placement.column + 1
            )?;
            let transient = if self.transient { ",N=1" } else { "" };
            let transfer = self.transmitter.transmit(
                writer,
                &format!(
                    "a=T,f=24,s={},v={},i={},p={},q=2,C=1,c={},r={}{}",
                    frame.width(),
                    frame.height(),
                    self.image_id,
                    self.placement_id,
                    placement.columns,
                    placement.rows,
                    transient
                ),
                rgb.as_ref(),
                false,
                self.transfer_options,
            )?;
            update_stats(&mut stats, full, transfer);
            stats.regions = 1;
            stats.full_frame = true;
            return Ok(stats);
        }

        if self.placement != Some(placement) {
            write!(
                writer,
                "\x1b[{};{}H\x1b_Ga=p,i={},p={},q=2,C=1,c={},r={};\x1b\\",
                placement.row + 1,
                placement.column + 1,
                self.image_id,
                self.placement_id,
                placement.columns,
                placement.rows
            )?;
        }
        let transient = if self.transient { ",N=1" } else { "" };
        for &rect in damage {
            let rgb = frame.region_rgb_data(rect)?;
            let transfer = self.transmitter.transmit(
                writer,
                &format!(
                    "a=f,r=1,i={},f=24,q=2,x={},y={},s={},v={},X=1{}",
                    self.image_id, rect.x, rect.y, rect.width, rect.height, transient
                ),
                rgb.as_ref(),
                true,
                self.transfer_options,
            )?;
            update_stats(&mut stats, rect, transfer);
        }
        if !damage.is_empty() {
            select_frame(writer, self.image_id, 1)?;
            stats.regions = damage.len();
            stats.full_frame = damage == [Rect::full(frame.width(), frame.height())];
        }
        Ok(stats)
    }

    /// Forgets cached terminal image and placement state.
    pub fn invalidate(&mut self) {
        self.initialized_size = None;
        self.placement = None;
    }

    /// Deletes the presenter's image from the terminal and invalidates local state.
    pub fn delete(&mut self, writer: &mut impl Write) -> io::Result<()> {
        let result = (|| {
            write!(writer, "\x1b_Ga=d,d=I,i={},q=2;\x1b\\", self.image_id)?;
            writer.flush()
        })();
        // A successful delete removes the image, while a failed write leaves
        // terminal state uncertain. Both cases require reinitialization.
        self.invalidate();
        result
    }
}

fn update_stats(stats: &mut PresentStats, rect: Rect, transfer: TransferStats) {
    stats.pixels += rect.area();
    stats.medium = Some(transfer.medium);
}

struct CountingWriter<'a, W> {
    inner: &'a mut W,
    written: usize,
}

impl<'a, W> CountingWriter<'a, W> {
    fn new(inner: &'a mut W) -> Self {
        Self { inner, written: 0 }
    }

    fn written(&self) -> usize {
        self.written
    }
}

impl<W: Write> Write for CountingWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.written = self.written.saturating_add(written);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Writes a Kitty animation command selecting `frame` for `image_id`.
pub fn select_frame(writer: &mut impl Write, image_id: u32, frame: u32) -> io::Result<()> {
    write!(writer, "\x1b_Ga=a,q=2,c={frame},i={image_id};\x1b\\")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn frame(serial: u64, damage: Vec<Rect>) -> Frame {
        Frame::rgb(serial, 4, 2, 12, Arc::<[u8]>::from(vec![0x44; 24]), damage).unwrap()
    }

    #[test]
    fn local_media_coalesces_multiple_animation_edits() {
        let damaged = frame(2, vec![Rect::new(0, 0, 1, 1), Rect::new(3, 1, 1, 1)]);
        let mut presenter = KittyPresenter::new(7, true);
        let (damage, initialize) = presenter.planned_damage(&damaged, false);
        assert!(!initialize);
        assert_eq!(damage, [Rect::full(4, 2)]);

        presenter.set_transfer_options(TransferOptions {
            transport: GraphicsTransport::Direct,
            ..TransferOptions::default()
        });
        let (damage, initialize) = presenter.planned_damage(&damaged, false);
        assert!(!initialize);
        assert_eq!(damage.len(), 2);
    }

    #[test]
    fn stable_image_is_initialized_then_partially_updated() {
        let placement = Placement::new(0, 0, 4, 2).unwrap();
        let mut presenter = KittyPresenter::new(7, false);
        presenter.set_transfer_options(TransferOptions {
            transport: GraphicsTransport::Direct,
            zlib: ZlibPolicy::Never,
            chunk_size: 4096,
        });
        let mut output = Vec::new();
        let initial_stats = presenter
            .present(&mut output, &frame(1, vec![]), placement)
            .unwrap();
        assert!(initial_stats.full_frame);
        assert_eq!(initial_stats.wire_bytes, output.len());

        let split = output.len();
        let stats = presenter
            .present(
                &mut output,
                &frame(2, vec![Rect::new(1, 0, 1, 1)]),
                placement,
            )
            .unwrap();
        let update = String::from_utf8_lossy(&output[split..]);
        assert_eq!(stats.regions, 1);
        assert_eq!(stats.wire_bytes, output.len() - split);
        assert!(update.contains("a=f,r=1,i=7"));
        assert!(update.contains("a=a,q=2,c=1,i=7;"));
        assert!(!update.contains("a=T"));
    }

    #[test]
    fn verified_transient_hint_is_sent_with_image_data() {
        let placement = Placement::new(0, 0, 4, 2).unwrap();
        let mut presenter = KittyPresenter::new(7, false);
        presenter.set_transient_hint(true);
        presenter.set_transfer_options(TransferOptions {
            transport: GraphicsTransport::Direct,
            zlib: ZlibPolicy::Never,
            chunk_size: 4096,
        });
        let mut output = Vec::new();
        presenter
            .present(&mut output, &frame(1, vec![]), placement)
            .unwrap();
        let split = output.len();
        presenter
            .present(
                &mut output,
                &frame(2, vec![Rect::new(1, 0, 1, 1)]),
                placement,
            )
            .unwrap();
        let initial = String::from_utf8_lossy(&output[..split]);
        let update = String::from_utf8_lossy(&output[split..]);
        assert!(initial.contains("a=T,f=24") && initial.contains(",N=1,t=d,"));
        assert!(update.contains("a=f,r=1") && update.contains(",N=1,t=d,"));
    }

    #[test]
    fn baseline_graphics_retransmits_damaged_frames() {
        let placement = Placement::new(0, 0, 4, 2).unwrap();
        let mut presenter = KittyPresenter::probed(7, false, false, false, false);
        presenter.set_transfer_options(TransferOptions {
            transport: GraphicsTransport::Direct,
            zlib: ZlibPolicy::Never,
            chunk_size: 4096,
        });
        presenter
            .present(&mut Vec::new(), &frame(1, vec![]), placement)
            .unwrap();

        let mut output = Vec::new();
        let stats = presenter
            .present(
                &mut output,
                &frame(2, vec![Rect::new(1, 0, 1, 1)]),
                placement,
            )
            .unwrap();
        assert!(stats.full_frame);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("a=T"));
        assert!(!output.contains("a=f"));
    }

    struct FailFlush(Vec<u8>);

    impl Write for FailFlush {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "flush failed"))
        }
    }

    #[test]
    fn failed_delete_invalidates_the_image() {
        let placement = Placement::new(0, 0, 4, 2).unwrap();
        let mut presenter = KittyPresenter::new(7, false);
        presenter.set_transfer_options(TransferOptions {
            transport: GraphicsTransport::Direct,
            zlib: ZlibPolicy::Never,
            chunk_size: 4096,
        });
        presenter
            .present(&mut Vec::new(), &frame(1, vec![]), placement)
            .unwrap();

        assert!(presenter.delete(&mut FailFlush(Vec::new())).is_err());
        let mut retry = Vec::new();
        assert!(
            presenter
                .present(&mut retry, &frame(2, vec![]), placement)
                .unwrap()
                .full_frame
        );
        assert!(String::from_utf8(retry).unwrap().contains("a=T"));
    }

    #[test]
    fn failed_final_flush_invalidates_the_image() {
        let placement = Placement::new(0, 0, 4, 2).unwrap();
        let mut presenter = KittyPresenter::new(7, false);
        presenter.set_transfer_options(TransferOptions {
            transport: GraphicsTransport::Direct,
            zlib: ZlibPolicy::Never,
            chunk_size: 4096,
        });
        presenter
            .present(&mut Vec::new(), &frame(1, vec![]), placement)
            .unwrap();

        let mut failing = FailFlush(Vec::new());
        assert!(presenter
            .present(
                &mut failing,
                &frame(2, vec![Rect::new(1, 0, 1, 1)]),
                placement,
            )
            .is_err());

        let mut retry = Vec::new();
        let stats = presenter
            .present(&mut retry, &frame(3, vec![]), placement)
            .unwrap();
        assert!(stats.full_frame);
        assert!(String::from_utf8(retry).unwrap().contains("a=T"));
    }
}
