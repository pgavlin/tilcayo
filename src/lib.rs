//! A low-level GUI runtime for Kitty terminals.
//!
//! [`Runtime`] owns terminal session setup and restoration, capability probing,
//! input events, and serialized graphics output. Tilcayo accepts packed-RGB
//! framebuffers, plans damaged regions, and writes Kitty graphics protocol
//! commands using direct, temporary-file, or POSIX shared-memory transfers. Its
//! latest-frame mailbox prevents slow terminal output from blocking a UI event
//! loop. The crate currently targets Unix-like systems with POSIX terminal and
//! shared-memory APIs.

#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

mod capabilities;
mod clipboard;
mod damage;
mod events;
mod frame;
mod input;
/// Kitty graphics protocol probing, transport, and presentation.
pub mod kitty;
mod mailbox;
mod runtime;
mod session;
mod wakeup;
mod worker;

pub use capabilities::{LogicalDpi, TerminalCapabilities, TerminalSize};
pub use clipboard::osc52;
pub use damage::{plan_damage, DamagePolicy, Rect};
pub use events::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, MediaKeyCode, ModifierKeyCode,
    Modifiers, PointerButton, PointerEvent, PointerEventKind,
};
pub use frame::Frame;
pub use input::{map_pixel_pointer, EventReader};
pub use kitty::Placement;
pub use mailbox::{LatestFrameMailbox, Presentation};
pub use runtime::{Runtime, RuntimeConfig};
pub use session::TerminalSession;
pub use wakeup::Wakeup;
pub use worker::{PresentationObserver, PresenterWorker};
