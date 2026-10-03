// This example's geometry and animation model are derived from Martin Backschat's
// MIT-licensed AMICUS Boing Ball port: https://github.com/mbackschat/amiga-boing-web
// It intentionally omits that project's Workbench layer and audio.

use std::{f64::consts::PI, io, sync::Arc, time::Duration, time::Instant};

use tilcayo::{
    Event, Frame, KeyCode, KeyEventKind, Placement, Rect, Runtime, RuntimeConfig, TerminalSize,
};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 216;
const BALL_WIDTH: u32 = 336;
const BALL_HEIGHT: u32 = 216;
const BALL_CENTER_X: i32 = 168;
const BALL_CENTER_Y: i32 = 108;
const PHYSICS_STEP: Duration = Duration::from_nanos(1_000_000_000 / 60);
const MAX_FRAME_TIME: Duration = Duration::from_millis(200);

const SKY: [u8; 3] = [0xaa, 0xaa, 0xaa];
const RIM: [u8; 3] = [0x66, 0x66, 0x66];
const MAGENTA: [u8; 3] = [0xaa, 0x00, 0xaa];
const DARK_MAGENTA: [u8; 3] = [0x66, 0x00, 0x66];
const WHITE: [u8; 3] = [0xff, 0xff, 0xff];
const RED: [u8; 3] = [0xff, 0x00, 0x00];
const PINK: [u8; 3] = [0xff, 0xdd, 0xdd];

fn main() -> io::Result<()> {
    let mut runtime = Runtime::enter(RuntimeConfig::default())?;
    let result = run(&mut runtime);
    let shutdown = runtime.shutdown();
    result.and(shutdown)
}

fn run(runtime: &mut Runtime) -> io::Result<()> {
    let mut terminal_size = runtime.capabilities().size;
    runtime.set_placement(demo_placement(terminal_size)?);

    let mut scene = Scene::new();
    let mut serial = 0_u64;
    let mut ready = true;
    let mut redraw = true;
    let mut force_full = true;
    let mut paused = false;
    let mut accumulator = Duration::ZERO;
    let mut sampled_at = Instant::now();
    let mut next_frame = sampled_at;

    loop {
        let now = Instant::now();
        if !paused {
            let elapsed = now
                .saturating_duration_since(sampled_at)
                .min(MAX_FRAME_TIME);
            accumulator += elapsed;
            while accumulator >= PHYSICS_STEP {
                scene.step();
                accumulator -= PHYSICS_STEP;
                redraw = true;
            }
        }
        sampled_at = now;

        if ready && redraw && (paused || now >= next_frame) {
            serial = serial.wrapping_add(1).max(1);
            let (pixels, damage) = scene.render(force_full);
            let frame = Frame::rgb(
                serial,
                WIDTH,
                HEIGHT,
                WIDTH as usize * 3,
                Arc::<[u8]>::from(pixels),
                vec![damage],
            )?;
            runtime.submit(frame).map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "terminal runtime stopped")
            })?;
            ready = false;
            redraw = false;
            force_full = false;
            next_frame = now + PHYSICS_STEP;
        }

        let timeout = if ready && !paused {
            Some(next_frame.saturating_duration_since(Instant::now()))
        } else {
            None
        };
        if runtime.wakeup().wait(timeout)? {
            runtime.wakeup().clear()?;
        }

        let event_time = Instant::now();
        if !paused {
            accumulator += event_time
                .saturating_duration_since(sampled_at)
                .min(MAX_FRAME_TIME);
        }
        sampled_at = event_time;

        while let Some(event) = runtime.try_event()? {
            match event {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char(' ') => {
                        paused = !paused;
                        accumulator = Duration::ZERO;
                        redraw = true;
                        next_frame = event_time;
                    }
                    KeyCode::Char(character) if character.eq_ignore_ascii_case(&'r') => {
                        scene.reset();
                        accumulator = Duration::ZERO;
                        redraw = true;
                        next_frame = event_time;
                    }
                    _ => {}
                },
                Event::Resize(new_size) => {
                    terminal_size = new_size;
                    runtime.set_placement(demo_placement(terminal_size)?);
                    redraw = true;
                    next_frame = event_time;
                }
                _ => {}
            }
        }

        if runtime.take_completion().is_some() {
            ready = true;
        }
        if let Some(error) = runtime.output_error() {
            return Err(error);
        }
        if runtime.is_stopped() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "terminal runtime stopped",
            ));
        }
    }
}

struct Scene {
    room: Vec<u8>,
    ball_bitmap: Vec<u8>,
    ball_bitmap_bounds: Rect,
    palette: [[u8; 3]; 32],
    pixels: Vec<u8>,
    physics: Physics,
    previous_bounds: Option<Rect>,
}

