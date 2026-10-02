//! A low-level GUI runtime for Kitty terminals.
//!
//! [`Runtime`] owns terminal session setup and restoration, capability probing,
//! input events, and serialized graphics output. Tilcayo accepts immutable
//! packed-RGB framebuffers, plans damaged regions, and writes Kitty graphics
//! protocol commands using direct, temporary-file, or POSIX shared-memory
//! transfers. Its bounded latest-frame queue prevents slow terminal output from
//! blocking a UI event loop.

mod capabilities;
mod clipboard;
mod damage;
mod events;
mod frame;
mod input;
pub mod kitty;
mod queue;
mod runtime;
mod session;
mod worker;

pub use capabilities::{TerminalCapabilities, TerminalSize};
pub use clipboard::osc52;
pub use damage::{plan_damage, DamagePolicy, Rect};
pub use events::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, MediaKeyCode, ModifierKeyCode,
    Modifiers, PointerButton, PointerEvent, PointerEventKind,
};
pub use frame::Frame;
pub use input::{map_pixel_pointer, EventReader};
pub use kitty::Placement;
pub use queue::LatestFrameQueue;
pub use runtime::{Runtime, RuntimeConfig};
pub use session::TerminalSession;
pub use worker::{PresentationObserver, PresenterWorker};
