use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crossterm::event::{
    self, Event as CrosstermEvent, KeyCode as CrosstermKeyCode,
    KeyEventKind as CrosstermKeyEventKind, KeyEventState as CrosstermKeyEventState,
    KeyModifiers as CrosstermModifiers, MediaKeyCode as CrosstermMediaKeyCode,
    ModifierKeyCode as CrosstermModifierKeyCode, MouseButton as CrosstermMouseButton,
    MouseEventKind as CrosstermMouseEventKind,
};

use crate::{
    kitty::Placement, Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, MediaKeyCode,
    ModifierKeyCode, Modifiers, PointerButton, PointerEvent, PointerEventKind, TerminalSize,
};

/// Adapts one Crossterm event into Tilcayo's runtime event type.
pub(crate) fn adapt(event: CrosstermEvent) -> io::Result<Option<Event>> {
    Ok(match event {
        CrosstermEvent::Key(value) => Some(Event::Key(adapt_key(value))),
        CrosstermEvent::Mouse(value) => Some(Event::Pointer(PointerEvent {
            kind: adapt_pointer_kind(value.kind),
            x: value.column,
            y: value.row,
            modifiers: adapt_modifiers(value.modifiers),
        })),
        CrosstermEvent::FocusGained => Some(Event::Focus(true)),
        CrosstermEvent::FocusLost => Some(Event::Focus(false)),
        CrosstermEvent::Resize(columns, rows) => {
            let mut size = TerminalSize::current().unwrap_or(TerminalSize {
                columns,
                rows,
                width_px: None,
                height_px: None,
            });
            size.columns = columns;
            size.rows = rows;
            Some(Event::Resize(size))
        }
        CrosstermEvent::Paste(value) => Some(Event::Paste(value)),
    })
}

fn adapt_key(value: crossterm::event::KeyEvent) -> KeyEvent {
    KeyEvent {
        code: adapt_key_code(value.code),
        modifiers: adapt_modifiers(value.modifiers),
        kind: match value.kind {
            CrosstermKeyEventKind::Press => KeyEventKind::Press,
            CrosstermKeyEventKind::Repeat => KeyEventKind::Repeat,
            CrosstermKeyEventKind::Release => KeyEventKind::Release,
        },
        state: adapt_key_state(value.state),
    }
}

fn adapt_key_code(value: CrosstermKeyCode) -> KeyCode {
    match value {
        CrosstermKeyCode::Backspace => KeyCode::Backspace,
        CrosstermKeyCode::Enter => KeyCode::Enter,
        CrosstermKeyCode::Left => KeyCode::Left,
        CrosstermKeyCode::Right => KeyCode::Right,
        CrosstermKeyCode::Up => KeyCode::Up,
        CrosstermKeyCode::Down => KeyCode::Down,
        CrosstermKeyCode::Home => KeyCode::Home,
        CrosstermKeyCode::End => KeyCode::End,
        CrosstermKeyCode::PageUp => KeyCode::PageUp,
        CrosstermKeyCode::PageDown => KeyCode::PageDown,
        CrosstermKeyCode::Tab => KeyCode::Tab,
        CrosstermKeyCode::BackTab => KeyCode::BackTab,
        CrosstermKeyCode::Delete => KeyCode::Delete,
        CrosstermKeyCode::Insert => KeyCode::Insert,
        CrosstermKeyCode::F(number) => KeyCode::F(number),
        CrosstermKeyCode::Char(character) => KeyCode::Char(character),
        CrosstermKeyCode::Null => KeyCode::Null,
        CrosstermKeyCode::Esc => KeyCode::Esc,
        CrosstermKeyCode::CapsLock => KeyCode::CapsLock,
        CrosstermKeyCode::ScrollLock => KeyCode::ScrollLock,
        CrosstermKeyCode::NumLock => KeyCode::NumLock,
        CrosstermKeyCode::PrintScreen => KeyCode::PrintScreen,
        CrosstermKeyCode::Pause => KeyCode::Pause,
        CrosstermKeyCode::Menu => KeyCode::Menu,
        CrosstermKeyCode::KeypadBegin => KeyCode::KeypadBegin,
        CrosstermKeyCode::Media(value) => KeyCode::Media(match value {
            CrosstermMediaKeyCode::Play => MediaKeyCode::Play,
            CrosstermMediaKeyCode::Pause => MediaKeyCode::Pause,
            CrosstermMediaKeyCode::PlayPause => MediaKeyCode::PlayPause,
            CrosstermMediaKeyCode::Reverse => MediaKeyCode::Reverse,
            CrosstermMediaKeyCode::Stop => MediaKeyCode::Stop,
            CrosstermMediaKeyCode::FastForward => MediaKeyCode::FastForward,
            CrosstermMediaKeyCode::Rewind => MediaKeyCode::Rewind,
            CrosstermMediaKeyCode::TrackNext => MediaKeyCode::TrackNext,
            CrosstermMediaKeyCode::TrackPrevious => MediaKeyCode::TrackPrevious,
            CrosstermMediaKeyCode::Record => MediaKeyCode::Record,
            CrosstermMediaKeyCode::LowerVolume => MediaKeyCode::LowerVolume,
            CrosstermMediaKeyCode::RaiseVolume => MediaKeyCode::RaiseVolume,
            CrosstermMediaKeyCode::MuteVolume => MediaKeyCode::MuteVolume,
        }),
        CrosstermKeyCode::Modifier(value) => KeyCode::Modifier(match value {
            CrosstermModifierKeyCode::LeftShift => ModifierKeyCode::LeftShift,
            CrosstermModifierKeyCode::LeftControl => ModifierKeyCode::LeftControl,
            CrosstermModifierKeyCode::LeftAlt => ModifierKeyCode::LeftAlt,
            CrosstermModifierKeyCode::LeftSuper => ModifierKeyCode::LeftSuper,
            CrosstermModifierKeyCode::LeftHyper => ModifierKeyCode::LeftHyper,
            CrosstermModifierKeyCode::LeftMeta => ModifierKeyCode::LeftMeta,
            CrosstermModifierKeyCode::RightShift => ModifierKeyCode::RightShift,
            CrosstermModifierKeyCode::RightControl => ModifierKeyCode::RightControl,
            CrosstermModifierKeyCode::RightAlt => ModifierKeyCode::RightAlt,
            CrosstermModifierKeyCode::RightSuper => ModifierKeyCode::RightSuper,
            CrosstermModifierKeyCode::RightHyper => ModifierKeyCode::RightHyper,
            CrosstermModifierKeyCode::RightMeta => ModifierKeyCode::RightMeta,
            CrosstermModifierKeyCode::IsoLevel3Shift => ModifierKeyCode::IsoLevel3Shift,
            CrosstermModifierKeyCode::IsoLevel5Shift => ModifierKeyCode::IsoLevel5Shift,
        }),
    }
}

