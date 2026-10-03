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
    Frame, LatestFrameMailbox, Presentation, Wakeup,
};

/// Receives low-overhead notifications from a [`PresenterWorker`].
///
/// Implementations must not block the presentation thread. The default `()`
/// observer ignores all notifications. A panic from an observer stops the
/// worker; a successfully written frame is published as complete before its
/// [`Self::presented`] callback, and [`PresenterWorker::shutdown`] reports the
/// worker panic.
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

/// Serializes framebuffer presentation and terminal control output on a
/// dedicated thread.
///
/// Frame submission does not wait for terminal I/O, and only the newest pending
/// frame is retained. Presentation and control writes are serialized, and only
/// the newest pending control write is retained.
///
/// An output failure stops the worker and is available through [`Self::error`].
/// Call [`Self::shutdown`] to join the thread and receive its final error.
/// Dropping the worker requests closure but does not wait for the thread.
pub struct PresenterWorker {
    mailbox: LatestFrameMailbox,
    error: Arc<Mutex<Option<io::Error>>>,
    presented: Arc<AtomicU64>,
    completion: Arc<Mutex<CompletionState>>,
    placement: Arc<Mutex<Placement>>,
    commands: Arc<Mutex<VecDeque<Vec<u8>>>>,
    observer: Arc<dyn PresentationObserver>,
    wakeup: Wakeup,
    thread: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct CompletionState {
    latest: Option<u64>,
    stopped: bool,
}

struct WorkerExit {
    completion: Arc<Mutex<CompletionState>>,
    wakeup: Wakeup,
}

impl Drop for WorkerExit {
    fn drop(&mut self) {
        self.completion
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .stopped = true;
        self.wakeup.signal();
    }
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
        writer: W,
        presenter: KittyPresenter,
        placement: Placement,
        observer: Arc<O>,
    ) -> Self
    where
        W: Write + Send + 'static,
        O: PresentationObserver,
    {
        let wakeup = Wakeup::new().expect("create presenter wakeup");
        Self::spawn_instrumented_with_wakeup(writer, presenter, placement, observer, wakeup)
    }