impl Scene {
    fn new() -> Self {
        let mut room = vec![0_u8; WIDTH as usize * HEIGHT as usize];
        draw_room(&mut room);

        let mut ball_bitmap = vec![0_u8; BALL_WIDTH as usize * BALL_HEIGHT as usize];
        draw_static_ball(&mut ball_bitmap);
        let ball_bitmap_bounds = nonzero_bounds(&ball_bitmap, BALL_WIDTH, BALL_HEIGHT)
            .expect("the Boing Ball bitmap is nonempty");

        let mut scene = Self {
            room,
            ball_bitmap,
            ball_bitmap_bounds,
            palette: [[0; 3]; 32],
            pixels: vec![0; WIDTH as usize * HEIGHT as usize * 3],
            physics: Physics::new(),
            previous_bounds: None,
        };
        scene.initialize_palette();
        scene
    }

    fn initialize_palette(&mut self) {
        self.palette[0] = SKY;
        self.palette[1] = RIM;
        self.palette[16] = MAGENTA;
        self.palette[17] = DARK_MAGENTA;
        self.update_palette();
    }

    fn update_palette(&mut self) {
        let phase = self.physics.rotation_phase as usize;
        for i in 0..7 {
            let slot = (i + phase) % 14 + 2;
            self.palette[slot] = WHITE;
            self.palette[slot + 16] = WHITE;
        }
        for i in 7..14 {
            let slot = (i + phase) % 14 + 2;
            self.palette[slot] = RED;
            self.palette[slot + 16] = RED;
        }
        let highlight = ((if self.physics.velocity_x >= 0 { 0 } else { 6 }) + phase) % 14 + 2;
        self.palette[highlight] = PINK;
        self.palette[highlight + 16] = PINK;
    }

    fn step(&mut self) {
        self.physics.step();
        self.update_palette();
    }

    fn reset(&mut self) {
        self.physics = Physics::new();
        self.update_palette();
    }

    fn render(&mut self, force_full: bool) -> (Vec<u8>, Rect) {
        let bounds = translated_bounds(self.ball_bitmap_bounds, self.physics.x, self.physics.y);
        let damage = if force_full {
            Rect::full(WIDTH, HEIGHT)
        } else {
            self.previous_bounds
                .map_or(bounds, |previous| previous.union(bounds))
        };
        self.composite(damage);
        self.previous_bounds = Some(bounds);
        (self.pixels.clone(), damage)
    }

    fn composite(&mut self, rect: Rect) {
        let offset_x = BALL_CENTER_X - self.physics.x;
        let offset_y = BALL_CENTER_Y - self.physics.y;
        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                let bitmap_x = x as i32 + offset_x;
                let bitmap_y = y as i32 + offset_y;
                let ball_index = if bitmap_x >= 0
                    && bitmap_x < BALL_WIDTH as i32
                    && bitmap_y >= 0
                    && bitmap_y < BALL_HEIGHT as i32
                {
                    self.ball_bitmap[bitmap_y as usize * BALL_WIDTH as usize + bitmap_x as usize]
                } else {
                    0
                };
                let room_bit = self.room[y as usize * WIDTH as usize + x as usize];
                let palette_index = match ball_index {
                    0 if room_bit != 0 => 16,
                    0 => 0,
                    1 if room_bit != 0 => 17,
                    1 => 1,
                    value if room_bit != 0 => value + 16,
                    value => value,
                };
                let output = (y as usize * WIDTH as usize + x as usize) * 3;
                self.pixels[output..output + 3]
                    .copy_from_slice(&self.palette[palette_index as usize]);
            }
        }
    }
}

struct Physics {
    x: i32,
    y: i32,
    velocity_x: i32,
    velocity_y: i32,
    float_y: i32,
    rotation_phase: u8,
}

impl Physics {
    fn new() -> Self {
        Self {
            x: 160,
            y: 56,
            velocity_x: 1,
            velocity_y: 0,
            float_y: 1,
            rotation_phase: 0,
        }
    }

    fn step(&mut self) {
        self.rotation_phase = if self.velocity_x >= 0 {
            (self.rotation_phase + 13) % 14
        } else {
            (self.rotation_phase + 1) % 14
        };

        self.float_y += self.velocity_y / 10;
        if self.float_y > 96 {
            self.float_y = 192 - self.float_y;
            self.velocity_y = -self.velocity_y;
        }
        self.y = self.float_y + 55;

        self.x += self.velocity_x;
        if self.x <= 80 {
            self.x = 160 - self.x;
            self.velocity_x = -self.velocity_x;
        }
        if self.x >= 264 {
            self.x = 528 - self.x;
            self.velocity_x = -self.velocity_x;
        }
        self.velocity_y += 1;
    }
}

