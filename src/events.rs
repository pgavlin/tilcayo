use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};

use crate::TerminalSize;

/// An input or geometry event produced by the host terminal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    /// A keyboard key changed state.
    Key(KeyEvent),
    /// A mouse or other pointer event occurred.
    Pointer(PointerEvent),
    /// Terminal focus changed; `true` indicates focus was gained.
    Focus(bool),
    /// The terminal viewport was resized.
    Resize(TerminalSize),
    /// Text was received through bracketed paste.
    Paste(String),
}

/// A keyboard event.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct KeyEvent {
    /// Logical key reported by the terminal.
    pub code: KeyCode,
    /// Modifier keys active for this event.
    pub modifiers: Modifiers,
    /// Press, repeat, or release phase.
    pub kind: KeyEventKind,
    /// Lock and keypad state associated with the event.
    pub state: KeyEventState,
}

impl KeyEvent {
    /// Creates a key-press event with no additional state flags.
    pub const fn new(code: KeyCode, modifiers: Modifiers) -> Self {
        Self {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    /// Creates an event with the given phase and no additional state flags.
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
    /// Initial key press.
    Press,
    /// Automatic or explicit repeated press.
    Repeat,
    /// Key release.
    Release,
}

/// A key reported by the terminal.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum KeyCode {
    /// Backspace key.
    Backspace,
    /// Enter or return key.
    Enter,
    /// Left arrow key.
    Left,
    /// Right arrow key.
    Right,
    /// Up arrow key.
    Up,
    /// Down arrow key.
    Down,
    /// Home key.
    Home,
    /// End key.
    End,
    /// Page Up key.
    PageUp,
    /// Page Down key.
    PageDown,
    /// Tab key.
    Tab,
    /// Reverse-tab key, commonly Shift+Tab.
    BackTab,
    /// Delete key.
    Delete,
    /// Insert key.
    Insert,
    /// Function key with its one-based number.
    F(u8),
    /// Unicode character key.
    Char(char),
    /// Null key value.
    Null,
    /// Escape key.
    Esc,
    /// Caps Lock key.
    CapsLock,
    /// Scroll Lock key.
    ScrollLock,
    /// Num Lock key.
    NumLock,
    /// Print Screen key.
    PrintScreen,
    /// Pause key.
    Pause,
    /// Menu key.
    Menu,
    /// Keypad Begin key.
    KeypadBegin,
    /// Media-control key.
    Media(MediaKeyCode),
    /// A physical modifier key.
    Modifier(ModifierKeyCode),
    /// A protocol key value that this version of Tilcayo does not identify.
    Unidentified(u32),
}

/// A media-control key.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MediaKeyCode {
    /// Start playback.
    Play,
    /// Pause playback.
    Pause,
    /// Toggle playback and pause.
    PlayPause,
    /// Reverse playback.
    Reverse,
    /// Stop playback.
    Stop,
    /// Fast-forward playback.
    FastForward,
    /// Rewind playback.
    Rewind,
    /// Select the next track.
    TrackNext,
    /// Select the previous track.
    TrackPrevious,
    /// Start recording.
    Record,
    /// Lower audio volume.
    LowerVolume,
    /// Raise audio volume.
    RaiseVolume,
    /// Mute audio volume.
    MuteVolume,
}

/// A physical modifier key, including its side where available.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ModifierKeyCode {
    /// Left Shift key.
    LeftShift,
    /// Left Control key.
    LeftControl,
    /// Left Alt key.
    LeftAlt,
    /// Left Super key.
    LeftSuper,
    /// Left Hyper key.
    LeftHyper,
    /// Left Meta key.
    LeftMeta,
    /// Right Shift key.
    RightShift,
    /// Right Control key.
    RightControl,
    /// Right Alt key.
    RightAlt,
    /// Right Super key.
    RightSuper,
    /// Right Hyper key.
    RightHyper,
    /// Right Meta key.
    RightMeta,
    /// ISO level-3 shift key, often AltGr.
    IsoLevel3Shift,
    /// ISO level-5 shift key.
    IsoLevel5Shift,
}

/// A pointer event. Coordinates are reported in the terminal's active mouse
/// coordinate space; Tilcayo requests SGR-Pixels mode for runtime sessions.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PointerEvent {
    /// Pointer action.
    pub kind: PointerEventKind,
    /// Horizontal coordinate in the active terminal mouse coordinate space.
    pub x: u16,
    /// Vertical coordinate in the active terminal mouse coordinate space.
    pub y: u16,
    /// Modifier keys active for this event.
    pub modifiers: Modifiers,
}

/// The action represented by a pointer event.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PointerEventKind {
    /// A button was pressed.
    Down(PointerButton),
    /// A button was released.
    Up(PointerButton),
    /// The pointer moved while a button was held.
    Drag(PointerButton),
    /// The pointer moved without a reported held button.
    Moved,
    /// The wheel or touch surface scrolled up.
    ScrollUp,
    /// The wheel or touch surface scrolled down.
    ScrollDown,
    /// The wheel or touch surface scrolled left.
    ScrollLeft,
    /// The wheel or touch surface scrolled right.
    ScrollRight,
}

/// A pointer button.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PointerButton {
    /// Primary (left) button.
    Left,
    /// Secondary (right) button.
    Right,
    /// Middle button.
    Middle,
    /// A button not otherwise identified by this version of Tilcayo.
    Other(u16),
}

macro_rules! flags {
    ($name:ident, $bits:ty, $( $flag:ident = $value:expr ),+ $(,)?) => {
        #[doc = concat!("A bit set of `", stringify!($name), "` flags.")]
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
        pub struct $name($bits);

        impl $name {
            /// No flags are set.
            pub const NONE: Self = Self(0);
            $(#[doc = concat!("The `", stringify!($flag), "` flag.")]
            pub const $flag: Self = Self($value);)+

            /// Creates a value while retaining both known and unknown bits.
            pub const fn from_bits_retain(bits: $bits) -> Self {
                Self(bits)
            }

            /// Returns the raw flag bits.
            pub const fn bits(self) -> $bits {
                self.0
            }

            /// Returns whether no flags are set.
            pub const fn is_empty(self) -> bool {
                self.0 == 0
            }

            /// Returns whether all flags in `other` are set.
            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }

            /// Returns whether any flag in `other` is set.
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