    pub(crate) fn spawn_instrumented_with_wakeup<W, O>(
        mut writer: W,
        mut presenter: KittyPresenter,
        placement: Placement,
        observer: Arc<O>,
        wakeup: Wakeup,
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
        let completion = Arc::new(Mutex::new(CompletionState::default()));
        let worker_completion = completion.clone();
        let placement = Arc::new(Mutex::new(placement));
        let commands = Arc::new(Mutex::new(VecDeque::<Vec<u8>>::new()));
        let worker_commands = commands.clone();
        let worker_observer = observer.clone();
        let worker_wakeup = wakeup.clone();
        let thread = thread::Builder::new()
            .name("tilcayo-presenter".into())
            .spawn(move || {
                let _exit = WorkerExit {
                    completion: worker_completion.clone(),
                    wakeup: worker_wakeup.clone(),
                };
                loop {
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
                        if let MailboxEvent::Presentation(presentation) = event {
                            let (frame, placement) = presentation.into_parts();
                            let serial = frame.serial();
                            let present_started = Instant::now();
                            let stats = presenter.present(&mut writer, &frame, placement)?;
                            let completed = Instant::now();
                            worker_presented.store(serial, Ordering::Release);
                            worker_completion
                                .lock()
                                .expect("presenter completion lock poisoned")
                                .latest = Some(serial);
                            worker_wakeup.signal();
                            worker_observer.presented(
                                stats,
                                completed.duration_since(present_started),
                                completed.duration_since(frame.produced_at),
                            );
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
                }
            })
            .expect("spawn presenter thread");
        Self {
            mailbox,
            error,
            presented,
            completion,
            placement,
            commands,
            observer,
            wakeup,
            thread: Some(thread),
        }
    }

    /// Submits a frame without blocking on terminal output.
    ///
    /// Returns the frame if the mailbox is closed.
    pub fn submit(&self, frame: Frame) -> Result<(), Frame> {
        let placement = *self
            .placement
            .lock()
            .expect("presenter placement lock poisoned");
        self.submit_presentation(Presentation::new(frame, placement))
            .map_err(|presentation| presentation.into_parts().0)
    }

    /// Submits a frame and its already captured placement without blocking.
    ///
    /// Returns the request if the mailbox is closed. Replacing a pending
    /// request carries its damage into this request while retaining this
    /// request's placement.
    pub fn submit_presentation(&self, presentation: Presentation) -> Result<(), Presentation> {
        let result = self.mailbox.submit(presentation);
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
        if self.error().is_some() || self.is_stopped() {
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

    /// Updates the default placement captured by future [`Self::submit`] calls.
    ///
    /// Requests already in the mailbox retain their previous placement.
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

    /// Takes the newest unconsumed successfully written frame serial.
    ///
    /// Multiple completions may be coalesced; the newest serial is retained.
    /// A completion means the complete terminal write succeeded, not that the
    /// terminal displayed or acknowledged the frame.
    pub fn take_completion(&self) -> Option<u64> {
        self.completion
            .lock()
            .expect("presenter completion lock poisoned")
            .latest
            .take()
    }

    /// Returns whether the presentation thread has stopped.
    pub fn is_stopped(&self) -> bool {
        self.completion
            .lock()
            .expect("presenter completion lock poisoned")
            .stopped
    }

    /// Returns the pollable handle signaled by completions and worker exit.
    pub fn wakeup(&self) -> &Wakeup {
        &self.wakeup
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
    use std::{
        sync::{mpsc, Arc},
        time::Duration,
    };

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

    struct BlockingWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
        started: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
    }

    impl Write for BlockingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(started) = self.started.take() {
                started.send(()).unwrap();
                self.release.recv().unwrap();
            }
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn test_frame(serial: u64) -> Frame {
        Frame::rgb(
            serial,
            2,
            2,
            6,
            Arc::<[u8]>::from(vec![serial as u8; 12]),
            vec![Rect::full(2, 2)],
        )
        .unwrap()
    }

    #[test]
    fn pending_frame_retains_placement_captured_at_submission() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let writer = BlockingWriter {
            bytes: bytes.clone(),
            started: Some(started_sender),
            release: release_receiver,
        };
        let mut presenter = KittyPresenter::new(1, false);
        presenter.set_transfer_options(TransferOptions {
            transport: GraphicsTransport::Direct,
            zlib: ZlibPolicy::Never,
            chunk_size: 4096,
        });
        let worker = PresenterWorker::spawn(writer, presenter, Placement::new(0, 0, 2, 2).unwrap());
        worker.submit(test_frame(1)).unwrap();
        started_receiver.recv().unwrap();

        worker.set_placement(Placement::new(1, 0, 2, 2).unwrap());
        worker.submit(test_frame(2)).unwrap();
        worker.set_placement(Placement::new(2, 0, 2, 2).unwrap());
        release_sender.send(()).unwrap();
        worker.shutdown().unwrap();

        let output = String::from_utf8_lossy(&bytes.lock().unwrap()).into_owned();
        assert!(output.contains("\u{1b}[1;2H\u{1b}_Ga=p"));
        assert!(!output.contains("\u{1b}[1;3H\u{1b}_Ga=p"));
    }

    #[test]
    fn completion_is_pollable_after_the_full_write() {
        let worker = PresenterWorker::spawn(
            RecordingWriter::default(),
            KittyPresenter::new(1, false),
            Placement::new(0, 0, 2, 2).unwrap(),
        );
        worker.submit(test_frame(7)).unwrap();
        assert!(worker.wakeup().wait(Some(Duration::from_secs(1))).unwrap());
        worker.wakeup().clear().unwrap();
        assert_eq!(worker.take_completion(), Some(7));
        worker.shutdown().unwrap();
    }

    struct PanickingObserver;

    impl PresentationObserver for PanickingObserver {
        fn presented(&self, _stats: PresentStats, _elapsed: Duration, _pipeline: Duration) {
            panic!("observer failed");
        }
    }

    #[test]
    fn observer_panic_still_publishes_completion_and_exit() {
        let worker = PresenterWorker::spawn_instrumented(
            RecordingWriter::default(),
            KittyPresenter::new(1, false),
            Placement::new(0, 0, 2, 2).unwrap(),
            Arc::new(PanickingObserver),
        );
        worker.submit(test_frame(9)).unwrap();
        assert!(worker.wakeup().wait(Some(Duration::from_secs(1))).unwrap());
        assert_eq!(worker.take_completion(), Some(9));
        while !worker.is_stopped() {
            worker.wakeup().clear().unwrap();
            assert!(worker.wakeup().wait(Some(Duration::from_secs(1))).unwrap());
        }
        assert!(worker.shutdown().is_err());
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
