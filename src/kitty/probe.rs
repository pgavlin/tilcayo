use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use termwiz::{
    escape::{apc::KittyImageData, parser::Parser, Action, KittyImage},
    input::{InputEvent, InputParser, KeyCode as TermwizKeyCode, Modifiers as TermwizModifiers},
};

use crate::{Event, KeyCode, KeyEvent, LogicalDpi, Modifiers};

// Runtime input ownership is deliberately phased. During this module's probe,
// advanced terminal input modes are still disabled and Termwiz parses both
// Kitty APC replies and any basic keystrokes that share the byte stream. Those
// keystrokes are translated and queued before Crossterm becomes the sole input
// reader. This avoids concurrent readers and preserves Crossterm's richer
// steady-state handling, but it is not a transferable parser state: a sequence
// fragmented exactly across the handoff remains a theoretical edge case. A
// Kitty-only parser would not remove that limitation unless Crossterm also
// gained a public API for injecting unconsumed bytes or extending its parser.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphicsCapabilities {
    pub graphics: bool,
    /// POSIX shared-memory transfer was successfully queried.
    pub shared_memory: bool,
    /// Temporary-file transfer was successfully queried.
    pub temporary_file: bool,
    /// Animation-frame updates were successfully queried.
    pub animation: bool,
}

/// Capabilities discovered during the isolated terminal probe phase.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TerminalProbe {
    pub graphics: GraphicsCapabilities,
    pub logical_dpi: Option<LogicalDpi>,
}

/// Actively probes Kitty graphics before the normal Crossterm event reader is
/// started.
///
/// This convenience function discards concurrent startup keystrokes. Runtime
/// integrations should prefer [`probe_with_events`] and enqueue its events.
pub fn probe(
    input: &mut (impl Read + AsRawFd),
    output: &mut impl Write,
    timeout: Duration,
) -> io::Result<GraphicsCapabilities> {
    probe_terminal(input, output, timeout).map(|probe| probe.graphics)
}

/// Probe graphics support and Kitty's logical DPI.
///
/// This convenience function discards concurrent startup keystrokes. Runtime
/// integrations should prefer [`probe_terminal_with_events`].
pub fn probe_terminal(
    input: &mut (impl Read + AsRawFd),
    output: &mut impl Write,
    timeout: Duration,
) -> io::Result<TerminalProbe> {
    probe_terminal_with_events(input, output, timeout).map(|(probe, _)| probe)
}

/// Probe graphics support while decoding unrelated startup keystrokes instead
/// of discarding them. Termwiz handles fragmented APC framing and unenhanced
/// keyboard input. Mouse, focus, paste, and keyboard-enhancement reporting
/// modes must not yet be enabled.
pub fn probe_with_events(
    input: &mut (impl Read + AsRawFd),
    output: &mut impl Write,
    timeout: Duration,
) -> io::Result<(GraphicsCapabilities, Vec<Event>)> {
    probe_terminal_with_events(input, output, timeout)
        .map(|(probe, events)| (probe.graphics, events))
}

/// Probe graphics support and Kitty's logical DPI while preserving unrelated
/// startup keystrokes. All queries share the supplied timeout.
pub fn probe_terminal_with_events(
    input: &mut (impl Read + AsRawFd),
    output: &mut impl Write,
    timeout: Duration,
) -> io::Result<(TerminalProbe, Vec<Event>)> {
    let id = std::process::id().wrapping_add(0x5759).max(1);
    write!(output, "\x1b_Gi={id},s=1,v=1,a=q,t=d,f=24,q=0;AAAA\x1b\\")?;
    output.flush()?;

    let deadline = Instant::now() + timeout;
    let mut decoder = ProbeDecoder::default();
    let graphics = wait_for_ack_preserving(input, id, deadline, &mut decoder)?;
    let mut shared_memory = false;
    let mut temporary_file = false;
    let mut animation = false;

    if graphics {
        if local_media_eligible() {
            shared_memory = probe_shared_memory(input, output, deadline, &mut decoder)?;
            temporary_file = probe_temporary_file(input, output, deadline, &mut decoder)?;
        }
        animation = probe_animation(input, output, deadline, &mut decoder)?;
    }
    let logical_dpi = probe_logical_dpi(input, output, deadline, &mut decoder)?;

    let events = decoder.finish();
    Ok((
        TerminalProbe {
            graphics: GraphicsCapabilities {
                graphics,
                shared_memory,
                temporary_file,
                animation,
            },
            logical_dpi,
        },
        events,
    ))
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

fn local_media_eligible() -> bool {
    std::env::var_os("SSH_CONNECTION").is_none()
        && std::env::var_os("TMUX").is_none()
        && std::env::var_os("STY").is_none()
}

fn probe_shared_memory(
    input: &mut (impl Read + AsRawFd),
    output: &mut impl Write,
    deadline: Instant,
    decoder: &mut ProbeDecoder,
) -> io::Result<bool> {
    let object = match ProbeShm::new(b"\0\0\0") {
        Ok(object) => object,
        Err(_) => return Ok(false),
    };
    let id = std::process::id().wrapping_add(0x575a).max(1);
    let name = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        object.name.as_bytes(),
    );
    write!(
        output,
        "\x1b_Gi={id},s=1,v=1,a=q,t=s,f=24,q=0,S=3;{name}\x1b\\"
    )?;
    output.flush()?;
    wait_for_ack_preserving(input, id, deadline, decoder)
}

