use std::{
    io,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use tilcayo::{
    kitty::{PresentStats, TransferMedium},
    Event, Frame, KeyCode, KeyEventKind, Placement, PresentationObserver, Rect, Runtime,
    RuntimeConfig, TerminalSize,
};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const HUD_HEIGHT: u32 = 76;
const FRAME_TIME: Duration = Duration::from_nanos(1_000_000_000 / 60);
const MAX_SPRITES: usize = 10;

fn main() -> io::Result<()> {
    let stats = Arc::new(Stats::default());
    let mut runtime = Runtime::enter_instrumented(RuntimeConfig::default(), stats.clone())?;
    let result = run(&mut runtime, &stats);
    let shutdown = runtime.shutdown();
    result.and(shutdown)
}

fn run(runtime: &mut Runtime, stats: &Stats) -> io::Result<()> {
    let mut terminal_size = runtime.capabilities().size;
    runtime.set_placement(demo_placement(terminal_size)?);
    let capabilities = runtime.graphics_capabilities();
    let mut lab = Lab::new();
    let mut serial = 0_u64;
    let mut ready = true;
    let mut paused = false;
    let mut paced = true;
    let mut show_damage = true;
    let mut force_redraw = true;
    let mut next_frame = Instant::now();

    loop {
        let now = Instant::now();
        let due = now >= next_frame;
        let may_submit = !paced || ready;
        if may_submit && (force_redraw || (!paused && due)) {
            let mut snapshot = stats.snapshot(runtime.dropped());
            // Submission and completion serials are synchronously available;
            // observer totals may be published just after the completion wakeup.
            snapshot.submitted = serial;
            snapshot.completed = runtime.written_serial();
            let (pixels, damage) = lab.render(!paused, show_damage, paced, snapshot, capabilities);
            serial = serial.wrapping_add(1).max(1);
            runtime
                .submit(Frame::rgb(
                    serial,
                    WIDTH,
                    HEIGHT,
                    WIDTH as usize * 3,
                    Arc::<[u8]>::from(pixels),
                    damage,
                )?)
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "terminal runtime stopped")
                })?;
            ready = false;
            force_redraw = false;
            next_frame = now + FRAME_TIME;
        }

        let timeout = if paused && !force_redraw {
            None
        } else {
            Some(next_frame.saturating_duration_since(Instant::now()))
        };
        if runtime.wakeup().wait(timeout)? {
            runtime.wakeup().clear()?;
        }

        while let Some(event) = runtime.try_event()? {
            match event {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char(' ') => {
                        paused = !paused;
                        force_redraw = true;
                    }
                    KeyCode::Char('p') | KeyCode::Char('P') => {
                        paced = !paced;
                        ready = true;
                        force_redraw = true;
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') => {
                        show_damage = !show_damage;
                        lab.force_full = true;
                        force_redraw = true;
                    }
                    KeyCode::Char('r') | KeyCode::Char('R') => {
                        lab = Lab::new();
                        force_redraw = true;
                    }
                    KeyCode::Char('1') => {
                        lab.mode = DamageMode::Sparse;
                        lab.force_full = true;
                        force_redraw = true;
                    }
                    KeyCode::Char('2') => {
                        lab.mode = DamageMode::Bounds;
                        lab.force_full = true;
                        force_redraw = true;
                    }
                    KeyCode::Char('3') => {
                        lab.mode = DamageMode::Full;
                        lab.force_full = true;
                        force_redraw = true;
                    }
                    KeyCode::Char('4') => {
                        lab.mode = DamageMode::Many;
                        lab.force_full = true;
                        force_redraw = true;
                    }
                    _ => {}
                },
                Event::Resize(new_size) => {
                    terminal_size = new_size;
                    runtime.set_placement(demo_placement(terminal_size)?);
                    lab.force_full = true;
                    force_redraw = true;
                }
                _ => {}
            }
        }

        while runtime.take_completion().is_some() {
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

#[derive(Default)]
struct Stats {
    submitted: AtomicU64,
    completed: AtomicU64,
    regions: AtomicU64,
    pixels: AtomicU64,
    bytes: AtomicU64,
    medium: AtomicU64,
    elapsed_us: AtomicU64,
    pipeline_us: AtomicU64,
}

impl Stats {
    fn snapshot(&self, dropped: u64) -> Snapshot {
        Snapshot {
            submitted: self.submitted.load(Ordering::Relaxed),
            completed: self.completed.load(Ordering::Relaxed),
            dropped,
            regions: self.regions.load(Ordering::Relaxed),
            pixels: self.pixels.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            medium: self.medium.load(Ordering::Relaxed),
            elapsed_us: self.elapsed_us.load(Ordering::Relaxed),
            pipeline_us: self.pipeline_us.load(Ordering::Relaxed),
        }
    }
}

impl PresentationObserver for Stats {
    fn submitted(&self) {
        self.submitted.fetch_add(1, Ordering::Relaxed);
    }

    fn presented(&self, stats: PresentStats, elapsed: Duration, pipeline: Duration) {
        self.completed.fetch_add(1, Ordering::Relaxed);
        self.regions.store(stats.regions as u64, Ordering::Relaxed);
        self.pixels.store(stats.pixels, Ordering::Relaxed);
        self.bytes.store(stats.wire_bytes as u64, Ordering::Relaxed);
        self.medium.store(
            match stats.medium {
                None => 0,
                Some(TransferMedium::Direct) => 1,
                Some(TransferMedium::TemporaryFile) => 2,
                Some(TransferMedium::SharedMemory) => 3,
            },
            Ordering::Relaxed,
        );
        self.elapsed_us
            .store(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.pipeline_us
            .store(pipeline.as_micros() as u64, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Default)]
struct Snapshot {
    submitted: u64,
    completed: u64,
    dropped: u64,
    regions: u64,
    pixels: u64,
    bytes: u64,
    medium: u64,
    elapsed_us: u64,
    pipeline_us: u64,
}

#[derive(Clone, Copy)]
enum DamageMode {
    Sparse,
    Bounds,
    Full,
    Many,
}

impl DamageMode {
    fn name(self) -> &'static str {
        match self {
            Self::Sparse => "SPARSE",
            Self::Bounds => "BOUNDS",
            Self::Full => "FULL",
            Self::Many => "MANY",
        }
    }
}

struct Lab {
    background: Vec<u8>,
    pixels: Vec<u8>,
    sprites: Vec<Sprite>,
    mode: DamageMode,
    frame: u64,
    previous_visual: Vec<Rect>,
    force_full: bool,
}

impl Lab {
    fn new() -> Self {
        let background = make_background();
        let colors = [
            [234, 92, 72],
            [245, 190, 66],
            [76, 181, 181],
            [104, 131, 219],
            [205, 103, 177],
        ];
        let sprites = (0..MAX_SPRITES)
            .map(|index| Sprite {
                x: 36.0 + (index * 53 % 520) as f32,
                y: 102.0 + (index * 71 % 210) as f32,
                vx: 0.75 + (index % 4) as f32 * 0.34,
                vy: if index & 1 == 0 {
                    0.8 + (index % 3) as f32 * 0.27
                } else {
                    -0.9 - (index % 3) as f32 * 0.23
                },
                size: 10 + (index % 4) as u32 * 3,
                color: colors[index % colors.len()],
                kind: index % 3,
            })
            .collect();
        Self {
            pixels: background.clone(),
            background,
            sprites,
            mode: DamageMode::Sparse,
            frame: 0,
            previous_visual: Vec::new(),
            force_full: true,
        }
    }

    fn render(
        &mut self,
        advance: bool,
        show_damage: bool,
        paced: bool,
        stats: Snapshot,
        capabilities: tilcayo::kitty::GraphicsCapabilities,
    ) -> (Vec<u8>, Vec<Rect>) {
        let old_bounds: Vec<_> = self.sprites.iter().map(Sprite::bounds).collect();
        if advance {
            self.step();
        }
        let new_bounds: Vec<_> = self.sprites.iter().map(Sprite::bounds).collect();
        self.pixels.copy_from_slice(&self.background);

        if matches!(self.mode, DamageMode::Many) {
            draw_scattered_tiles(&mut self.pixels, self.frame);
        }
        for sprite in &self.sprites {
            sprite.draw(&mut self.pixels);
        }

        let mut visual = self.base_damage(&old_bounds, &new_bounds);
        let mut damage = visual.clone();
        if show_damage {
            for rect in &visual {
                outline_rect(&mut self.pixels, *rect, [255, 80, 120]);
            }
            damage.extend(self.previous_visual.iter().copied());
            self.previous_visual = visual.clone();
        } else {
            damage.extend(self.previous_visual.drain(..));
        }

        draw_hud(
            &mut self.pixels,
            self.mode,
            paced,
            show_damage,
            stats,
            capabilities,
        );
        // Keep diagnostics as several ordinary partial edits. A framebuffer-wide
        // strip can obscure the cost difference between sparse sprite modes.
        let hud = [
            Rect::new(6, 4, 142, 18),
            Rect::new(152, 5, 474, 12),
            Rect::new(6, 24, 430, 11),
            Rect::new(6, 37, 620, 11),
            Rect::new(6, 50, 628, 12),
        ];
        visual.extend(hud);
        damage.extend(hud);

        if self.force_full || matches!(self.mode, DamageMode::Full) {
            damage.clear();
            damage.push(Rect::full(WIDTH, HEIGHT));
            self.force_full = false;
        }
        (self.pixels.clone(), damage)
    }

    fn step(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        for sprite in &mut self.sprites {
            sprite.x += sprite.vx;
            sprite.y += sprite.vy;
            let radius = sprite.size as f32;
            if sprite.x - radius < 4.0 || sprite.x + radius >= WIDTH as f32 - 4.0 {
                sprite.vx = -sprite.vx;
                sprite.x = sprite.x.clamp(radius + 4.0, WIDTH as f32 - radius - 5.0);
            }
            if sprite.y - radius < HUD_HEIGHT as f32 + 4.0
                || sprite.y + radius >= HEIGHT as f32 - 4.0
            {
                sprite.vy = -sprite.vy;
                sprite.y = sprite.y.clamp(
                    HUD_HEIGHT as f32 + radius + 4.0,
                    HEIGHT as f32 - radius - 5.0,
                );
            }
        }
    }

    fn base_damage(&self, old_bounds: &[Rect], new_bounds: &[Rect]) -> Vec<Rect> {
        match self.mode {
            DamageMode::Sparse => old_bounds.iter().chain(new_bounds).copied().collect(),
            DamageMode::Bounds => old_bounds
                .iter()
                .chain(new_bounds)
                .copied()
                .reduce(Rect::union)
                .into_iter()
                .collect(),
            DamageMode::Full => vec![Rect::full(WIDTH, HEIGHT)],
            DamageMode::Many => {
                let mut damage: Vec<_> = old_bounds.iter().chain(new_bounds).copied().collect();
                for index in 0..72_u32 {
                    let x = 8 + index.wrapping_mul(83) % (WIDTH - 16);
                    let y = HUD_HEIGHT + 8 + index.wrapping_mul(47) % (HEIGHT - HUD_HEIGHT - 16);
                    damage.push(Rect::new(x, y, 5, 5));
                }
                damage
            }
        }
    }
}

struct Sprite {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    size: u32,
    color: [u8; 3],
    kind: usize,
}

impl Sprite {
    fn bounds(&self) -> Rect {
        let radius = self.size + 3;
        Rect::new(
            (self.x as i32 - radius as i32).max(0) as u32,
            (self.y as i32 - radius as i32).max(0) as u32,
            radius * 2 + 1,
            radius * 2 + 1,
        )
    }

    fn draw(&self, pixels: &mut [u8]) {
        let radius = self.size as i32;
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let inside = match self.kind {
                    0 => dx * dx + dy * dy <= radius * radius,
                    1 => dx.abs() + dy.abs() <= radius,
                    _ => dx.abs().max(dy.abs()) <= radius,
                };
                if inside {
                    set_pixel(pixels, self.x as i32 + dx, self.y as i32 + dy, self.color);
                }
            }
        }
    }
}

