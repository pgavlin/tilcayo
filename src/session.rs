use std::io::{self, Write};

use crossterm::{
    cursor::{Hide, Show},
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
/// Restores every terminal mode that Tilcayo enables when dropped.
pub struct TerminalSession {
    keyboard_enhancement: bool,
    input_enabled: bool,
}

impl TerminalSession {
    /// Enters raw mode and the alternate screen, hides the cursor, and enables
    /// mouse, focus, paste, and supported keyboard-enhancement reporting.
    pub fn enter() -> io::Result<Self> {
        let mut session = Self::enter_for_probe()?;
        session.enable_input()?;
        Ok(session)
    }

    pub(crate) fn enter_for_probe() -> io::Result<Self> {
        enable_raw_mode()?;
        let result = (|| {
            let mut stdout = io::stdout();
            execute!(stdout, EnterAlternateScreen, Hide)?;
            stdout.flush()?;
            Ok(Self {
                keyboard_enhancement: false,
                input_enabled: false,
            })
        })();
        if result.is_err() {
            restore(false, false);
        }
        result
    }

    /// Enable input-reporting modes after startup probing has finished, so
    /// probe-time input uses the terminal's unenhanced keyboard encoding.
    pub(crate) fn enable_input(&mut self) -> io::Result<()> {
        let keyboard_enhancement =
            crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
        self.keyboard_enhancement = keyboard_enhancement;
        self.input_enabled = true;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnableMouseCapture,
            EnableFocusChange,
            EnableBracketedPaste
        )?;
        // SGR-Pixels mouse mode. Crossterm remains the normal event parser.
        use io::Write;
        stdout.write_all(b"\x1b[?1016h")?;
        if keyboard_enhancement {
            execute!(
                stdout,
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                )
            )?;
        }
        stdout.flush()?;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        restore(self.keyboard_enhancement, self.input_enabled);
    }
}

fn restore(keyboard_enhancement: bool, input_enabled: bool) {
    use io::Write;
    let mut stdout = io::stdout();
    if input_enabled {
        let _ = stdout.write_all(b"\x1b[?1016l");
        if keyboard_enhancement {
            let _ = execute!(stdout, PopKeyboardEnhancementFlags);
        }
        let _ = execute!(
            stdout,
            DisableBracketedPaste,
            DisableFocusChange,
            DisableMouseCapture
        );
    }
    let _ = execute!(stdout, Show, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}
