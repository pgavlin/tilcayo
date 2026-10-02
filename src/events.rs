use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};

use crate::TerminalSize;

/// An input or geometry event produced by the host terminal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    Key(KeyEvent),
    Pointer(PointerEvent),
    Focus(bool),
    Resize(TerminalSize),
    Paste(String),
}

/// A keyboard event.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: Modifiers,
    pub kind: KeyEventKind,
    pub state: KeyEventState,
}

impl KeyEvent {
    pub const fn new(code: KeyCode, modifiers: Modifiers) -> Self {
        Self {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    pub const fn new_with_kind(code: KeyCode, modifiers: Modifiers, kind: KeyEventKind) -> Self {
        Self {
            code,
            modifiers,
            kind,
            state: KeyEventState::NONE,
        }
    }
}

/// The phase of a keyboard event.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum KeyEventKind {
    Press,
    Repeat,
    Release,
}

/// A key reported by the terminal.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum KeyCode {
    Backspace,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Delete,
    Insert,
    F(u8),
    Char(char),
    Null,
    Esc,
    CapsLock,
    ScrollLock,
    NumLock,
    PrintScreen,
    Pause,
    Menu,
    KeypadBegin,
    Media(MediaKeyCode),
    Modifier(ModifierKeyCode),
    /// A protocol key value that this version of Tilcayo does not identify.
    Unidentified(u32),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MediaKeyCode {
    Play,
    Pause,
    PlayPause,
    Reverse,
    Stop,
    FastForward,
    Rewind,
    TrackNext,
    TrackPrevious,
    Record,
    LowerVolume,
    RaiseVolume,
    MuteVolume,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ModifierKeyCode {
    LeftShift,
    LeftControl,
    LeftAlt,
    LeftSuper,
    LeftHyper,
    LeftMeta,
    RightShift,
    RightControl,
    RightAlt,
    RightSuper,
    RightHyper,
    RightMeta,
    IsoLevel3Shift,
    IsoLevel5Shift,
}

/// A pointer event. Coordinates are reported in the terminal's active mouse
/// coordinate space; Tilcayo requests SGR-Pixels mode for runtime sessions.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PointerEvent {
    pub kind: PointerEventKind,
    pub x: u16,
    pub y: u16,
    pub modifiers: Modifiers,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PointerEventKind {
    Down(PointerButton),
    Up(PointerButton),
    Drag(PointerButton),
    Moved,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PointerButton {
    Left,
    Right,
    Middle,
    Other(u16),
}

macro_rules! flags {
    ($name:ident, $bits:ty, $( $flag:ident = $value:expr ),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
        pub struct $name($bits);

        impl $name {
            pub const NONE: Self = Self(0);
            $(pub const $flag: Self = Self($value);)+

            pub const fn from_bits_retain(bits: $bits) -> Self {
                Self(bits)
            }

            pub const fn bits(self) -> $bits {
                self.0
            }

            pub const fn is_empty(self) -> bool {
                self.0 == 0
            }

            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }

            pub const fn intersects(self, other: Self) -> bool {
                self.0 & other.0 != 0
            }
        }

        impl BitOr for $name {
            type Output = Self;
            fn bitor(self, rhs: Self) -> Self {
                Self(self.0 | rhs.0)
            }
        }

        impl BitOrAssign for $name {
            fn bitor_assign(&mut self, rhs: Self) {
                self.0 |= rhs.0;
            }
        }

        impl BitAnd for $name {
            type Output = Self;
            fn bitand(self, rhs: Self) -> Self {
                Self(self.0 & rhs.0)
            }
        }

        impl BitAndAssign for $name {
            fn bitand_assign(&mut self, rhs: Self) {
                self.0 &= rhs.0;
            }
        }

        impl Not for $name {
            type Output = Self;
            fn not(self) -> Self {
                Self(!self.0)
            }
        }
    };
}

flags!(
    Modifiers,
    u8,
    SHIFT = 1 << 0,
    CONTROL = 1 << 1,
    ALT = 1 << 2,
    SUPER = 1 << 3,
    HYPER = 1 << 4,
    META = 1 << 5,
);

flags!(
    KeyEventState,
    u8,
    KEYPAD = 1 << 0,
    CAPS_LOCK = 1 << 1,
    NUM_LOCK = 1 << 2,
);