fn make_background() -> Vec<u8> {
    let mut pixels = vec![0; WIDTH as usize * HEIGHT as usize * 3];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let checker = ((x / 32 + y / 32) & 1) as u8;
            let base = if y < HUD_HEIGHT { 18 } else { 24 + checker * 3 };
            set_pixel(&mut pixels, x as i32, y as i32, [base, base + 2, base + 5]);
        }
    }
    for x in (0..WIDTH).step_by(32) {
        for y in HUD_HEIGHT..HEIGHT {
            set_pixel(&mut pixels, x as i32, y as i32, [38, 41, 46]);
        }
    }
    for y in (HUD_HEIGHT as usize..HEIGHT as usize).step_by(32) {
        for x in 0..WIDTH {
            set_pixel(&mut pixels, x as i32, y as i32, [38, 41, 46]);
        }
    }
    pixels
}

fn draw_scattered_tiles(pixels: &mut [u8], frame: u64) {
    for index in 0..72_u32 {
        let x = 8 + index.wrapping_mul(83) % (WIDTH - 16);
        let y = HUD_HEIGHT + 8 + index.wrapping_mul(47) % (HEIGHT - HUD_HEIGHT - 16);
        let bright = ((frame + u64::from(index)) & 1) == 0;
        fill_rect(
            pixels,
            Rect::new(x, y, 5, 5),
            if bright { [74, 92, 102] } else { [31, 36, 41] },
        );
    }
}

