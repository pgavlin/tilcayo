use std::io;

use crossterm::terminal;

/// The terminal viewport dimensions in cells and, when available, pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    /// Number of character-cell columns.
    pub columns: u16,
    /// Number of character-cell rows.
    pub rows: u16,
    /// Viewport width in pixels, if reported by the terminal.
    pub width_px: Option<u16>,
    /// Viewport height in pixels, if reported by the terminal.
    pub height_px: Option<u16>,
}

impl TerminalSize {
    /// Queries the current terminal viewport size.
    pub fn current() -> io::Result<Self> {
        let (columns, rows) = terminal::size()?;
        let pixels = terminal::window_size()
            .ok()
            .filter(|size| size.width > 0 && size.height > 0);
        Ok(Self {
            columns,
            rows,
            width_px: pixels.as_ref().map(|size| size.width),
            height_px: pixels.as_ref().map(|size| size.height),
        })
    }
}

/// Logical dots per inch reported by the terminal for its active window.
///
/// This reflects UI/font scaling, not necessarily the monitor's physical DPI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LogicalDpi {
    x: f64,
    y: f64,
}

impl LogicalDpi {
    /// Creates a logical-DPI value when both axes are finite and in the
    /// supported range of 1 through 1000 DPI.
    pub fn new(x: f64, y: f64) -> Option<Self> {
        (valid_dpi(x) && valid_dpi(y)).then_some(Self { x, y })
    }

    /// Returns the horizontal logical DPI.
    pub fn x(self) -> f64 {
        self.x
    }

    /// Returns the vertical logical DPI.
    pub fn y(self) -> f64 {
        self.y
    }
}

// Construction rejects NaN, so LogicalDpi's equality is reflexive.
impl Eq for LogicalDpi {}

fn valid_dpi(value: f64) -> bool {
    value.is_finite() && (1.0..=1000.0).contains(&value)
}

/// Terminal features and geometry known to Tilcayo.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalCapabilities {
    /// Whether Kitty graphics support was detected.
    pub kitty_graphics: bool,
    /// Whether Kitty keyboard enhancements are supported.
    pub kitty_keyboard: bool,
    /// Whether Tilcayo requests pixel-coordinate mouse reporting.
    pub pixel_mouse_requested: bool,
    /// Logical DPI reported by Kitty, when actively probed.
    pub logical_dpi: Option<LogicalDpi>,
    /// Current terminal viewport size.
    pub size: TerminalSize,
}

impl TerminalCapabilities {
    /// Conservative discovery without consuming terminal input. Graphics is
    /// inferred only from Kitty's environment marker; active queries belong to
    /// an isolated startup phase once their replies are exposed by our parser.
    pub fn detect() -> io::Result<Self> {
        Ok(Self {
            kitty_graphics: std::env::var_os("KITTY_WINDOW_ID").is_some(),
            kitty_keyboard: terminal::supports_keyboard_enhancement().unwrap_or(false),
            pixel_mouse_requested: true,
            logical_dpi: None,
            size: TerminalSize::current()?,
        })
    }
}
