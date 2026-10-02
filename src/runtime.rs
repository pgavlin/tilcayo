use std::{io, sync::Arc, time::Duration};

use crate::{
    clipboard::osc52,
    input::EventReader,
    kitty::{probe, GraphicsCapabilities, KittyPresenter, Placement},
    Event, Frame, PresentationObserver, PresenterWorker, TerminalCapabilities, TerminalSession,
};

/// Terminal modes and graphics resources configured by [`Runtime::enter`].
#[derive(Clone, Copy, Debug)]
pub struct RuntimeConfig {
    /// Kitty image identifier. Zero is normalized to one.
    pub image_id: u32,
    /// Actively query Kitty graphics support for this long after entering raw mode.
    /// Set to `None` to rely on conservative environment-based detection.
    pub probe_timeout: Option<Duration>,
    /// Fail startup when Kitty graphics support is not detected.
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
    pub fn enter(config: RuntimeConfig) -> io::Result<Self> {
        Self::enter_instrumented(config, Arc::new(()))
    }

    pub fn enter_instrumented<O>(config: RuntimeConfig, observer: Arc<O>) -> io::Result<Self>
    where
        O: PresentationObserver,
    {
        let mut capabilities = TerminalCapabilities::detect()?;
        let session = TerminalSession::enter()?;

        let graphics = if let Some(timeout) = config.probe_timeout {
            let mut input = io::stdin().lock();
            let mut output = io::stdout().lock();
            probe(&mut input, &mut output, timeout)?
        } else {
            GraphicsCapabilities {
                graphics: capabilities.kitty_graphics,
                shared_memory: capabilities.kitty_graphics
                    && std::env::var_os("SSH_CONNECTION").is_none()
                    && std::env::var_os("TMUX").is_none()
                    && std::env::var_os("STY").is_none(),
                animation: capabilities.kitty_graphics,
            }
        };
        capabilities.kitty_graphics = graphics.graphics;
        if config.require_graphics && !graphics.graphics {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Kitty graphics protocol was not detected",
            ));
        }

        let placement = Placement::new(0, 0, capabilities.size.columns, capabilities.size.rows)?;
        let presenter = PresenterWorker::spawn_instrumented(
            io::stdout(),
            KittyPresenter::detected(config.image_id),
            placement,
            observer,
        );
        let input = match EventReader::spawn() {
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

    pub fn capabilities(&self) -> TerminalCapabilities {
        self.capabilities
    }

    pub fn graphics_capabilities(&self) -> GraphicsCapabilities {
        self.graphics
    }

    pub fn placement(&self) -> Placement {
        self.placement
    }

    pub fn submit(&self, frame: Frame) -> Result<(), Frame> {
        self.presenter
            .as_ref()
            .expect("runtime presenter unavailable")
            .submit(frame)
    }

    pub fn set_placement(&mut self, placement: Placement) {
        self.placement = placement;
        self.presenter
            .as_ref()
            .expect("runtime presenter unavailable")
            .set_placement(placement);
    }

    pub fn set_clipboard(&self, bytes: &[u8]) -> io::Result<()> {
        self.write_control(osc52(bytes))
    }

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

    pub fn dropped(&self) -> u64 {
        self.presenter
            .as_ref()
            .expect("runtime presenter unavailable")
            .dropped()
    }

    pub fn output_error(&self) -> Option<io::Error> {
        self.presenter
            .as_ref()
            .expect("runtime presenter unavailable")
            .error()
    }

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
