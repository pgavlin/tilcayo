use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use base64::Engine as _;
use flate2::{write::ZlibEncoder, Compression};

static NEXT_TRANSFER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ZlibPolicy {
    Never,
    Adaptive,
    Always,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphicsTransport {
    Auto,
    Direct,
    TemporaryFile,
    SharedMemory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferMedium {
    Direct,
    TemporaryFile,
    SharedMemory,
}

#[derive(Clone, Copy, Debug)]
pub struct TransferOptions {
    pub transport: GraphicsTransport,
    pub zlib: ZlibPolicy,
    pub chunk_size: usize,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            transport: GraphicsTransport::Auto,
            zlib: ZlibPolicy::Adaptive,
            chunk_size: 4096,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransferStats {
    pub payload_bytes: usize,
    pub medium: TransferMedium,
    pub compressed: bool,
}

#[derive(Debug)]
pub struct TransportManager {
    shared_memory: bool,
    temporary_file: bool,
}

impl TransportManager {
    pub fn detect() -> Self {
        let local_media = std::env::var_os("KITTY_WINDOW_ID").is_some()
            && std::env::var_os("SSH_CONNECTION").is_none()
            && std::env::var_os("TMUX").is_none()
            && std::env::var_os("STY").is_none();
        Self::new(local_media)
    }

    pub fn new(local_media: bool) -> Self {
        Self {
            shared_memory: local_media,
            temporary_file: local_media,
        }
    }

    pub(crate) fn probed(shared_memory: bool, temporary_file: bool) -> Self {
        Self {
            shared_memory,
            temporary_file,
        }
    }

    pub fn transmit(
        &mut self,
        writer: &mut impl Write,
        control: &str,
        bytes: &[u8],
        animation: bool,
        options: TransferOptions,
    ) -> io::Result<TransferStats> {
        if options.chunk_size == 0 || options.chunk_size > 4096 || options.chunk_size % 4 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Kitty chunks must be a nonzero multiple of four at most 4096 bytes",
            ));
        }
        let auto_shared = options.transport == GraphicsTransport::Auto && self.shared_memory;
        let auto_temporary = options.transport == GraphicsTransport::Auto && self.temporary_file;
        let local = auto_shared
            || auto_temporary
            || matches!(
                options.transport,
                GraphicsTransport::TemporaryFile | GraphicsTransport::SharedMemory
            );
        let compressed = if options.zlib == ZlibPolicy::Always
            || (options.zlib == ZlibPolicy::Adaptive && !local)
        {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(bytes)?;
            Some(encoder.finish()?)
        } else {
            None
        };
        let use_compressed = match (&compressed, options.zlib) {
            (Some(_), ZlibPolicy::Always) => true,
            (Some(value), ZlibPolicy::Adaptive) => value.len() < bytes.len(),
            _ => false,
        };
        let payload = if use_compressed {
            compressed.as_deref().unwrap()
        } else {
            bytes
        };
        let compression = if use_compressed { ",o=z" } else { "" };

        if options.transport == GraphicsTransport::SharedMemory || auto_shared {
            match self.shared_payload(payload) {
                Ok(mut object) => {
                    let name =
                        base64::engine::general_purpose::STANDARD.encode(object.name.as_bytes());
                    write!(
                        writer,
                        "\x1b_G{control}{compression},t=s,S={};{name}\x1b\\",
                        payload.len()
                    )?;
                    writer.flush()?;
                    object.disarm(); // Kitty owns removal after a successful command flush.
                    return Ok(TransferStats {
                        payload_bytes: payload.len(),
                        medium: TransferMedium::SharedMemory,
                        compressed: use_compressed,
                    });
                }
                Err(error) if options.transport == GraphicsTransport::SharedMemory => {
                    return Err(error)
                }
                Err(_) => {}
            }
        }

        if options.transport == GraphicsTransport::TemporaryFile || auto_temporary {
            let mut object = self.temporary_payload(payload)?;
            let path = base64::engine::general_purpose::STANDARD
                .encode(object.path.as_os_str().as_encoded_bytes());
            write!(
                writer,
                "\x1b_G{control}{compression},t=t,S={};{path}\x1b\\",
                payload.len()
            )?;
            writer.flush()?;
            object.disarm(); // Kitty deletes t=t files after reading them.
            return Ok(TransferStats {
                payload_bytes: payload.len(),
                medium: TransferMedium::TemporaryFile,
                compressed: use_compressed,
            });
        }

        let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
        for (index, chunk) in encoded.as_bytes().chunks(options.chunk_size).enumerate() {
            let more = usize::from((index + 1) * options.chunk_size < encoded.len());
            if index == 0 {
                write!(writer, "\x1b_G{control}{compression},t=d,m={more};")?;
            } else if animation {
                write!(writer, "\x1b_Ga=f,m={more};")?;
            } else {
                write!(writer, "\x1b_Gm={more};")?;
            }
            writer.write_all(chunk)?;
            writer.write_all(b"\x1b\\")?;
        }
        Ok(TransferStats {
            payload_bytes: payload.len(),
            medium: TransferMedium::Direct,
            compressed: use_compressed,
        })
    }

    fn shared_payload(&mut self, payload: &[u8]) -> io::Result<ShmObject> {
        loop {
            let name = CString::new(format!(
                "/tilcayo-gfx-{:x}-{:x}",
                std::process::id(),
                NEXT_TRANSFER.fetch_add(1, Ordering::Relaxed)
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
            let mut object = ShmObject { name, armed: true };
            file.set_len(payload.len() as u64)?;
            if payload.is_empty() {
                return Ok(object);
            }
            let mapping = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    payload.len(),
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
                std::ptr::copy_nonoverlapping(payload.as_ptr(), mapping.cast(), payload.len());
                if libc::msync(mapping, payload.len(), libc::MS_SYNC) != 0 {
                    let error = io::Error::last_os_error();
                    libc::munmap(mapping, payload.len());
                    return Err(error);
                }
                libc::munmap(mapping, payload.len());
            }
            object.armed = true;
            return Ok(object);
        }
    }

    fn temporary_payload(&mut self, payload: &[u8]) -> io::Result<TemporaryObject> {
        loop {
            // The protocol only permits terminals to delete temporary files
            // whose paths contain this marker.
            let path = std::env::temp_dir().join(format!(
                "tilcayo-tty-graphics-protocol-{}-{}",
                std::process::id(),
                NEXT_TRANSFER.fetch_add(1, Ordering::Relaxed)
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let mut object = TemporaryObject { path, armed: true };
                    file.write_all(payload)?;
                    file.flush()?;
                    object.armed = true;
                    return Ok(object);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

struct ShmObject {
    name: CString,
    armed: bool,
}
impl ShmObject {
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for ShmObject {
    fn drop(&mut self) {
        if self.armed {
            unsafe {
                libc::shm_unlink(self.name.as_ptr());
            }
        }
    }
}

struct TemporaryObject {
    path: PathBuf,
    armed: bool,
}
impl TemporaryObject {
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for TemporaryObject {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_transfer_is_chunked_and_adaptively_compressed() {
        let mut manager = TransportManager::new(false);
        let mut output = Vec::new();
        let stats = manager
            .transmit(
                &mut output,
                "a=f,i=1",
                &vec![0xa5; 30_000],
                true,
                TransferOptions::default(),
            )
            .unwrap();
        assert_eq!(stats.medium, TransferMedium::Direct);
        assert!(stats.compressed);
        assert!(String::from_utf8(output).unwrap().contains(",o=z,t=d,"));
    }

    #[test]
    fn auto_uses_shared_memory_for_small_local_updates() {
        let mut manager = TransportManager::new(true);
        let mut output = Vec::new();
        let stats = manager
            .transmit(
                &mut output,
                "a=f,i=1",
                &[0xa5; 768],
                true,
                TransferOptions::default(),
            )
            .unwrap();
        assert_eq!(stats.medium, TransferMedium::SharedMemory);
        assert!(!stats.compressed);

        // The fake terminal did not consume and unlink the object.
        let command = String::from_utf8(output).unwrap();
        let encoded = command
            .split_once(';')
            .unwrap()
            .1
            .strip_suffix("\x1b\\")
            .unwrap();
        let name = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let name = CString::new(name).unwrap();
        assert_eq!(unsafe { libc::shm_unlink(name.as_ptr()) }, 0);
    }

    #[test]
    fn invalid_chunks_are_rejected() {
        let mut manager = TransportManager::new(false);
        let options = TransferOptions {
            chunk_size: 3,
            ..TransferOptions::default()
        };
        assert!(manager
            .transmit(&mut Vec::new(), "a=T", b"x", false, options)
            .is_err());
    }

    #[test]
    fn armed_transfer_objects_clean_up_on_error_paths() {
        let mut manager = TransportManager::new(false);
        let shm = manager.shared_payload(b"content").unwrap();
        let name = shm.name.clone();
        drop(shm);
        let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) };
        assert_eq!(fd, -1);

        let temporary = manager.temporary_payload(b"content").unwrap();
        let path = temporary.path.clone();
        assert!(path.to_string_lossy().contains("tty-graphics-protocol"));
        assert!(path.exists());
        drop(temporary);
        assert!(!path.exists());
    }

    struct FailFlush(Vec<u8>);
    impl Write for FailFlush {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "fake terminal closed",
            ))
        }
    }

    #[test]
    fn failed_flush_removes_shared_memory() {
        let mut manager = TransportManager::new(false);
        let mut writer = FailFlush(Vec::new());
        let options = TransferOptions {
            transport: GraphicsTransport::SharedMemory,
            zlib: ZlibPolicy::Never,
            chunk_size: 4096,
        };
        assert!(manager
            .transmit(&mut writer, "a=T", b"content", false, options)
            .is_err());
        let command = String::from_utf8(writer.0).unwrap();
        let encoded = command
            .split_once(';')
            .unwrap()
            .1
            .strip_suffix("\x1b\\")
            .unwrap();
        let name = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let name = CString::new(name).unwrap();
        assert_eq!(
            unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) },
            -1
        );
    }
}
