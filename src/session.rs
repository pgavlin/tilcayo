use std::io;

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
}

impl TerminalSession {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let result = (|| {
            let keyboard_enhancement =
                crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
            let mut stdout = io::stdout();
            execute!(
                stdout,
                EnterAlternateScreen,
                Hide,
                EnableMouseCapture,
                EnableFocusChange,
                EnableBracketedPaste
            )?;
            // SGR-Pixels mouse mode. Crossterm remains the sole event parser.
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
            Ok(Self {
                keyboard_enhancement,
            })
        })();
        if result.is_err() {
            restore(false);
        }
        result
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        restore(self.keyboard_enhancement);
    }
}

fn restore(keyboard_enhancement: bool) {
    use io::Write;
    let mut stdout = io::stdout();
    let _ = stdout.write_all(b"\x1b[?1016l");
    if keyboard_enhancement {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        stdout,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture,
        Show,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
}
