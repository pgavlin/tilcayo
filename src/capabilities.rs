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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalCapabilities {
    pub kitty_graphics: bool,
    pub kitty_keyboard: bool,
    pub pixel_mouse_requested: bool,
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
            size: TerminalSize::current()?,
        })
    }
}
