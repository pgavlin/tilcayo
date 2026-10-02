use std::io;

use crossterm::terminal;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    pub columns: u16,
    pub rows: u16,
    pub width_px: Option<u16>,
    pub height_px: Option<u16>,
}

impl TerminalSize {
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
    pub fn new(x: f64, y: f64) -> Option<Self> {
        (valid_dpi(x) && valid_dpi(y)).then_some(Self { x, y })
    }

    pub fn x(self) -> f64 {
        self.x
    }

    pub fn y(self) -> f64 {
        self.y
    }
}

// Construction rejects NaN, so LogicalDpi's equality is reflexive.
impl Eq for LogicalDpi {}

fn valid_dpi(value: f64) -> bool {
    value.is_finite() && (1.0..=1000.0).contains(&value)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalCapabilities {
    pub kitty_graphics: bool,
    pub kitty_keyboard: bool,
    pub pixel_mouse_requested: bool,
    pub logical_dpi: Option<LogicalDpi>,
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
