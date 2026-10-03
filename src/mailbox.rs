use std::sync::{Arc, Condvar, Mutex};

use super::{kitty::Placement, Frame, Rect};

/// An immutable frame presentation request.
///
/// Placement is captured when the request is constructed, so a later resize or
/// placement change cannot alter where an already submitted frame is drawn.
#[derive(Clone, Debug)]
pub struct Presentation {
    frame: Frame,
    placement: Placement,
}

impl Presentation {
    /// Captures `frame` and its terminal-cell `placement` in one request.
    pub fn new(frame: Frame, placement: Placement) -> Self {
        Self { frame, placement }
    }

    /// Returns the submitted frame.
    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// Returns the placement captured for this frame.
    pub fn placement(&self) -> Placement {
        self.placement
    }

    /// Decomposes this request into its frame and placement.
    pub fn into_parts(self) -> (Frame, Placement) {
        (self.frame, self.placement)
    }
}

/// A single-slot mailbox that retains the most recently submitted presentation.
///
/// Submission does not wait for capacity. If another request is pending, the
/// new request replaces it; a request already taken by a consumer is unaffected.
/// "Latest" means most recently submitted—the mailbox does not inspect or order
/// frame serials. Clones share the same pending slot and closure state.
///
/// Damage from a replaced frame is added to the replacement so changes remain
/// visible when intermediate frames are skipped. This requires each [`Frame`]
/// to contain the complete current framebuffer. If the framebuffer dimensions
/// differ, the replacement is marked as fully damaged instead. During
/// replacement, more than 32 accumulated nonempty rectangles are reduced to
/// their clipped bounding rectangle to bound damage bookkeeping.
///
/// Closing the mailbox rejects new submissions and wakes blocked consumers. A
/// request already pending remains available before the mailbox reports closure.
#[derive(Clone, Default)]
pub struct LatestFrameMailbox {
    shared: Arc<(Mutex<State>, Condvar)>,
}

#[derive(Default)]
struct State {
    pending: Option<Presentation>,
    wake: bool,
    closed: bool,
    dropped: u64,
}

pub(crate) enum MailboxEvent {
    Presentation(Presentation),
    Wake,
    Closed,
}

impl LatestFrameMailbox {
    /// Submits a request, replacing any pending request and preserving damage.
    ///
    /// Returns the supplied request unchanged if the mailbox is closed.
    pub fn submit(&self, mut presentation: Presentation) -> Result<(), Presentation> {
        let (lock, ready) = &*self.shared;
        let mut state = lock.lock().expect("latest-frame mailbox poisoned");
        if state.closed {
            return Err(presentation);
        }
        if let Some(old) = state.pending.take() {
            if old.frame.size() == presentation.frame.size() {
                presentation
                    .frame
                    .damage_mut()
                    .extend_from_slice(old.frame.damage());
                let (width, height) = presentation.frame.size();
                let damage = std::mem::take(presentation.frame.damage_mut());
                *presentation.frame.damage_mut() = coalesce_bounds(damage, width, height);
            } else {
                let (width, height) = presentation.frame.size();
                *presentation.frame.damage_mut() = vec![Rect::full(width, height)];
            }
            state.dropped += 1;
        }
        state.pending = Some(presentation);
        ready.notify_one();
        Ok(())
    }

    /// Blocks until a presentation is available or the mailbox is closed.
    pub fn take(&self) -> Option<Presentation> {
        loop {
            match self.take_event() {
                MailboxEvent::Presentation(presentation) => return Some(presentation),
                MailboxEvent::Wake => {}
                MailboxEvent::Closed => return None,
            }
        }
    }

    pub(crate) fn take_event(&self) -> MailboxEvent {
        let (lock, ready) = &*self.shared;
        let mut state = lock.lock().expect("latest-frame mailbox poisoned");
        while state.pending.is_none() && !state.wake && !state.closed {
            state = ready.wait(state).expect("latest-frame mailbox poisoned");
        }
        if let Some(presentation) = state.pending.take() {
            MailboxEvent::Presentation(presentation)
        } else if state.wake {
            state.wake = false;
            MailboxEvent::Wake
        } else {
            MailboxEvent::Closed
        }
    }

    pub(crate) fn wake(&self) {
        let (lock, ready) = &*self.shared;
        let mut state = lock.lock().expect("latest-frame mailbox poisoned");
        if !state.closed {
            state.wake = true;
            ready.notify_one();
        }
    }

    /// Takes the pending presentation without blocking.
    pub fn try_take(&self) -> Option<Presentation> {
        self.shared
            .0
            .lock()
            .expect("latest-frame mailbox poisoned")
            .pending
            .take()
    }

    /// Closes the mailbox and wakes blocked consumers.
    ///
    /// A presentation already pending remains available for one final take.
    pub fn close(&self) {
        let (lock, ready) = &*self.shared;
        lock.lock().expect("latest-frame mailbox poisoned").closed = true;
        ready.notify_all();
    }

    /// Returns the number of pending frames replaced by newer submissions.
    pub fn dropped(&self) -> u64 {
        self.shared
            .0
            .lock()
            .expect("latest-frame mailbox poisoned")
            .dropped
    }
}

fn coalesce_bounds(mut damage: Vec<Rect>, width: u32, height: u32) -> Vec<Rect> {
    damage.retain(|rect| rect.width != 0 && rect.height != 0);
    if damage.len() <= 32 {
        return damage;
    }
    damage
        .into_iter()
        .reduce(Rect::union)
        .and_then(|rect| rect.clip(width, height))
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presentation(serial: u64, damage: Vec<Rect>, column: u16) -> Presentation {
        Presentation::new(
            Frame::rgb(serial, 2, 2, 6, vec![0; 12], damage).unwrap(),
            Placement::new(column, 0, 2, 2).unwrap(),
        )
    }

    #[test]
    fn replacement_is_bounded_and_preserves_damage() {
        let mailbox = LatestFrameMailbox::default();
        mailbox
            .submit(presentation(1, vec![Rect::new(0, 0, 1, 1)], 0))
            .unwrap();
        mailbox
            .submit(presentation(2, vec![Rect::new(1, 1, 1, 1)], 1))
            .unwrap();
        let latest = mailbox.try_take().unwrap();
        assert_eq!(latest.frame().serial(), 2);
        assert_eq!(latest.frame().damage().len(), 2);
        assert_eq!(latest.placement().column, 1);
        assert_eq!(mailbox.dropped(), 1);
    }
}
