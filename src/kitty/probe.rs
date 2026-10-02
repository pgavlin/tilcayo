use std::{
    io::{self, Read, Write},
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

use termwiz::escape::{apc::KittyImageData, parser::Parser, Action, KittyImage};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphicsCapabilities {
    pub graphics: bool,
    /// Local shared-memory transfer is eligible; actual support is confirmed
    /// by the first transfer and automatically falls back in Auto mode.
    pub shared_memory: bool,
    pub animation: bool,
}

/// Actively probes Kitty graphics before the normal Crossterm event reader is
/// started. Termwiz, a maintained terminal parser, handles fragmented APC
/// framing; Tilcayo only interprets the typed Kitty action.
pub fn probe(
    input: &mut (impl Read + AsRawFd),
    output: &mut impl Write,
    timeout: Duration,
) -> io::Result<GraphicsCapabilities> {
    let id = std::process::id().wrapping_add(0x5759).max(1);
    write!(output, "\x1b_Gi={id},s=1,v=1,a=q,t=d,f=24,q=0;AAAA\x1b\\")?;
    output.flush()?;

    if wait_for_ack(input, id, timeout)? {
        let local = std::env::var_os("SSH_CONNECTION").is_none()
            && std::env::var_os("TMUX").is_none()
            && std::env::var_os("STY").is_none();
        return Ok(GraphicsCapabilities {
            graphics: true,
            shared_memory: local,
            animation: true,
        });
    }
    Ok(GraphicsCapabilities::default())
}

/// Waits for a particular Kitty acknowledgement using Termwiz's parser.
/// Returns false for a Kitty error reply or a timeout.
pub fn wait_for_ack(
    input: &mut (impl Read + AsRawFd),
    id: u32,
    timeout: Duration,
) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    let mut parser = Parser::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let millis = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, millis) };
        if ready == 0 {
            break;
        }
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut bytes = [0; 256];
        let count = input.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        let mut response = None;
        parser.parse(&bytes[..count], |action| {
            if let Some(ok) = response_for(&action, id) {
                response = Some(ok);
            }
        });
        if let Some(ok) = response {
            return Ok(ok);
        }
    }
    Ok(false)
}

fn response_for(action: &Action, id: u32) -> Option<bool> {
    let Action::KittyImage(image) = action else {
        return None;
    };
    let KittyImage::TransmitData { transmit, .. } = image.as_ref() else {
        return None;
    };
    if transmit.image_id != Some(id) {
        return None;
    }
    Some(matches!(&transmit.data, KittyImageData::Direct(payload) if payload == "OK"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintained_parser_handles_fragmented_kitty_reply() {
        let id = 77;
        let mut parser = Parser::new();
        let mut ok = false;
        for chunk in [
            b"\x1b_".as_slice(),
            b"Gi=77".as_slice(),
            b";O".as_slice(),
            b"K\x1b\\".as_slice(),
        ] {
            parser.parse(chunk, |action| {
                ok |= response_for(&action, id) == Some(true)
            });
        }
        assert!(ok);
    }

    #[test]
    fn rejects_an_error_reply() {
        let mut parser = Parser::new();
        assert!(!parser
            .parse_as_vec(b"\x1b_Gi=77;EINVAL\x1b\\")
            .iter()
            .any(|action| response_for(action, 77) == Some(true)));
    }
}
