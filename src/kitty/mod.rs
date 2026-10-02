mod probe;
mod transport;

use std::io::{self, Write};

use crate::{plan_damage, DamagePolicy, Frame, Rect};
pub use probe::{probe, wait_for_ack, GraphicsCapabilities};
pub use transport::{
    GraphicsTransport, TransferMedium, TransferOptions, TransferStats, TransportManager, ZlibPolicy,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Placement {
    pub column: u16,
    pub row: u16,
    pub columns: u16,
    pub rows: u16,
}

impl Placement {
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PresentStats {
    pub serial: u64,
    pub regions: usize,
    pub pixels: u64,
    pub wire_bytes: usize,
    pub full_frame: bool,
    pub medium: Option<TransferMedium>,
}

#[derive(Debug)]
pub struct KittyPresenter {
    image_id: u32,
    placement_id: u32,
    initialized_size: Option<(u32, u32)>,
    placement: Option<Placement>,
    transport: TransportManager,
    transfer_options: TransferOptions,
    damage_policy: DamagePolicy,
}

impl KittyPresenter {
    pub fn new(image_id: u32, local_media: bool) -> Self {
        Self {
            image_id: image_id.max(1),
            placement_id: 1,
            initialized_size: None,
            placement: None,
            transport: TransportManager::new(local_media),
            transfer_options: TransferOptions::default(),
            damage_policy: DamagePolicy::default(),
        }
    }

    pub fn detected(image_id: u32) -> Self {
        let mut value = Self::new(image_id, false);
        value.transport = TransportManager::detect();
        value
    }

    pub fn set_transfer_options(&mut self, options: TransferOptions) {
        self.transfer_options = options;
    }

    pub fn present(
        &mut self,
        writer: &mut impl Write,
        frame: &Frame,
        placement: Placement,
    ) -> io::Result<PresentStats> {
        let initialize = self.initialized_size != Some(frame.size());
        let damage = plan_damage(
            frame.width,
            frame.height,
            frame.damage.iter().copied(),
            initialize,
            self.damage_policy,
        );
        if damage.is_empty() && self.placement == Some(placement) {
            return Ok(PresentStats {
                serial: frame.serial,
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

    fn present_inner(
        &mut self,
        writer: &mut impl Write,
        frame: &Frame,
        placement: Placement,
        initialize: bool,
        damage: &[Rect],
    ) -> io::Result<PresentStats> {
        let mut stats = PresentStats {
            serial: frame.serial,
            ..PresentStats::default()
        };
        if initialize {
            let full = Rect::full(frame.width, frame.height);
            let rgb = frame.region_rgb(full)?;
            write!(
                writer,
                "\x1b[{};{}H",
                placement.row + 1,
                placement.column + 1
            )?;
            let transfer = self.transport.transmit(
                writer,
                &format!(
                    "a=T,f=24,s={},v={},i={},p={},q=2,C=1,c={},r={}",
                    frame.width,
                    frame.height,
                    self.image_id,
                    self.placement_id,
                    placement.columns,
                    placement.rows
                ),
                &rgb,
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
        for &rect in damage {
            let rgb = frame.region_rgb(rect)?;
            let transfer = self.transport.transmit(
                writer,
                &format!(
                    "a=f,r=1,i={},f=24,q=2,x={},y={},s={},v={},X=1",
                    self.image_id, rect.x, rect.y, rect.width, rect.height
                ),
                &rgb,
                true,
                self.transfer_options,
            )?;
            update_stats(&mut stats, rect, transfer);
        }
        if !damage.is_empty() {
            select_frame(writer, self.image_id, 1)?;
            stats.regions = damage.len();
            stats.full_frame = damage == [Rect::full(frame.width, frame.height)];
        }
        Ok(stats)
    }

    pub fn invalidate(&mut self) {
        self.initialized_size = None;
        self.placement = None;
    }

    pub fn delete(&mut self, writer: &mut impl Write) -> io::Result<()> {
        write!(writer, "\x1b_Ga=d,d=I,i={},q=2;\x1b\\", self.image_id)?;
        writer.flush()?;
        self.invalidate();
        Ok(())
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