fn adapt_modifiers(value: CrosstermModifiers) -> Modifiers {
    let mut result = Modifiers::NONE;
    if value.contains(CrosstermModifiers::SHIFT) {
        result |= Modifiers::SHIFT;
    }
    if value.contains(CrosstermModifiers::CONTROL) {
        result |= Modifiers::CONTROL;
    }
    if value.contains(CrosstermModifiers::ALT) {
        result |= Modifiers::ALT;
    }
    if value.contains(CrosstermModifiers::SUPER) {
        result |= Modifiers::SUPER;
    }
    if value.contains(CrosstermModifiers::HYPER) {
        result |= Modifiers::HYPER;
    }
    if value.contains(CrosstermModifiers::META) {
        result |= Modifiers::META;
    }
    result
}

fn adapt_key_state(value: CrosstermKeyEventState) -> KeyEventState {
    let mut result = KeyEventState::NONE;
    if value.contains(CrosstermKeyEventState::KEYPAD) {
        result |= KeyEventState::KEYPAD;
    }
    if value.contains(CrosstermKeyEventState::CAPS_LOCK) {
        result |= KeyEventState::CAPS_LOCK;
    }
    if value.contains(CrosstermKeyEventState::NUM_LOCK) {
        result |= KeyEventState::NUM_LOCK;
    }
    result
}

fn adapt_pointer_kind(value: CrosstermMouseEventKind) -> PointerEventKind {
    match value {
        CrosstermMouseEventKind::Down(button) => PointerEventKind::Down(adapt_button(button)),
        CrosstermMouseEventKind::Up(button) => PointerEventKind::Up(adapt_button(button)),
        CrosstermMouseEventKind::Drag(button) => PointerEventKind::Drag(adapt_button(button)),
        CrosstermMouseEventKind::Moved => PointerEventKind::Moved,
        CrosstermMouseEventKind::ScrollDown => PointerEventKind::ScrollDown,
        CrosstermMouseEventKind::ScrollUp => PointerEventKind::ScrollUp,
        CrosstermMouseEventKind::ScrollLeft => PointerEventKind::ScrollLeft,
        CrosstermMouseEventKind::ScrollRight => PointerEventKind::ScrollRight,
    }
}

fn adapt_button(value: CrosstermMouseButton) -> PointerButton {
    match value {
        CrosstermMouseButton::Left => PointerButton::Left,
        CrosstermMouseButton::Right => PointerButton::Right,
        CrosstermMouseButton::Middle => PointerButton::Middle,
    }
}

/// The sole terminal-input consumer used by [`crate::Runtime`].
///
/// The reader polls with a short timeout so it can be stopped and joined
/// without requiring another byte of terminal input.
pub struct EventReader {
    receiver: mpsc::Receiver<io::Result<Event>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl EventReader {
    pub fn spawn() -> io::Result<Self> {
        Self::spawn_with_events(Vec::new())
    }

    pub(crate) fn spawn_with_events(initial: Vec<Event>) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        for event in initial {
            sender.send(Ok(event)).map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "terminal event queue closed")
            })?;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("tilcayo-input".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    let ready = match event::poll(Duration::from_millis(50)) {
                        Ok(ready) => ready,
                        Err(error) => {
                            let _ = sender.send(Err(error));
                            break;
                        }
                    };
                    if !ready {
                        continue;
                    }
                    let result = event::read().and_then(|event| {
                        adapt(event)?.ok_or_else(|| {
                            io::Error::new(io::ErrorKind::InvalidData, "ignored terminal event")
                        })
                    });
                    let failed = result.is_err();
                    if sender.send(result).is_err() || failed {
                        break;
                    }
                }
            })?;
        Ok(Self {
            receiver,
            stop,
            thread: Some(thread),
        })
    }

    pub fn try_recv(&self) -> io::Result<Option<Event>> {
        match self.receiver.try_recv() {
            Ok(Ok(event)) => Ok(Some(event)),
            Ok(Err(error)) => Err(error),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "terminal input reader stopped",
            )),
        }
    }

    pub fn recv(&self) -> io::Result<Event> {
        self.receiver.recv().map_err(|_| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "terminal input reader stopped",
            )
        })?
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| io::Error::other("terminal input reader panicked"))?;
        }
        Ok(())
    }
}