fn draw_hud(
    pixels: &mut [u8],
    mode: DamageMode,
    paced: bool,
    show_damage: bool,
    stats: Snapshot,
    caps: tilcayo::kitty::GraphicsCapabilities,
) {
    fill_rect(pixels, Rect::new(0, 0, WIDTH, HUD_HEIGHT), [17, 19, 22]);
    draw_text(pixels, 8, 6, 2, "DAMAGE LAB", [242, 235, 211]);
    draw_text(
        pixels,
        156,
        8,
        1,
        &format!(
            "MODE {}  PACE {}  OUTLINE {}",
            mode.name(),
            if paced { "COMPLETION" } else { "UNRESTRICTED" },
            if show_damage { "ON" } else { "OFF" }
        ),
        [151, 178, 190],
    );
    draw_text(
        pixels,
        8,
        27,
        1,
        &format!(
            "SUB {}  DONE {}  DROP {}  REGIONS {}",
            stats.submitted, stats.completed, stats.dropped, stats.regions
        ),
        [224, 226, 222],
    );
    draw_text(
        pixels,
        8,
        40,
        1,
        &format!(
            "PIXELS {}  BYTES {}  WRITE {}US  PIPE {}US  MED {}",
            stats.pixels,
            stats.bytes,
            stats.elapsed_us,
            stats.pipeline_us,
            medium_name(stats.medium)
        ),
        [224, 226, 222],
    );
    draw_text(
        pixels,
        8,
        53,
        1,
        &format!(
            "CAP SHM {} FILE {} ANIM {} TRANSIENT {}   1-4 MODE  P PACE  D OUTLINE  SPACE PAUSE  R RESET  Q QUIT",
            yes_no(caps.shared_memory),
            yes_no(caps.temporary_file),
            yes_no(caps.animation),
            yes_no(caps.transient)
        ),
        [174, 178, 180],
    );
}

