use std::{io, sync::Arc, time::Duration};

use crate::{
    clipboard::osc52,
    input::EventReader,
    kitty::{probe_terminal_with_events, GraphicsCapabilities, KittyPresenter, Placement},
    Event, Frame, PresentationObserver, PresenterWorker, TerminalCapabilities, TerminalSession,
};

/// Configures terminal discovery and graphics resources for [`Runtime::enter`].
///
/// When [`Self::probe_timeout`] is set, startup actively verifies baseline
/// graphics, local transfer media, animation updates, and logical DPI. All
/// queries share one deadline, so the timeout bounds the probe phase as a whole
/// rather than applying independently to each query. A longer timeout tolerates
/// slower terminals and intermediaries but delays startup when replies are
/// missing; a shorter timeout can produce false negatives.
///
/// The default configuration allows 500 milliseconds for probing, requires
/// graphics support, and uses Kitty image identifier 1. Setting
/// [`Self::probe_timeout`] to `None` avoids probe latency but leaves optional
/// features disabled and infers baseline graphics support only from Kitty's
/// environment marker. Setting [`Self::require_graphics`] to `false` permits
/// startup after graphics detection fails, but does not provide a fallback
/// presenter.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeConfig {
    /// Kitty image identifier; zero is normalized to one.
    pub image_id: u32,
    /// Total time allowed for all active terminal queries, or `None` to skip them.
    pub probe_timeout: Option<Duration>,
    /// Whether startup fails if baseline Kitty graphics support is not detected.
    pub require_graphics: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            image_id: 1,
            probe_timeout: Some(Duration::from_millis(500)),
            require_graphics: true,
        }
    }
}

/// Owns terminal input, presentation output, and terminal-mode restoration.
///
/// `Runtime` is the intended top-level Tilcayo interface. It performs capability
/// discovery before starting the sole input reader, serializes framebuffer and
/// control output through the presenter worker, and restores terminal modes on
/// shutdown. Call [`Runtime::shutdown`] on normal exit so pending terminal output
/// is joined before restoration.
pub struct Runtime {
    capabilities: TerminalCapabilities,
    graphics: GraphicsCapabilities,
    placement: Placement,
    input: Option<EventReader>,
    presenter: Option<PresenterWorker>,
    session: Option<TerminalSession>,
}

impl Runtime {
    /// Configures the terminal and starts input and presentation workers.
    pub fn enter(config: RuntimeConfig) -> io::Result<Self> {
        Self::enter_instrumented(config, Arc::new(()))
    }

