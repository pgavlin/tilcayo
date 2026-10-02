use std::{
    collections::VecDeque,
    io::{self, Write},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Instant,
};

use crate::{
    kitty::{KittyPresenter, Placement, PresentStats},
    mailbox::MailboxEvent,
    Frame, LatestFrameMailbox,
};

/// Receives low-overhead notifications from a [`PresenterWorker`].
///
/// Implementations must not block the presentation thread. The default `()`
/// observer ignores all notifications.
pub trait PresentationObserver: Send + Sync + 'static {
    /// Called after a frame is accepted by the latest-frame mailbox.
    fn submitted(&self) {}

    /// Called after a frame's complete terminal write succeeds.
    ///
    /// `elapsed` measures presentation work and `pipeline` measures time since
    /// frame construction.
    fn presented(
        &self,
        _stats: PresentStats,
        _elapsed: std::time::Duration,
        _pipeline: std::time::Duration,
    ) {
    }
}

impl PresentationObserver for () {}

/// Owns blocking terminal output on a dedicated thread.
pub struct PresenterWorker {
    mailbox: LatestFrameMailbox,
    error: Arc<Mutex<Option<io::Error>>>,
    presented: Arc<AtomicU64>,
    placement: Arc<Mutex<Placement>>,
    commands: Arc<Mutex<VecDeque<Vec<u8>>>>,
    observer: Arc<dyn PresentationObserver>,
    thread: Option<JoinHandle<()>>,
}

impl PresenterWorker {
    /// Starts a presentation thread with no instrumentation observer.
    pub fn spawn<W>(writer: W, presenter: KittyPresenter, placement: Placement) -> Self
    where
        W: Write + Send + 'static,
    {
        Self::spawn_instrumented(writer, presenter, placement, Arc::new(()))
    }

    /// Starts a presentation thread that reports activity to `observer`.
    pub fn spawn_instrumented<W, O>(
        mut writer: W,
        mut presenter: KittyPresenter,
        placement: Placement,
        observer: Arc<O>,
    ) -> Self
    where
        W: Write + Send + 'static,
        O: PresentationObserver,
    {
        let mailbox = LatestFrameMailbox::default();
        let worker_mailbox = mailbox.clone();
        let error = Arc::new(Mutex::new(None));
        let worker_error = error.clone();
        let presented = Arc::new(AtomicU64::new(0));
        let worker_presented = presented.clone();
        let placement = Arc::new(Mutex::new(placement));
        let worker_placement = placement.clone();
        let commands = Arc::new(Mutex::new(VecDeque::<Vec<u8>>::new()));
        let worker_commands = commands.clone();
        let worker_observer = observer.clone();
        let thread = thread::Builder::new()
            .name("tilcayo-presenter".into())
            .spawn(move || loop {
                let event = worker_mailbox.take_event();
                let closed = matches!(event, MailboxEvent::Closed);
                let result = (|| {
                    while let Some(command) = worker_commands
                        .lock()
                        .expect("presenter command lock poisoned")
                        .pop_front()
                    {
                        writer.write_all(&command)?;
                        writer.flush()?;
                    }
                    if let MailboxEvent::Frame(frame) = event {
                        let serial = frame.serial;
                        let placement = *worker_placement
                            .lock()
                            .expect("presenter placement lock poisoned");
                        let present_started = Instant::now();
                        let stats = presenter.present(&mut writer, &frame, placement)?;
                        let completed = Instant::now();
                        worker_observer.presented(
                            stats,
                            completed.duration_since(present_started),
                            completed.duration_since(frame.produced_at),
                        );
                        worker_presented.store(serial, Ordering::Release);
                    }
                    Ok::<_, io::Error>(())
                })();
                if let Err(value) = result {
                    *worker_error.lock().expect("presenter error lock poisoned") = Some(value);
                    worker_mailbox.close();
                    break;
                }
                if closed {
                    break;
                }
            })
            .expect("spawn presenter thread");
        Self {
            mailbox,
            error,
            presented,
            placement,
            commands,
            observer,
            thread: Some(thread),
        }
    }