#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}

#[derive(Clone, Copy)]
struct Vertex {
    point: Point,
    color: u8,
}

fn draw_static_ball(bitmap: &mut [u8]) {
    draw_shadow(bitmap);
    let vertices = generate_vertices();
    let mut quad = [Point { x: 0.0, y: 0.0 }; 4];
    for latitude in 0..8 {
        for longitude in 0..55 {
            let top = latitude * 56;
            let bottom = (latitude + 1) * 56;
            quad[0] = vertices[top + longitude].point;
            quad[1] = vertices[top + longitude + 1].point;
            quad[2] = vertices[bottom + longitude + 1].point;
            quad[3] = vertices[bottom + longitude].point;
            fill_polygon(
                bitmap,
                BALL_WIDTH,
                BALL_HEIGHT,
                &quad,
                vertices[top + longitude].color,
            );
        }
    }
}

fn draw_shadow(bitmap: &mut [u8]) {
    let mut points = [Point { x: 0.0, y: 0.0 }; 16];
    for (index, point) in points.iter_mut().enumerate() {
        let angle = 12.0_f64.to_radians() + index as f64 / 16.0 * 2.0 * PI;
        *point = Point {
            x: f64::from(BALL_CENTER_X) + 25.0 + 55.0 * angle.cos(),
            y: f64::from(BALL_CENTER_Y) + 50.0 * angle.sin(),
        };
    }
    fill_polygon(bitmap, BALL_WIDTH, BALL_HEIGHT, &points, 1);
}

fn generate_vertices() -> Vec<Vertex> {
    let mut vertices = Vec::with_capacity(9 * 56);
    for latitude in 0..9 {
        let theta = latitude as f64 / 8.0 * PI;
        for longitude in 0..56 {
            let phi = longitude as f64 / 56.0 * PI;
            let x = 80.0 * theta.sin() * phi.cos();
            let y = 80.0 * theta.cos();
            let color = (((latitude & 1) * 7 + longitude) % 14 + 2) as u8;
            vertices.push(Vertex {
                point: Point {
                    x: f64::from(BALL_CENTER_X) + (y / 2.0 + x * 1.6875) * 0.4,
                    y: f64::from(BALL_CENTER_Y) - (y * 1.4375 - x / 2.0) * 0.4,
                },
                color,
            });
        }
    }
    vertices
}

fn fill_polygon(buffer: &mut [u8], width: u32, height: u32, points: &[Point], value: u8) {
    let minimum_y = points
        .iter()
        .map(|point| point.y)
        .fold(f64::INFINITY, f64::min);
    let maximum_y = points
        .iter()
        .map(|point| point.y)
        .fold(f64::NEG_INFINITY, f64::max);
    let start_y = minimum_y.ceil().max(0.0) as i32;
    let end_y = maximum_y.floor().min(f64::from(height - 1)) as i32;
    let mut intersections = Vec::with_capacity(points.len());

    for y in start_y..=end_y {
        intersections.clear();
        let scanline = f64::from(y);
        for index in 0..points.len() {
            let a = points[index];
            let b = points[(index + 1) % points.len()];
            let edge_min = a.y.min(b.y);
            let edge_max = a.y.max(b.y);
            if scanline < edge_min || scanline >= edge_max {
                continue;
            }
            let position = (scanline - a.y) / (b.y - a.y);
            intersections.push(a.x + position * (b.x - a.x));
        }
        intersections.sort_by(f64::total_cmp);
        for pair in intersections.chunks_exact(2) {
            let start_x = pair[0].ceil().max(0.0) as i32;
            let end_x = pair[1].floor().min(f64::from(width - 1)) as i32;
            for x in start_x..=end_x {
                buffer[y as usize * width as usize + x as usize] = value;
            }
        }
    }
}

fn draw_room(buffer: &mut [u8]) {
    for x in (48..300).step_by(16) {
        draw_line(buffer, WIDTH, HEIGHT, x, 0, x, 192, 1);
    }
    for y in (0..=200).step_by(16) {
        draw_line(buffer, WIDTH, HEIGHT, 48, y, 288, y, 1);
    }
    for x in (48..300).step_by(16) {
        let end_x = 160 + ((x - 160) * 5) / 4;
        draw_line(buffer, WIDTH, HEIGHT, x, 192, end_x, 215, 1);
    }
    for (y, left, right) in [
        (194, 45, 291),
        (197, 41, 295),
        (201, 37, 300),
        (207, 30, 308),
    ] {
        draw_line(buffer, WIDTH, HEIGHT, left, y, right, y, 1);
    }
    draw_line(buffer, WIDTH, HEIGHT, 20, 215, 319, 215, 1);
}

