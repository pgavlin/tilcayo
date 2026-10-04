use std::{
    ffi::CString,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, QueryCommand, QueryResponse, TerminalResponse, TerminalResponseKind},
    Command,
};

use crate::LogicalDpi;

/// Kitty graphics features verified by active terminal queries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphicsCapabilities {
    /// Baseline Kitty graphics commands were successfully queried.
    pub graphics: bool,
    /// POSIX shared-memory transfer was successfully queried.
    pub shared_memory: bool,
    /// Temporary-file transfer was successfully queried.
    pub temporary_file: bool,
    /// Animation-frame updates were successfully queried.
    pub animation: bool,
    /// The transient graphics usage hint was accepted by the terminal.
    pub transient: bool,
}

/// Capabilities discovered during the isolated terminal probe phase.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TerminalProbe {
    /// Actively verified Kitty graphics features.
    pub graphics: GraphicsCapabilities,
    /// Kitty's logical DPI, when reported and valid.
    pub logical_dpi: Option<LogicalDpi>,
}

/// Actively probes Kitty graphics before the normal event reader is started.
///
/// Crossterm remains the sole terminal-input parser throughout probing. Input
/// events received while a query is pending stay in Crossterm's event queue and
/// are delivered by the normal reader after this function returns. Terminal
/// input must already be in raw mode so responses are available immediately.
pub fn probe(output: &mut impl Write, timeout: Duration) -> io::Result<GraphicsCapabilities> {
    probe_terminal(output, timeout).map(|probe| probe.graphics)
}

/// Probes Kitty graphics support and logical DPI.
///
/// All queries share `timeout`. A query is not emitted once that common
/// deadline has expired. Crossterm preserves unrelated terminal input in its
/// event queue for subsequent calls to its event APIs. Terminal input must
/// already be in raw mode so responses are available immediately.
pub fn probe_terminal(output: &mut impl Write, timeout: Duration) -> io::Result<TerminalProbe> {
    let deadline = deadline_after(timeout);
    let id = std::process::id().wrapping_add(0x5759).max(1);
    let graphics = query_kitty(
        output,
        format!("\x1b_Gi={id},s=1,v=1,a=q,t=d,f=24,q=0;AAAA\x1b\\"),
        &[id],
        deadline,
    )?
    .is_some_and(|response| response.id == id && response.ok);

    let mut shared_memory = false;
    let mut temporary_file = false;
    let mut animation = false;
    let mut transient = false;

    if graphics {
        transient = probe_transient_hint(output, deadline)?;
        if local_media_eligible() {
            shared_memory = probe_shared_memory(output, deadline)?;
            temporary_file = probe_temporary_file(output, deadline)?;
        }
        animation = probe_animation(output, deadline)?;
    }
    let logical_dpi = probe_logical_dpi(output, deadline)?;

    Ok(TerminalProbe {
        graphics: GraphicsCapabilities {
            graphics,
            shared_memory,
            temporary_file,
            animation,
            transient,
        },
        logical_dpi,
    })
}

fn deadline_after(timeout: Duration) -> Instant {
    let now = Instant::now();
    let mut bounded = timeout;
    loop {
        if let Some(deadline) = now.checked_add(bounded) {
            return deadline;
        }
        bounded /= 2;
    }
}

fn remaining(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
}

fn local_media_eligible() -> bool {
    std::env::var_os("SSH_CONNECTION").is_none()
        && std::env::var_os("TMUX").is_none()
        && std::env::var_os("STY").is_none()
}

#[derive(Debug)]
struct KittyResponse {
    id: u32,
    ok: bool,
}

impl QueryResponse for KittyResponse {
    const KIND: TerminalResponseKind = TerminalResponseKind::Apc;
}

struct KittyQuery {
    command: String,
    ids: Vec<u32>,
}

impl Command for KittyQuery {
    fn write_ansi(&self, writer: &mut impl fmt::Write) -> fmt::Result {
        writer.write_str(&self.command)
    }
}

impl QueryCommand for KittyQuery {
    type Response = KittyResponse;

    fn parse_response(
        &self,
        response: TerminalResponse,
    ) -> Result<Self::Response, TerminalResponse> {
        match parse_kitty_response(response.as_bytes()) {
            Some(parsed) if self.ids.contains(&parsed.id) => Ok(parsed),
            _ => Err(response),
        }
    }
}

fn query_kitty(
    output: &mut impl Write,
    command: String,
    ids: &[u32],
    deadline: Instant,
) -> io::Result<Option<KittyResponse>> {
    let Some(timeout) = remaining(deadline) else {
        return Ok(None);
    };
    match event::query(
        output,
        KittyQuery {
            command,
            ids: ids.to_vec(),
        },
        timeout,
    ) {
        Ok(response) => Ok(Some(response)),
        Err(error) if error.kind() == io::ErrorKind::TimedOut => Ok(None),
        Err(error) => Err(error),
    }
}

