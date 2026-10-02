use std::{io, sync::Arc, thread, time::Duration};

use crossterm::event::{KeyCode, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use tilcayo::{
    map_pixel_pointer, Event, Frame, Placement, Rect, Runtime, RuntimeConfig, TerminalSize,
};

fn main() -> io::Result<()> {
    let mut runtime = Runtime::enter(RuntimeConfig::default())?;
    let result = run(&mut runtime);
    let shutdown = runtime.shutdown();
    result.and(shutdown)
}

fn run(runtime: &mut Runtime) -> io::Result<()> {
    let mut size = runtime.capabilities().size;
    let mut placement = runtime.placement();
    let (mut width, mut height) = framebuffer_size(size);
    let mut pixels = blank_canvas(width, height);
    let mut serial = 1;
    let mut painting = false;
    submit(
        runtime,
        serial,
        width,
        height,
        &pixels,
        Rect::full(width, height),
    )?;

    loop {
        while let Some(event) = runtime.try_event()? {
            match event {
                Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char(character) if character.eq_ignore_ascii_case(&'c') => {
                        pixels = blank_canvas(width, height);
                        serial += 1;
                        submit(
                            runtime,
                            serial,
                            width,
                            height,
                            &pixels,
                            Rect::full(width, height),
                        )?;
                    }
                    _ => {}
                },
                Event::Pointer(event) => {
                    if let Some((damage, center, color)) =
                        pointer_stroke(event, &mut painting, size, placement, (width, height))
                    {
                        draw_disc(&mut pixels, width, height, damage, center, color);
                        serial += 1;
                        submit(runtime, serial, width, height, &pixels, damage)?;
                    }
                }
                Event::Focus(false) => painting = false,
                Event::Resize(new_size) => {
                    size = new_size;
                    placement = full_placement(size)?;
                    runtime.set_placement(placement);
                    (width, height) = framebuffer_size(size);
                    pixels = blank_canvas(width, height);
                    painting = false;
                    serial += 1;
                    submit(
                        runtime,
                        serial,
                        width,
                        height,
                        &pixels,
                        Rect::full(width, height),
                    )?;
                }
                _ => {}
            }
        }
        if let Some(error) = runtime.output_error() {
            return Err(error);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn pointer_stroke(
    event: MouseEvent,
    painting: &mut bool,
    terminal: TerminalSize,
    placement: Placement,
    output: (u32, u32),
) -> Option<(Rect, (u32, u32), [u8; 3])> {
    let button = match event.kind {
        MouseEventKind::Down(button) => {
            *painting = true;
            button
        }
        MouseEventKind::Drag(button) => {
            *painting = true;
            button
        }
        MouseEventKind::Up(_) => {
            *painting = false;
            return None;
        }
        MouseEventKind::Moved if *painting => MouseButton::Left,
        _ => return None,
    };
    let (x, y) = pointer_position(event, terminal, placement, output)?;
    let radius = 10;
    let damage = Rect::new(
        x.saturating_sub(radius),
        y.saturating_sub(radius),
        radius * 2 + 1,
        radius * 2 + 1,
    )
    .clip(output.0, output.1)?;
    let color = match button {
        MouseButton::Left => [255, 80, 70],
        MouseButton::Right => [70, 150, 255],
        MouseButton::Middle => [255, 220, 70],
    };
    Some((damage, (x, y), color))
}

fn pointer_position(
    event: MouseEvent,
    terminal: TerminalSize,
    placement: Placement,
    output: (u32, u32),
) -> Option<(u32, u32)> {
    let position = map_pixel_pointer(event.column, event.row, terminal, placement, output)
        .or_else(|| {
            (terminal.width_px.is_none() || terminal.height_px.is_none()).then(|| {
                (
                    f64::from(event.column) * f64::from(output.0)
                        / f64::from(terminal.columns.max(1)),
                    f64::from(event.row) * f64::from(output.1) / f64::from(terminal.rows.max(1)),
                )
            })
        })?;
    Some((
        (position.0 as u32).min(output.0.saturating_sub(1)),
        (position.1 as u32).min(output.1.saturating_sub(1)),
    ))
}

fn draw_disc(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    bounds: Rect,
    center: (u32, u32),
    color: [u8; 3],
) {
    let (center_x, center_y) = center;
    let radius = 10_u32;
    for y in bounds.y..(bounds.y + bounds.height).min(height) {
        for x in bounds.x..(bounds.x + bounds.width).min(width) {
            let dx = i64::from(x) - i64::from(center_x);
            let dy = i64::from(y) - i64::from(center_y);
            if dx * dx + dy * dy <= i64::from(radius * radius) {
                let offset = (y as usize * width as usize + x as usize) * 3;
                pixels[offset..offset + 3].copy_from_slice(&color);
            }
        }
    }
}

fn submit(
    runtime: &Runtime,
    serial: u64,
    width: u32,
    height: u32,
    pixels: &[u8],
    damage: Rect,
) -> io::Result<()> {
    let frame = Frame::rgb(
        serial,
        width,
        height,
        width as usize * 3,
        Arc::<[u8]>::from(pixels),
        vec![damage],
    )?;
    runtime
        .submit(frame)
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "terminal runtime stopped"))
}

fn blank_canvas(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = vec![0; width as usize * height as usize * 3];
    for chunk in pixels.chunks_exact_mut(3) {
        chunk.copy_from_slice(&[18, 20, 26]);
    }
    pixels
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