fn medium_name(value: u64) -> &'static str {
    match value {
        1 => "DIRECT",
        2 => "FILE",
        3 => "SHM",
        _ => "NONE",
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "Y"
    } else {
        "N"
    }
}

fn fill_rect(pixels: &mut [u8], rect: Rect, color: [u8; 3]) {
    if let Some(rect) = rect.clip(WIDTH, HEIGHT) {
        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                set_pixel(pixels, x as i32, y as i32, color);
            }
        }
    }
}

fn outline_rect(pixels: &mut [u8], rect: Rect, color: [u8; 3]) {
    if let Some(rect) = rect.clip(WIDTH, HEIGHT) {
        let x2 = rect.x + rect.width - 1;
        let y2 = rect.y + rect.height - 1;
        for x in rect.x..=x2 {
            set_pixel(pixels, x as i32, rect.y as i32, color);
            set_pixel(pixels, x as i32, y2 as i32, color);
        }
        for y in rect.y..=y2 {
            set_pixel(pixels, rect.x as i32, y as i32, color);
            set_pixel(pixels, x2 as i32, y as i32, color);
        }
    }
}

fn set_pixel(pixels: &mut [u8], x: i32, y: i32, color: [u8; 3]) {
    if x < 0 || y < 0 || x >= WIDTH as i32 || y >= HEIGHT as i32 {
        return;
    }
    let offset = (y as usize * WIDTH as usize + x as usize) * 3;
    pixels[offset..offset + 3].copy_from_slice(&color);
}