fn probe_temporary_file(
    input: &mut (impl Read + AsRawFd),
    output: &mut impl Write,
    deadline: Instant,
    decoder: &mut ProbeDecoder,
) -> io::Result<bool> {
    let object = match ProbeTemp::new(b"\0\0\0") {
        Ok(object) => object,
        Err(_) => return Ok(false),
    };
    let id = std::process::id().wrapping_add(0x575c).max(1);
    let path = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        object.path.as_os_str().as_encoded_bytes(),
    );
    write!(
        output,
        "\x1b_Gi={id},s=1,v=1,a=q,t=t,f=24,q=0,S=3;{path}\x1b\\"
    )?;
    output.flush()?;
    wait_for_ack_preserving(input, id, deadline, decoder)
}

const DPI_X_QUERY: &str = "kitty-query-dpi_x";
const DPI_Y_QUERY: &str = "kitty-query-dpi_y";

fn probe_logical_dpi(
    input: &mut (impl Read + AsRawFd),
    output: &mut impl Write,
    deadline: Instant,
    decoder: &mut ProbeDecoder,
) -> io::Result<Option<LogicalDpi>> {
    write!(
        output,
        "\x1bP+q{};{}\x1b\\",
        hex_encode(DPI_X_QUERY.as_bytes()),
        hex_encode(DPI_Y_QUERY.as_bytes())
    )?;
    output.flush()?;

    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        if let Some(dpi) = decoder.logical_dpi() {
            return Ok(Some(dpi));
        }
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
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let mut bytes = [0; 256];
        let count = input.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        decoder.feed(&bytes[..count], 0);
    }
    Ok(decoder.logical_dpi())
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}

fn probe_animation(
    input: &mut (impl Read + AsRawFd),
    output: &mut impl Write,
    deadline: Instant,
    decoder: &mut ProbeDecoder,
) -> io::Result<bool> {
    let id = std::process::id().wrapping_add(0x575b).max(1);
    write!(output, "\x1b_Gi={id},s=1,v=1,a=t,t=d,f=24,q=0;AAAA\x1b\\")?;
    output.flush()?;
    if !wait_for_ack_preserving(input, id, deadline, decoder)? {
        return Ok(false);
    }
    write!(
        output,
        "\x1b_Gi={id},a=f,r=1,x=0,y=0,s=1,v=1,t=d,f=24,X=1,q=0;AAAA\x1b\\"
    )?;
    output.flush()?;
    let supported = wait_for_ack_preserving(input, id, deadline, decoder)?;
    write!(output, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\")?;
    output.flush()?;
    Ok(supported)
}

static NEXT_PROBE_SHM: AtomicU64 = AtomicU64::new(1);

struct ProbeShm {
    name: CString,
}

impl ProbeShm {
    fn new(bytes: &[u8]) -> io::Result<Self> {
        loop {
            let name = CString::new(format!(
                "/tilcayo-probe-{:x}-{:x}",
                std::process::id(),
                NEXT_PROBE_SHM.fetch_add(1, Ordering::Relaxed)
            ))
            .unwrap();
            let fd = unsafe {
                libc::shm_open(
                    name.as_ptr(),
                    libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                    0o600,
                )
            };
            if fd < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(error);
            }
            let mut file = unsafe { File::from_raw_fd(fd) };
            if let Err(error) = file.write_all(bytes).and_then(|_| file.flush()) {
                unsafe { libc::shm_unlink(name.as_ptr()) };
                return Err(error);
            }
            return Ok(Self { name });
        }
    }
}

impl Drop for ProbeShm {
    fn drop(&mut self) {
        // A supporting terminal has already unlinked the object. This also
        // cleans it up when the query fails or times out.
        unsafe { libc::shm_unlink(self.name.as_ptr()) };
    }
}

struct ProbeTemp {
    path: PathBuf,
}

