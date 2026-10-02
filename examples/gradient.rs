use std::{io, sync::Arc, thread, time::Duration};

use crossterm::event::{KeyCode, KeyEventKind};
use tilcayo::{Event, Frame, Placement, Rect, Runtime, RuntimeConfig, TerminalSize};

fn main() -> io::Result<()> {
    let mut runtime = Runtime::enter(RuntimeConfig::default())?;
    let result = run(&mut runtime);
    let shutdown = runtime.shutdown();
    result.and(shutdown)
}

fn run(runtime: &mut Runtime) -> io::Result<()> {
    let mut size = runtime.capabilities().size;
    let (mut width, mut height) = framebuffer_size(size);
    let mut serial = 1;
    submit_gradient(runtime, serial, width, height)?;

    loop {
        while let Some(event) = runtime.try_event()? {
            match event {
                Event::Key(key)
                    if key.kind != KeyEventKind::Release
                        && matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) =>
                {
                    return Ok(());
                }
                Event::Resize(new_size) => {
                    size = new_size;
                    (width, height) = framebuffer_size(size);
                    runtime.set_placement(full_placement(size)?);
                    serial += 1;
                    submit_gradient(runtime, serial, width, height)?;
                }
                _ => {}
            }
        }
        if let Some(error) = runtime.output_error() {
            return Err(error);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn submit_gradient(runtime: &Runtime, serial: u64, width: u32, height: u32) -> io::Result<()> {
    let mut pixels = vec![0; width as usize * height as usize * 3];
    for y in 0..height {
        for x in 0..width {
            let offset = (y as usize * width as usize + x as usize) * 3;
            let checker = if (x / 64 + y / 64) % 2 == 0 { 24 } else { 0 };
            pixels[offset] = ((x * 255 / width.max(1)) as u8).saturating_add(checker);
            pixels[offset + 1] = ((y * 255 / height.max(1)) as u8).saturating_add(checker);
            pixels[offset + 2] = (((x ^ y) & 0xff) as u8).saturating_add(checker);
        }
    }
    let frame = Frame::rgb(
        serial,
        width,
        height,
        width as usize * 3,
        Arc::<[u8]>::from(pixels),
        vec![Rect::full(width, height)],
    )?;
    runtime
        .submit(frame)
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "terminal runtime stopped"))
}

fn framebuffer_size(size: TerminalSize) -> (u32, u32) {
    (
        u32::from(size.width_px.unwrap_or(size.columns.saturating_mul(8))).max(1),
        u32::from(size.height_px.unwrap_or(size.rows.saturating_mul(16))).max(1),
    )
}

fn full_placement(size: TerminalSize) -> io::Result<Placement> {
    Placement::new(0, 0, size.columns, size.rows)
}
