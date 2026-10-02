use std::sync::{Arc, Condvar, Mutex};

use super::{Frame, Rect};

/// A bounded queue containing only the newest frame not yet taken by the worker.
/// Replaced frame damage is carried into the replacement frame.
#[derive(Clone, Default)]
pub struct LatestFrameQueue {
    shared: Arc<(Mutex<State>, Condvar)>,
}

#[derive(Default)]
struct State {
    pending: Option<Frame>,
    wake: bool,
    closed: bool,
    dropped: u64,
}

pub(crate) enum QueueEvent {
    Frame(Frame),
    Wake,
    Closed,
}

impl LatestFrameQueue {
    pub fn submit(&self, mut frame: Frame) -> Result<(), Frame> {
        let (lock, ready) = &*self.shared;
        let mut state = lock.lock().expect("latest-frame queue poisoned");
        if state.closed {
            return Err(frame);
        }
        if let Some(old) = state.pending.take() {
            if old.size() == frame.size() {
                frame.damage.extend(old.damage);
                frame.damage = coalesce_bounds(frame.damage, frame.width, frame.height);
            } else {
                frame.damage = vec![Rect::full(frame.width, frame.height)];
            }
            state.dropped += 1;
        }
        state.pending = Some(frame);
        ready.notify_one();
        Ok(())
    }

    pub fn take(&self) -> Option<Frame> {
        loop {
            match self.take_event() {
                QueueEvent::Frame(frame) => return Some(frame),
                QueueEvent::Wake => {}
                QueueEvent::Closed => return None,
            }
        }
    }

    pub(crate) fn take_event(&self) -> QueueEvent {
        let (lock, ready) = &*self.shared;
        let mut state = lock.lock().expect("latest-frame queue poisoned");
        while state.pending.is_none() && !state.wake && !state.closed {
            state = ready.wait(state).expect("latest-frame queue poisoned");
        }
        if let Some(frame) = state.pending.take() {
            QueueEvent::Frame(frame)
        } else if state.wake {
            state.wake = false;
            QueueEvent::Wake
        } else {
            QueueEvent::Closed
        }
    }

    pub(crate) fn wake(&self) {
        let (lock, ready) = &*self.shared;
        let mut state = lock.lock().expect("latest-frame queue poisoned");
        if !state.closed {
            state.wake = true;
            ready.notify_one();
        }
    }

    pub fn try_take(&self) -> Option<Frame> {
        self.shared
            .0
            .lock()
            .expect("latest-frame queue poisoned")
            .pending
            .take()
    }

    pub fn close(&self) {
        let (lock, ready) = &*self.shared;
        lock.lock().expect("latest-frame queue poisoned").closed = true;
        ready.notify_all();
    }

    pub fn dropped(&self) -> u64 {
        self.shared
            .0
            .lock()
            .expect("latest-frame queue poisoned")
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

    fn frame(serial: u64, damage: Vec<Rect>) -> Frame {
        Frame::rgb(serial, 2, 2, 6, vec![0; 12], damage).unwrap()
    }

    #[test]
    fn replacement_is_bounded_and_preserves_damage() {
        let queue = LatestFrameQueue::default();
        queue.submit(frame(1, vec![Rect::new(0, 0, 1, 1)])).unwrap();
        queue.submit(frame(2, vec![Rect::new(1, 1, 1, 1)])).unwrap();
        let latest = queue.try_take().unwrap();
        assert_eq!(latest.serial, 2);
        assert_eq!(latest.damage.len(), 2);
        assert_eq!(queue.dropped(), 1);
    }
}