fn parse_kitty_response(bytes: &[u8]) -> Option<KittyResponse> {
    let body = bytes
        .strip_prefix(b"\x1b_G")
        .and_then(|body| body.strip_suffix(b"\x1b\\"))
        .or_else(|| {
            bytes
                .strip_prefix(b"\x9fG")
                .and_then(|body| body.strip_suffix(b"\x9c"))
        })?;
    let separator = body.iter().position(|byte| *byte == b';')?;
    let (control, payload) = (&body[..separator], &body[separator + 1..]);
    let id = control.split(|byte| *byte == b',').find_map(|field| {
        field
            .strip_prefix(b"i=")
            .and_then(|value| std::str::from_utf8(value).ok())
            .and_then(|value| value.parse().ok())
    })?;
    Some(KittyResponse {
        id,
        ok: payload == b"OK",
    })
}

fn probe_transient_hint(output: &mut impl Write, deadline: Instant) -> io::Result<bool> {
    let id = std::process::id().wrapping_add(0x575d).max(1);
    let sentinel_id = std::process::id().wrapping_add(0x575e).max(1);
    // Older Kitty versions reject an unknown N key without producing a
    // graphics response. Follow the extension query with a baseline query so
    // that rejection can be distinguished from an unresponsive terminal.
    let response = query_kitty(
        output,
        format!(
            "\x1b_Gi={id},s=1,v=1,a=q,t=d,f=24,q=0,N=1;AAAA\x1b\\\x1b_Gi={sentinel_id},s=1,v=1,a=q,t=d,f=24,q=0;AAAA\x1b\\"
        ),
        &[id, sentinel_id],
        deadline,
    )?;
    let supported = response
        .as_ref()
        .is_some_and(|response| response.id == id && response.ok);

    // A supporting terminal normally replies to both commands in order. If
    // the extension response completed the query, consume the already-issued
    // sentinel reply before changing the expected response framing.
    if response.is_some_and(|response| response.id == id) {
        let _ = query_kitty(output, String::new(), &[sentinel_id], deadline)?;
    }
    Ok(supported)
}

fn probe_shared_memory(output: &mut impl Write, deadline: Instant) -> io::Result<bool> {
    let object = match ProbeShm::new(b"\0\0\0") {
        Ok(object) => object,
        Err(_) => return Ok(false),
    };
    let id = std::process::id().wrapping_add(0x575a).max(1);
    let name = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        object.name.as_bytes(),
    );
    query_kitty(
        output,
        format!("\x1b_Gi={id},s=1,v=1,a=q,t=s,f=24,q=0,S=3;{name}\x1b\\"),
        &[id],
        deadline,
    )
    .map(|response| response.is_some_and(|response| response.ok))
}

fn probe_temporary_file(output: &mut impl Write, deadline: Instant) -> io::Result<bool> {
    let object = match ProbeTemp::new(b"\0\0\0") {
        Ok(object) => object,
        Err(_) => return Ok(false),
    };
    let id = std::process::id().wrapping_add(0x575c).max(1);
    let path = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        object.path.as_os_str().as_encoded_bytes(),
    );
    query_kitty(
        output,
        format!("\x1b_Gi={id},s=1,v=1,a=q,t=t,f=24,q=0,S=3;{path}\x1b\\"),
        &[id],
        deadline,
    )
    .map(|response| response.is_some_and(|response| response.ok))
}

const DPI_X_QUERY: &str = "kitty-query-dpi_x";
const DPI_Y_QUERY: &str = "kitty-query-dpi_y";

#[derive(Debug)]
struct TerminalCapabilityResponse(f64);

impl QueryResponse for TerminalCapabilityResponse {
    const KIND: TerminalResponseKind = TerminalResponseKind::Dcs;
}

struct TerminalCapabilityQuery {
    name: &'static str,
}

impl Command for TerminalCapabilityQuery {
    fn write_ansi(&self, writer: &mut impl fmt::Write) -> fmt::Result {
        write!(writer, "\x1bP+q{}\x1b\\", hex_encode(self.name.as_bytes()))
    }
}

impl QueryCommand for TerminalCapabilityQuery {
    type Response = TerminalCapabilityResponse;

    fn parse_response(
        &self,
        response: TerminalResponse,
    ) -> Result<Self::Response, TerminalResponse> {
        match parse_terminal_capability(response.as_bytes(), self.name) {
            Some(value) => Ok(TerminalCapabilityResponse(value)),
            None => Err(response),
        }
    }
}

fn query_terminal_capability(
    output: &mut impl Write,
    name: &'static str,
    deadline: Instant,
) -> io::Result<Option<f64>> {
    let Some(timeout) = remaining(deadline) else {
        return Ok(None);
    };
    match event::query(output, TerminalCapabilityQuery { name }, timeout) {
        Ok(TerminalCapabilityResponse(value)) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::TimedOut => Ok(None),
        Err(error) => Err(error),
    }
}

