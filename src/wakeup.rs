use std::{
    io,
    os::fd::{AsFd, AsRawFd, BorrowedFd},
    os::unix::net::UnixStream,
    sync::Arc,
    time::Duration,
};

/// A pollable, level-triggered notification handle.
///
/// The borrowed file descriptor becomes readable when associated runtime state
/// may have changed. Call [`Self::clear`] before draining that state. A producer
/// that races either publishes state for the current drain or leaves the
/// descriptor readable for the next poll. Notifications may be coalesced, so
/// callers must drain all available input and completion state rather than
/// assuming one item per wakeup.
#[derive(Clone)]
pub struct Wakeup {
    inner: Arc<Inner>,
}

struct Inner {
    reader: UnixStream,
    writer: UnixStream,
}

impl Wakeup {
    pub(crate) fn new() -> io::Result<Self> {
        let (reader, writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        Ok(Self {
            inner: Arc::new(Inner { reader, writer }),
        })
    }

    pub(crate) fn signal(&self) {
        let byte = 1_u8;
        loop {
            let result = unsafe {
                libc::write(
                    self.inner.writer.as_raw_fd(),
                    (&byte as *const u8).cast(),
                    1,
                )
            };
            if result == 1 {
                return;
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // A full socket is already readable, and closure only occurs when
            // the final handle is being dropped.
            return;
        }
    }

    /// Drains pending wakeup bytes without blocking.
    ///
    /// Clear the handle before reading all available state. A producer that
    /// publishes afterward makes the handle readable again.
    pub fn clear(&self) -> io::Result<()> {
        let mut bytes = [0_u8; 256];
        loop {
            let result = unsafe {
                libc::read(
                    self.inner.reader.as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if result > 0 {
                continue;
            }
            if result == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(()),
                _ => return Err(error),
            }
        }
    }

    /// Waits until the handle is readable or `timeout` expires.
    ///
    /// Returns `true` when state may be available and `false` on timeout. This
    /// convenience method does not clear the handle.
    pub fn wait(&self, timeout: Option<Duration>) -> io::Result<bool> {
        let milliseconds = match timeout {
            None => -1,
            Some(value) => value.as_millis().min(i32::MAX as u128) as i32,
        };
        let mut descriptor = libc::pollfd {
            fd: self.inner.reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            let result = unsafe { libc::poll(&mut descriptor, 1, milliseconds) };
            if result > 0 {
                return Ok(true);
            }
            if result == 0 {
                return Ok(false);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

impl AsFd for Wakeup {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.inner.reader.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_clear_and_can_signal_again() {
        let wakeup = Wakeup::new().unwrap();
        assert!(!wakeup.wait(Some(Duration::ZERO)).unwrap());
        wakeup.signal();
        assert!(wakeup.wait(Some(Duration::ZERO)).unwrap());
        wakeup.clear().unwrap();
        assert!(!wakeup.wait(Some(Duration::ZERO)).unwrap());
        wakeup.signal();
        assert!(wakeup.wait(Some(Duration::ZERO)).unwrap());
    }
}