impl Drop for EventReader {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Maps SGR-pixel coordinates through an image placement to framebuffer pixels.
pub fn map_pixel_pointer(
    x: u16,
    y: u16,
    terminal: TerminalSize,
    placement: Placement,
    output: (u32, u32),
) -> Option<(f64, f64)> {
    if output.0 == 0 || output.1 == 0 {
        return None;
    }

    let cell_width = f64::from(terminal.width_px?) / f64::from(terminal.columns.max(1));
    let cell_height = f64::from(terminal.height_px?) / f64::from(terminal.rows.max(1));
    let placement_left = f64::from(placement.column) * cell_width;
    let placement_top = f64::from(placement.row) * cell_height;
    let placement_width = f64::from(placement.columns) * cell_width;
    let placement_height = f64::from(placement.rows) * cell_height;

    // When both placement dimensions are specified, Kitty preserves the
    // source aspect ratio and centers it with letterboxing or pillarboxing.
    let scale = (placement_width / f64::from(output.0)).min(placement_height / f64::from(output.1));
    let image_width = f64::from(output.0) * scale;
    let image_height = f64::from(output.1) * scale;
    let image_left = placement_left + (placement_width - image_width) / 2.0;
    let image_top = placement_top + (placement_height - image_height) / 2.0;
    let (x, y) = (f64::from(x), f64::from(y));

    if x < image_left
        || y < image_top
        || x >= image_left + image_width
        || y >= image_top + image_height
    {
        return None;
    }
    Some(((x - image_left) / scale, (y - image_top) / scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapts_crossterm_events_to_owned_types() {
        let key = crossterm::event::KeyEvent::new_with_kind_and_state(
            CrosstermKeyCode::Char('x'),
            CrosstermModifiers::CONTROL | CrosstermModifiers::ALT,
            CrosstermKeyEventKind::Repeat,
            CrosstermKeyEventState::CAPS_LOCK,
        );
        assert_eq!(
            adapt(CrosstermEvent::Key(key)).unwrap(),
            Some(Event::Key(KeyEvent {
                code: KeyCode::Char('x'),
                modifiers: Modifiers::CONTROL | Modifiers::ALT,
                kind: KeyEventKind::Repeat,
                state: KeyEventState::CAPS_LOCK,
            }))
        );

        let pointer = crossterm::event::MouseEvent {
            kind: CrosstermMouseEventKind::Drag(CrosstermMouseButton::Left),
            column: 12,
            row: 34,
            modifiers: CrosstermModifiers::SHIFT,
        };
        assert_eq!(
            adapt(CrosstermEvent::Mouse(pointer)).unwrap(),
            Some(Event::Pointer(PointerEvent {
                kind: PointerEventKind::Drag(PointerButton::Left),
                x: 12,
                y: 34,
                modifiers: Modifiers::SHIFT,
            }))
        );
    }

    #[test]
    fn maps_and_rejects_pixel_mouse_coordinates() {
        let terminal = TerminalSize {
            columns: 100,
            rows: 50,
            width_px: Some(1000),
            height_px: Some(500),
        };
        let placement = Placement::new(10, 5, 80, 40).unwrap();
        assert_eq!(
            map_pixel_pointer(500, 250, terminal, placement, (800, 400)),
            Some((400.0, 200.0))
        );
        assert_eq!(
            map_pixel_pointer(1, 1, terminal, placement, (800, 400)),
            None
        );
    }

    #[test]
    fn accounts_for_placement_letterboxing() {
        let terminal = TerminalSize {
            columns: 20,
            rows: 10,
            width_px: Some(200),
            height_px: Some(100),
        };
        let placement = Placement::new(0, 0, 20, 10).unwrap();

        // A square image is centered in the 2:1 placement with 50-pixel bars.
        assert_eq!(
            map_pixel_pointer(49, 50, terminal, placement, (100, 100)),
            None
        );
        assert_eq!(
            map_pixel_pointer(50, 50, terminal, placement, (100, 100)),
            Some((0.0, 50.0))
        );
        assert_eq!(
            map_pixel_pointer(100, 50, terminal, placement, (100, 100)),
            Some((50.0, 50.0))
        );
        assert_eq!(
            map_pixel_pointer(150, 50, terminal, placement, (100, 100)),
            None
        );
    }
}