impl ProbeTemp {
    fn new(bytes: &[u8]) -> io::Result<Self> {
        loop {
            let path = std::env::temp_dir().join(format!(
                "tilcayo-tty-graphics-protocol-probe-{}-{}",
                std::process::id(),
                NEXT_PROBE_SHM.fetch_add(1, Ordering::Relaxed)
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(bytes)?;
                    file.flush()?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for ProbeTemp {
    fn drop(&mut self) {
        // A supporting terminal may already have removed the file.
        let _ = fs::remove_file(&self.path);
    }
}

fn wait_for_ack_preserving(
    input: &mut (impl Read + AsRawFd),
    id: u32,
    deadline: Instant,
    decoder: &mut ProbeDecoder,
) -> io::Result<bool> {
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
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let mut bytes = [0; 256];
        let count = input.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        if let Some(ok) = decoder.feed(&bytes[..count], id) {
            return Ok(ok);
        }
    }
    Ok(false)
}

#[derive(Default)]
struct ProbeDecoder {
    undecoded: Vec<u8>,
    input: InputParser,
    events: Vec<Event>,
    dpi_x: Option<f64>,
    dpi_y: Option<f64>,
}

#[derive(Clone, Copy)]
enum ProtocolString {
    KittyGraphics,
    DeviceControl,
}

impl ProbeDecoder {
    fn feed(&mut self, bytes: &[u8], id: u32) -> Option<bool> {
        const KITTY_PREFIX: &[u8] = b"\x1b_G";
        const DCS_PREFIX: &[u8] = b"\x1bP";
        const STRING_TERMINATOR: &[u8] = b"\x1b\\";

        self.undecoded.extend_from_slice(bytes);
        let mut response = None;
        loop {
            let kitty = find_bytes(&self.undecoded, KITTY_PREFIX)
                .map(|start| (start, ProtocolString::KittyGraphics));
            let dcs = find_bytes(&self.undecoded, DCS_PREFIX)
                .map(|start| (start, ProtocolString::DeviceControl));
            let Some((start, kind)) = earliest(kitty, dcs) else {
                let retained = if self.undecoded.ends_with(b"\x1b_") {
                    2
                } else if self.undecoded.ends_with(b"\x1b") {
                    1
                } else {
                    0
                };
                let ready = self.undecoded.len() - retained;
                self.feed_input_prefix(ready, true);
                break;
            };
            if start != 0 {
                self.feed_input_prefix(start, true);
                continue;
            }
            let prefix_len = match kind {
                ProtocolString::KittyGraphics => KITTY_PREFIX.len(),
                ProtocolString::DeviceControl => DCS_PREFIX.len(),
            };
            let Some(end) = find_bytes(&self.undecoded[prefix_len..], STRING_TERMINATOR) else {
                break;
            };
            let command_len = prefix_len + end + STRING_TERMINATOR.len();
            let command: Vec<_> = self.undecoded.drain(..command_len).collect();
            match kind {
                ProtocolString::KittyGraphics => {
                    let mut parser = Parser::new();
                    for action in parser.parse_as_vec(&command) {
                        if let Some(ok) = response_for(&action, id) {
                            response = Some(ok);
                        }
                    }
                }
                ProtocolString::DeviceControl => self.accept_terminal_query(&command),
            }
        }
        response
    }

    fn accept_terminal_query(&mut self, command: &[u8]) {
        let Some(body) = command
            .strip_prefix(b"\x1bP1+r")
            .and_then(|body| body.strip_suffix(b"\x1b\\"))
        else {
            return;
        };
        let Some(separator) = body.iter().position(|byte| *byte == b'=') else {
            return;
        };
        let (name, value) = (&body[..separator], &body[separator + 1..]);
        let (Some(name), Some(value)) = (hex_decode(name), hex_decode(value)) else {
            return;
        };
        let Some(value) = std::str::from_utf8(&value)
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
        else {
            return;
        };
        if !value.is_finite() || !(1.0..=1000.0).contains(&value) {
            return;
        }
        match name.as_slice() {
            b"kitty-query-dpi_x" => self.dpi_x = Some(value),
            b"kitty-query-dpi_y" => self.dpi_y = Some(value),
            _ => {}
        }
    }

    fn logical_dpi(&self) -> Option<LogicalDpi> {
        LogicalDpi::new(self.dpi_x?, self.dpi_y?)
    }

    fn feed_input_prefix(&mut self, len: usize, maybe_more: bool) {
        if len == 0 {
            return;
        }
        let bytes: Vec<_> = self.undecoded.drain(..len).collect();
        let events = &mut self.events;
        self.input.parse(
            &bytes,
            |event| {
                if let Some(event) = adapt_probe_input(event) {
                    events.push(event);
                }
            },
            maybe_more,
        );
    }

    fn finish(mut self) -> Vec<Event> {
        let len = self.undecoded.len();
        self.feed_input_prefix(len, false);
        let events = &mut self.events;
        self.input.parse(
            &[],
            |event| {
                if let Some(event) = adapt_probe_input(event) {
                    events.push(event);
                }
            },
            false,
        );
        self.events
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn earliest(
    left: Option<(usize, ProtocolString)>,
    right: Option<(usize, ProtocolString)>,
) -> Option<(usize, ProtocolString)> {
    match (left, right) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn hex_decode(encoded: &[u8]) -> Option<Vec<u8>> {
    if encoded.len() % 2 != 0 {
        return None;
    }
    encoded
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            Some(((high << 4) | low) as u8)
        })
        .collect()
}

fn adapt_probe_input(event: InputEvent) -> Option<Event> {
    let InputEvent::Key(key) = event else {
        return None;
    };
    let code = match key.key {
        TermwizKeyCode::Char(value) => KeyCode::Char(value),
        TermwizKeyCode::Backspace => KeyCode::Backspace,
        TermwizKeyCode::Enter => KeyCode::Enter,
        TermwizKeyCode::LeftArrow | TermwizKeyCode::ApplicationLeftArrow => KeyCode::Left,
        TermwizKeyCode::RightArrow | TermwizKeyCode::ApplicationRightArrow => KeyCode::Right,
        TermwizKeyCode::UpArrow | TermwizKeyCode::ApplicationUpArrow => KeyCode::Up,
        TermwizKeyCode::DownArrow | TermwizKeyCode::ApplicationDownArrow => KeyCode::Down,
        TermwizKeyCode::Home | TermwizKeyCode::KeyPadHome => KeyCode::Home,
        TermwizKeyCode::End | TermwizKeyCode::KeyPadEnd => KeyCode::End,
        TermwizKeyCode::PageUp | TermwizKeyCode::KeyPadPageUp => KeyCode::PageUp,
        TermwizKeyCode::PageDown | TermwizKeyCode::KeyPadPageDown => KeyCode::PageDown,
        TermwizKeyCode::Tab => KeyCode::Tab,
        TermwizKeyCode::Delete => KeyCode::Delete,
        TermwizKeyCode::Insert => KeyCode::Insert,
        TermwizKeyCode::Function(number) => KeyCode::F(number),
        TermwizKeyCode::Escape => KeyCode::Esc,
        _ => return None,
    };
    let mut modifiers = Modifiers::NONE;
    if key.modifiers.contains(TermwizModifiers::SHIFT) {
        modifiers |= Modifiers::SHIFT;
    }
    if key.modifiers.contains(TermwizModifiers::ALT) {
        modifiers |= Modifiers::ALT;
    }
    if key.modifiers.contains(TermwizModifiers::CTRL) {
        modifiers |= Modifiers::CONTROL;
    }
    if key.modifiers.contains(TermwizModifiers::SUPER) {
        modifiers |= Modifiers::SUPER;
    }
    Some(Event::Key(KeyEvent::new(code, modifiers)))
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

    #[test]
    fn decodes_fragmented_logical_dpi_responses() {
        let mut decoder = ProbeDecoder::default();
        decoder.feed(b"\x1bP1+r6b697474792d71756572792d6470695f78=313434\x1b", 0);
        assert_eq!(decoder.logical_dpi(), None);
        decoder.feed(
            b"\\\x1bP1+r6b697474792d71756572792d6470695f79=3132302e35\x1b\\",
            0,
        );
        assert_eq!(decoder.logical_dpi(), LogicalDpi::new(144.0, 120.5));
    }

    #[test]
    fn rejects_invalid_logical_dpi_responses() {
        let mut decoder = ProbeDecoder::default();
        decoder.feed(b"\x1bP1+r6b697474792d71756572792d6470695f78=30\x1b\\", 0);
        decoder.feed(
            b"\x1bP1+r6b697474792d71756572792d6470695f79=4e614e\x1b\\",
            0,
        );
        assert_eq!(decoder.logical_dpi(), None);
    }

    #[test]
    fn probe_decoder_preserves_input_around_fragmented_reply() {
        let mut decoder = ProbeDecoder::default();
        assert_eq!(decoder.feed(b"q\x1b_", 77), None);
        assert_eq!(decoder.feed(b"Gi=77;O", 77), None);
        assert_eq!(decoder.feed(b"K\x1b\\x", 77), Some(true));
        assert_eq!(
            decoder.finish(),
            vec![
                Event::Key(KeyEvent::new(KeyCode::Char('q'), Modifiers::NONE)),
                Event::Key(KeyEvent::new(KeyCode::Char('x'), Modifiers::NONE))
            ]
        );
    }
}