    /// Submits a frame without blocking on terminal output.
    ///
    /// Returns the frame if the mailbox is closed.
    pub fn submit(&self, frame: Frame) -> Result<(), Frame> {
        let result = self.mailbox.submit(frame);
        if result.is_ok() {
            self.observer.submitted();
        }
        result
    }

    /// Returns the number of pending frames replaced by newer submissions.
    pub fn dropped(&self) -> u64 {
        self.mailbox.dropped()
    }

    /// Queue a bounded terminal control write on the presentation thread.
    /// A new control write replaces an older pending write rather than blocking
    /// the producer behind a slow terminal.
    pub fn write_control(&self, bytes: Vec<u8>) -> io::Result<()> {
        if self.error().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "presenter stopped",
            ));
        }
        let mut commands = self
            .commands
            .lock()
            .expect("presenter command lock poisoned");
        commands.clear();
        commands.push_back(bytes);
        drop(commands);
        self.mailbox.wake();
        Ok(())
    }

    /// Update the terminal placement used by the next frame.
    pub fn set_placement(&self, placement: Placement) {
        *self
            .placement
            .lock()
            .expect("presenter placement lock poisoned") = placement;
    }

    /// Serial of the newest frame whose complete terminal write succeeded.
    pub fn presented_serial(&self) -> u64 {
        self.presented.load(Ordering::Acquire)
    }

    /// Returns a copy of the output error that stopped the worker, if any.
    pub fn error(&self) -> Option<io::Error> {
        self.error
            .lock()
            .expect("presenter error lock poisoned")
            .as_ref()
            .map(|error| io::Error::new(error.kind(), error.to_string()))
    }

    /// Closes the mailbox, joins the presentation thread, and returns its error.
    pub fn shutdown(mut self) -> io::Result<()> {
        self.mailbox.close();
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| io::Error::other("presenter thread panicked"))?;
        }
        match self.error() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for PresenterWorker {
    fn drop(&mut self) {
        self.mailbox.close();
        // Do not block an unwinding compositor on terminal I/O. A normal
        // shutdown must call `shutdown`; dropping detaches the worker.
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use super::*;
    use crate::{
        kitty::{GraphicsTransport, TransferOptions, ZlibPolicy},
        Rect,
    };

    struct SlowWriter;
    impl Write for SlowWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            thread::sleep(Duration::from_millis(2));
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct RecordingWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for RecordingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn control_writes_wake_an_idle_presenter() {
        let writer = RecordingWriter::default();
        let recorded = writer.0.clone();
        let worker = PresenterWorker::spawn(
            writer,
            KittyPresenter::new(1, false),
            Placement::new(0, 0, 2, 2).unwrap(),
        );
        worker.write_control(b"clipboard".to_vec()).unwrap();
        worker.shutdown().unwrap();
        assert_eq!(&*recorded.lock().unwrap(), b"clipboard");
    }

    #[test]
    fn slow_terminal_does_not_grow_the_mailbox() {
        let mut presenter = KittyPresenter::new(1, false);
        presenter.set_transfer_options(TransferOptions {
            transport: GraphicsTransport::Direct,
            zlib: ZlibPolicy::Never,
            chunk_size: 4096,
        });
        let worker =
            PresenterWorker::spawn(SlowWriter, presenter, Placement::new(0, 0, 2, 2).unwrap());
        for serial in 0..20 {
            worker
                .submit(
                    Frame::rgb(
                        serial,
                        2,
                        2,
                        6,
                        Arc::<[u8]>::from(vec![0; 12]),
                        vec![Rect::full(2, 2)],
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        assert!(worker.dropped() > 0);
        worker.shutdown().unwrap();
    }
}
