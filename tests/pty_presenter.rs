#![cfg(unix)]

use std::{
    fs::File,
    io::{Read, Write},
    mem::MaybeUninit,
    os::fd::FromRawFd,
    sync::Arc,
};

use tilcayo::kitty::{
    probe, GraphicsTransport, KittyPresenter, Placement, TransferOptions, ZlibPolicy,
};
use tilcayo::{Frame, Rect};

/// PTY-backed fake terminal: records the exact stream emitted to a terminal
/// device rather than relying only on a Vec-backed unit test.
#[test]
fn presenter_writes_complete_commands_through_a_pty() {
    let mut master = -1;
    let mut slave = -1;
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(result, 0, "openpty: {}", std::io::Error::last_os_error());
    let mut master = unsafe { File::from_raw_fd(master) };
    let mut slave = unsafe { File::from_raw_fd(slave) };

    // Disable output translations so the fake terminal sees exact protocol bytes.
    let mut attributes = MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(slave.as_raw_fd(), attributes.as_mut_ptr()) },
        0
    );
    let mut attributes = unsafe { attributes.assume_init() };
    unsafe { libc::cfmakeraw(&mut attributes) };
    assert_eq!(
        unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &attributes) },
        0
    );

    let frame = Frame::rgb(
        42,
        2,
        2,
        6,
        Arc::<[u8]>::from(vec![0x7f; 12]),
        vec![Rect::full(2, 2)],
    )
    .unwrap();
    let mut presenter = KittyPresenter::new(9, false);
    presenter.set_transfer_options(TransferOptions {
        transport: GraphicsTransport::Direct,
        zlib: ZlibPolicy::Never,
        chunk_size: 4096,
    });
    presenter
        .present(&mut slave, &frame, Placement::new(0, 0, 2, 2).unwrap())
        .unwrap();
    slave.flush().unwrap();

    let mut descriptor = libc::pollfd {
        fd: master.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 1000) }, 1);
    let mut output = Vec::new();
    loop {
        let mut bytes = [0; 4096];
        let count = master.read(&mut bytes).unwrap();
        output.extend_from_slice(&bytes[..count]);
        if output.ends_with(b"\x1b\\\x1b[u") {
            break;
        }
    }
    let output = String::from_utf8_lossy(&output);
    assert!(output.starts_with("\u{1b}[s\u{1b}[1;1H\u{1b}_G"));
    assert!(output.contains("a=T,f=24,s=2,v=2,i=9"));
}

#[test]
fn fake_terminal_answers_fragmented_capability_probe() {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let mut master = unsafe { File::from_raw_fd(master_fd) };
    let mut input = unsafe { File::from_raw_fd(slave_fd) };
    let mut output = input.try_clone().unwrap();
    let mut attributes = MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(input.as_raw_fd(), attributes.as_mut_ptr()) },
        0
    );
    let mut attributes = unsafe { attributes.assume_init() };
    unsafe { libc::cfmakeraw(&mut attributes) };
    assert_eq!(
        unsafe { libc::tcsetattr(input.as_raw_fd(), libc::TCSANOW, &attributes) },
        0
    );

    let (release_sender, release_receiver) = std::sync::mpsc::channel();
    let terminal = std::thread::spawn(move || {
        let mut pending = Vec::new();
        loop {
            let mut bytes = [0; 512];
            let count = master.read(&mut bytes).unwrap();
            pending.extend_from_slice(&bytes[..count]);
            while let Some(end) = pending.windows(2).position(|bytes| bytes == b"\x1b\\") {
                let command: Vec<_> = pending.drain(..end + 2).collect();
                let command = String::from_utf8_lossy(&command);
                if command.contains("q=0") {
                    let start = command.find("i=").unwrap() + 2;
                    let end = command[start..].find(',').unwrap() + start;
                    let id = &command[start..end];
                    master.write_all(b"\x1b_").unwrap();
                    master.write_all(format!("Gi={id}").as_bytes()).unwrap();
                    master.write_all(b";O").unwrap();
                    master.write_all(b"K\x1b\\").unwrap();
                }
                if command.contains("a=f") {
                    release_receiver.recv().unwrap();
                    return;
                }
            }
        }
    });
    let capabilities = probe(&mut input, &mut output, std::time::Duration::from_secs(1)).unwrap();
    release_sender.send(()).unwrap();
    terminal.join().unwrap();
    assert!(capabilities.graphics);
    assert!(capabilities.animation);
}

use std::os::fd::AsRawFd;