#[allow(clippy::too_many_arguments)]
fn draw_line(
    buffer: &mut [u8],
    width: u32,
    height: u32,
    mut x0: i32,
    mut y0: i32,
    x1: i32,
    y1: i32,
    value: u8,
) {
    let delta_x = (x1 - x0).abs();
    let delta_y = -(y1 - y0).abs();
    let step_x = if x0 < x1 { 1 } else { -1 };
    let step_y = if y0 < y1 { 1 } else { -1 };
    let mut error = delta_x + delta_y;
    loop {
        if x0 >= 0 && x0 < width as i32 && y0 >= 0 && y0 < height as i32 {
            buffer[y0 as usize * width as usize + x0 as usize] = value;
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let doubled = 2 * error;
        if doubled >= delta_y {
            error += delta_y;
            x0 += step_x;
        }
        if doubled <= delta_x {
            error += delta_x;
            y0 += step_y;
        }
    }
}

fn nonzero_bounds(buffer: &[u8], width: u32, height: u32) -> Option<Rect> {
    let mut left = width;
    let mut top = height;
    let mut right = 0;
    let mut bottom = 0;
    let mut found = false;
    for y in 0..height {
        for x in 0..width {
            if buffer[y as usize * width as usize + x as usize] != 0 {
                found = true;
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
    }
    found.then(|| Rect::new(left, top, right - left, bottom - top))
}

fn translated_bounds(bitmap: Rect, center_x: i32, center_y: i32) -> Rect {
    let left = center_x + bitmap.x as i32 - BALL_CENTER_X;
    let top = center_y + bitmap.y as i32 - BALL_CENTER_Y;
    let right = left + bitmap.width as i32;
    let bottom = top + bitmap.height as i32;
    rect_from_edges(left, top, right, bottom)
}

fn rect_from_edges(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
    let left = left.clamp(0, WIDTH as i32) as u32;
    let top = top.clamp(0, HEIGHT as i32) as u32;
    let right = right.clamp(0, WIDTH as i32) as u32;
    let bottom = bottom.clamp(0, HEIGHT as i32) as u32;
    Rect::new(
        left,
        top,
        right.saturating_sub(left),
        bottom.saturating_sub(top),
    )
}

fn demo_placement(size: TerminalSize) -> io::Result<Placement> {
    let columns = size.columns.max(1);
    let rows = size.rows.max(1);
    let cell_width =
        f64::from(size.width_px.unwrap_or(columns.saturating_mul(8))) / f64::from(columns);
    let cell_height =
        f64::from(size.height_px.unwrap_or(rows.saturating_mul(16))) / f64::from(rows);
    let available_width = cell_width * f64::from(columns);
    let available_height = cell_height * f64::from(rows);
    let aspect = f64::from(WIDTH) / f64::from(HEIGHT);

    let (placement_columns, placement_rows) = if available_width / available_height > aspect {
        let desired = (available_height * aspect / cell_width).round() as u16;
        (desired.clamp(1, columns), rows)
    } else {
        let desired = (available_width / aspect / cell_height).round() as u16;
        (columns, desired.clamp(1, rows))
    };
    Placement::new(
        (columns - placement_columns) / 2,
        (rows - placement_rows) / 2,
        placement_columns,
        placement_rows,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_geometry_has_expected_extents() {
        let scene = Scene::new();
        assert_eq!(scene.ball_bitmap_bounds, Rect::new(113, 59, 135, 99));
        assert_eq!(
            translated_bounds(scene.ball_bitmap_bounds, 160, 56),
            Rect::new(105, 7, 135, 99)
        );
    }

    #[test]
    fn source_physics_repeats_without_drift() {
        let mut physics = Physics::new();
        let initial = (physics.x, physics.y, physics.velocity_x, physics.velocity_y);
        for _ in 0..17_664 {
            physics.step();
        }
        assert_eq!(
            (physics.x, physics.y, physics.velocity_x, physics.velocity_y),
            initial
        );
    }

    #[test]
    fn full_render_uses_the_indexed_palette() {
        let mut scene = Scene::new();
        let (pixels, damage) = scene.render(true);
        assert_eq!(damage, Rect::full(WIDTH, HEIGHT));
        assert!(pixels.chunks_exact(3).any(|pixel| pixel == MAGENTA));
        assert!(pixels.chunks_exact(3).any(|pixel| pixel == RED));
        assert!(pixels.chunks_exact(3).any(|pixel| pixel == RIM));
    }
}