fn draw_text(pixels: &mut [u8], x: i32, y: i32, scale: i32, text: &str, color: [u8; 3]) {
    let mut cursor = x;
    for character in text.chars() {
        let rows = glyph(character);
        for (gy, row) in rows.into_iter().enumerate() {
            for gx in 0..3 {
                if row & (1 << (2 - gx)) != 0 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            set_pixel(
                                pixels,
                                cursor + gx * scale + sx,
                                y + gy as i32 * scale + sy,
                                color,
                            );
                        }
                    }
                }
            }
        }
        cursor += 4 * scale;
    }
}

fn glyph(character: char) -> [u8; 5] {
    match character.to_ascii_uppercase() {
        'A' => [2, 5, 7, 5, 5],
        'B' => [6, 5, 6, 5, 6],
        'C' => [3, 4, 4, 4, 3],
        'D' => [6, 5, 5, 5, 6],
        'E' => [7, 4, 6, 4, 7],
        'F' => [7, 4, 6, 4, 4],
        'G' => [3, 4, 5, 5, 3],
        'H' => [5, 5, 7, 5, 5],
        'I' => [7, 2, 2, 2, 7],
        'J' => [1, 1, 1, 5, 2],
        'K' => [5, 5, 6, 5, 5],
        'L' => [4, 4, 4, 4, 7],
        'M' => [5, 7, 7, 5, 5],
        'N' => [5, 7, 7, 7, 5],
        'O' => [2, 5, 5, 5, 2],
        'P' => [6, 5, 6, 4, 4],
        'Q' => [2, 5, 5, 7, 3],
        'R' => [6, 5, 6, 5, 5],
        'S' => [3, 4, 2, 1, 6],
        'T' => [7, 2, 2, 2, 2],
        'U' => [5, 5, 5, 5, 7],
        'V' => [5, 5, 5, 5, 2],
        'W' => [5, 5, 7, 7, 5],
        'X' => [5, 5, 2, 5, 5],
        'Y' => [5, 5, 2, 2, 2],
        'Z' => [7, 1, 2, 4, 7],
        '0' => [7, 5, 5, 5, 7],
        '1' => [2, 6, 2, 2, 7],
        '2' => [6, 1, 7, 4, 7],
        '3' => [6, 1, 3, 1, 6],
        '4' => [5, 5, 7, 1, 1],
        '5' => [7, 4, 6, 1, 6],
        '6' => [3, 4, 7, 5, 7],
        '7' => [7, 1, 2, 2, 2],
        '8' => [7, 5, 7, 5, 7],
        '9' => [7, 5, 7, 1, 6],
        ':' => [0, 2, 0, 2, 0],
        '-' => [0, 0, 7, 0, 0],
        '/' => [1, 1, 2, 4, 4],
        '.' => [0, 0, 0, 0, 2],
        _ => [0; 5],
    }
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
    fn every_mode_renders_a_valid_frame() {
        let stats = Snapshot::default();
        let caps = tilcayo::kitty::GraphicsCapabilities::default();
        for mode in [
            DamageMode::Sparse,
            DamageMode::Bounds,
            DamageMode::Full,
            DamageMode::Many,
        ] {
            let mut lab = Lab::new();
            lab.mode = mode;
            let (pixels, damage) = lab.render(true, true, true, stats, caps);
            assert_eq!(pixels.len(), WIDTH as usize * HEIGHT as usize * 3);
            assert!(!damage.is_empty());
            assert!(damage.iter().all(|rect| rect.clip(WIDTH, HEIGHT).is_some()));
        }
    }

    #[test]
    fn sparse_mode_reports_sprite_old_and_new_bounds_plus_hud() {
        let mut lab = Lab::new();
        lab.force_full = false;
        let (_, damage) = lab.render(
            true,
            false,
            true,
            Snapshot::default(),
            tilcayo::kitty::GraphicsCapabilities::default(),
        );
        assert_eq!(damage.len(), MAX_SPRITES * 2 + 5);
    }
}