fn probe_logical_dpi(output: &mut impl Write, deadline: Instant) -> io::Result<Option<LogicalDpi>> {
    let Some(x) = query_terminal_capability(output, DPI_X_QUERY, deadline)? else {
        return Ok(None);
    };
    let Some(y) = query_terminal_capability(output, DPI_Y_QUERY, deadline)? else {
        return Ok(None);
    };
    Ok(LogicalDpi::new(x, y))
}

fn parse_terminal_capability(bytes: &[u8], expected_name: &str) -> Option<f64> {
    let body = bytes
        .strip_prefix(b"\x1bP1+r")
        .and_then(|body| body.strip_suffix(b"\x1b\\"))
        .or_else(|| {
            bytes
                .strip_prefix(b"\x901+r")
                .and_then(|body| body.strip_suffix(b"\x9c"))
        })?;
    let separator = body.iter().position(|byte| *byte == b'=')?;
    let (name, value) = (&body[..separator], &body[separator + 1..]);
    let (name, value) = (hex_decode(name)?, hex_decode(value)?);
    if name != expected_name.as_bytes() {
        return None;
    }
    let value = std::str::from_utf8(&value).ok()?.parse::<f64>().ok()?;
    (value.is_finite() && (1.0..=1000.0).contains(&value)).then_some(value)
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
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

fn probe_animation(output: &mut impl Write, deadline: Instant) -> io::Result<bool> {
    let id = std::process::id().wrapping_add(0x575b).max(1);
    let transmitted = query_kitty(
        output,
        format!("\x1b_Gi={id},s=1,v=1,a=t,t=d,f=24,q=0;AAAA\x1b\\"),
        &[id],
        deadline,
    )?
    .is_some_and(|response| response.ok);
    if !transmitted {
        return Ok(false);
    }
    let supported = query_kitty(
        output,
        format!("\x1b_Gi={id},a=f,r=1,x=0,y=0,s=1,v=1,t=d,f=24,X=1,q=0;AAAA\x1b\\"),
        &[id],
        deadline,
    )?
    .is_some_and(|response| response.ok);
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
            let file = unsafe { File::from_raw_fd(fd) };
            let object = Self { name };
            file.set_len(bytes.len() as u64)?;
            if bytes.is_empty() {
                return Ok(object);
            }
            let mapping = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    bytes.len(),
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    file.as_raw_fd(),
                    0,
                )
            };
            if mapping == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapping.cast(), bytes.len());
                if libc::msync(mapping, bytes.len(), libc::MS_SYNC) != 0 {
                    let error = io::Error::last_os_error();
                    libc::munmap(mapping, bytes.len());
                    return Err(error);
                }
                libc::munmap(mapping, bytes.len());
            }
            return Ok(object);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excessive_probe_timeouts_are_bounded_without_panicking() {
        assert!(deadline_after(Duration::MAX) > Instant::now());
    }

    #[test]
    fn expired_deadlines_do_not_emit_queries() {
        let mut output = Vec::new();
        let response = query_kitty(
            &mut output,
            "query".to_owned(),
            &[1],
            Instant::now() - Duration::from_millis(1),
        )
        .unwrap();
        assert!(response.is_none());
        assert!(output.is_empty());
    }

    #[test]
    fn creates_sized_shared_memory_probe_payload() {
        let object = ProbeShm::new(b"rgb").unwrap();
        let fd = unsafe { libc::shm_open(object.name.as_ptr(), libc::O_RDONLY, 0) };
        assert!(fd >= 0);
        let file = unsafe { File::from_raw_fd(fd) };
        assert!(file.metadata().unwrap().len() >= 3);
    }

    #[test]
    fn parses_kitty_acknowledgements() {
        assert!(parse_kitty_response(b"\x1b_Gi=77;OK\x1b\\").unwrap().ok);
        let error = parse_kitty_response(b"\x1b_Gi=77;EINVAL\x1b\\").unwrap();
        assert_eq!(error.id, 77);
        assert!(!error.ok);
    }

    #[test]
    fn parses_valid_logical_dpi_responses() {
        assert_eq!(
            parse_terminal_capability(
                b"\x1bP1+r6b697474792d71756572792d6470695f78=3134342e35\x1b\\",
                DPI_X_QUERY,
            ),
            Some(144.5)
        );
    }

    #[test]
    fn rejects_invalid_logical_dpi_responses() {
        assert_eq!(
            parse_terminal_capability(
                b"\x1bP1+r6b697474792d71756572792d6470695f78=30\x1b\\",
                DPI_X_QUERY,
            ),
            None
        );
        assert_eq!(
            parse_terminal_capability(
                b"\x1bP1+r6b697474792d71756572792d6470695f79=4e614e\x1b\\",
                DPI_Y_QUERY,
            ),
            None
        );
    }
}