    /// Configures the runtime with a presentation observer for instrumentation.
    pub fn enter_instrumented<O>(config: RuntimeConfig, observer: Arc<O>) -> io::Result<Self>
    where
        O: PresentationObserver,
    {
        let mut capabilities = TerminalCapabilities::detect()?;
        let mut session = TerminalSession::enter_for_probe()?;

        let (graphics, pending_events) = if let Some(timeout) = config.probe_timeout {
            let mut input = io::stdin().lock();
            let mut output = io::stdout().lock();
            let (probe, events) = probe_terminal_with_events(&mut input, &mut output, timeout)?;
            capabilities.logical_dpi = probe.logical_dpi;
            (probe.graphics, events)
        } else {
            (
                GraphicsCapabilities {
                    graphics: capabilities.kitty_graphics,
                    shared_memory: false,
                    temporary_file: false,
                    animation: false,
                    transient: false,
                },
                Vec::new(),
            )
        };
        capabilities.kitty_graphics = graphics.graphics;
        capabilities.size = crate::TerminalSize::current()?;
        if config.require_graphics && !graphics.graphics {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Kitty graphics protocol was not detected",
            ));
        }

        session.enable_input()?;
        let placement = Placement::new(0, 0, capabilities.size.columns, capabilities.size.rows)?;
        let presenter = PresenterWorker::spawn_instrumented(
            io::stdout(),
            KittyPresenter::probed(
                config.image_id,
                graphics.animation,
                graphics.transient,
                graphics.shared_memory,
                graphics.temporary_file,
            ),
            placement,
            observer,
        );
        let input = match EventReader::spawn_with_events(pending_events) {
            Ok(input) => input,
            Err(error) => {
                let _ = presenter.shutdown();
                return Err(error);
            }
        };

        Ok(Self {
            capabilities,
            graphics,
            placement,
            input: Some(input),
            presenter: Some(presenter),
            session: Some(session),
        })
    }

    /// Returns the detected terminal capabilities and geometry.
    pub fn capabilities(&self) -> TerminalCapabilities {
        self.capabilities
    }

    /// Returns actively verified Kitty graphics features.
    pub fn graphics_capabilities(&self) -> GraphicsCapabilities {
        self.graphics
    }

    /// Returns the current image placement.
    pub fn placement(&self) -> Placement {
        self.placement
    }

    /// Submits a frame without blocking on terminal output.
    ///
    /// Returns the frame if the presentation mailbox is closed.
    pub fn submit(&self, frame: Frame) -> Result<(), Frame> {
        self.presenter
            .as_ref()
            .expect("runtime presenter unavailable")
            .submit(frame)
    }

    /// Changes the placement used by subsequently presented frames.
    pub fn set_placement(&mut self, placement: Placement) {
        self.placement = placement;
        self.presenter
            .as_ref()
            .expect("runtime presenter unavailable")
            .set_placement(placement);
    }

    /// Queues an OSC 52 host-clipboard update.
    ///
    /// The caller is responsible for enforcing an appropriate size limit.
    pub fn set_clipboard(&self, bytes: &[u8]) -> io::Result<()> {
        self.write_control(osc52(bytes))
    }

    /// Queues terminal control bytes on the serialized presentation thread.
    ///
    /// A pending control write may be replaced by a newer one.
    pub fn write_control(&self, bytes: Vec<u8>) -> io::Result<()> {
        self.presenter
            .as_ref()
            .expect("runtime presenter unavailable")
            .write_control(bytes)
    }

    /// Returns the next queued event without blocking.
    pub fn try_event(&self) -> io::Result<Option<Event>> {
        self.input
            .as_ref()
            .expect("runtime input unavailable")
            .try_recv()
    }

    /// Blocks until the next terminal event is available.
    pub fn next_event(&self) -> io::Result<Event> {
        self.input
            .as_ref()
            .expect("runtime input unavailable")
            .recv()
    }

    /// Serial of the newest frame whose complete terminal write succeeded.
    /// This is not a display acknowledgement from the terminal.
    pub fn written_serial(&self) -> u64 {
        self.presenter
            .as_ref()
            .expect("runtime presenter unavailable")
            .presented_serial()
    }

    /// Returns the number of pending frames replaced by newer submissions.
    pub fn dropped(&self) -> u64 {
        self.presenter
            .as_ref()
            .expect("runtime presenter unavailable")
            .dropped()
    }

    /// Returns a copy of the terminal-output error that stopped the presenter.
    pub fn output_error(&self) -> Option<io::Error> {
        self.presenter
            .as_ref()
            .expect("runtime presenter unavailable")
            .error()
    }

    /// Stops and joins workers before restoring terminal modes.
    pub fn shutdown(mut self) -> io::Result<()> {
        if let Some(mut input) = self.input.take() {
            input.shutdown()?;
        }
        let result = match self.presenter.take() {
            Some(presenter) => presenter.shutdown(),
            None => Ok(()),
        };
        self.session.take();
        result
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(mut input) = self.input.take() {
            let _ = input.shutdown();
        }
        // PresenterWorker intentionally detaches on exceptional/drop shutdown so
        // unwinding cannot hang forever on terminal backpressure. Applications
        // should use `shutdown` for ordered output completion and restoration.
        self.presenter.take();
        self.session.take();
    }
}
