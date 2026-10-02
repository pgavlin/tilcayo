use std::{
    env, fs,
    io::{self, Read, Write},
    os::fd::AsRawFd,
    path::PathBuf,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use tilcayo::{
    kitty::{
        probe, GraphicsCapabilities, GraphicsTransport, KittyPresenter, Placement, TransferMedium,
        TransferOptions, ZlibPolicy,
    },
    Frame, Rect, TerminalSize,
};

const RESPONSE_TIMEOUT_MS: i32 = 5_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Suite {
    Focused,
    Matrix,
}

impl Suite {
    fn name(self) -> &'static str {
        match self {
            Self::Focused => "focused",
            Self::Matrix => "matrix",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum UpdateMode {
    RootEdit,
    FullReplace,
}

impl UpdateMode {
    fn name(self) -> &'static str {
        match self {
            Self::RootEdit => "root-edit",
            Self::FullReplace => "full-replace",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct BenchmarkConfig {
    transport: GraphicsTransport,
    transport_name: &'static str,
    zlib: ZlibPolicy,
    zlib_name: &'static str,
    chunk_size: usize,
}

impl BenchmarkConfig {
    fn chunk_name(self) -> String {
        match self.transport {
            GraphicsTransport::Direct => self.chunk_size.to_string(),
            _ => "none".to_owned(),
        }
    }

    fn transfer_options(self) -> TransferOptions {
        TransferOptions {
            transport: self.transport,
            zlib: self.zlib,
            chunk_size: self.chunk_size,
        }
    }
}

#[derive(Debug)]
struct Args {
    suite: Suite,
    warmups: usize,
    samples: usize,
    output: PathBuf,
}

#[derive(Clone, Debug)]
struct DamageCase {
    name: &'static str,
    rects: Vec<Rect>,
}

impl DamageCase {
    fn pixels(&self) -> u64 {
        self.rects.iter().map(|rect| rect.area()).sum()
    }

    fn bounds(&self) -> Rect {
        self.rects
            .iter()
            .copied()
            .reduce(Rect::union)
            .expect("benchmark damage is nonempty")
    }
}

#[derive(Debug)]
struct ResultGroup {
    suite: Suite,
    width: u32,
    height: u32,
    config: BenchmarkConfig,
    mode: UpdateMode,
    case: DamageCase,
    samples: Vec<Duration>,
    presented_pixels: u64,
    wire_bytes: usize,
    medium: Option<TransferMedium>,
}

struct BenchmarkTerminal;

impl BenchmarkTerminal {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let result = execute!(io::stdout(), EnterAlternateScreen, Hide);
        if result.is_err() {
            let _ = disable_raw_mode();
        }
        result.map(|_| Self)
    }
}

impl Drop for BenchmarkTerminal {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args()?;
    if env::var_os("KITTY_WINDOW_ID").is_none() {
        return Err("the benchmark must run directly inside Kitty".into());
    }
    if unsafe { libc::isatty(io::stdin().as_raw_fd()) } != 1
        || unsafe { libc::isatty(io::stdout().as_raw_fd()) } != 1
    {
        return Err("the benchmark requires terminal stdin and stdout".into());
    }

    let terminal_size = TerminalSize::current()?;
    let native = (
        u32::from(
            terminal_size
                .width_px
                .ok_or("the terminal did not report its pixel width")?,
        ),
        u32::from(
            terminal_size
                .height_px
                .ok_or("the terminal did not report its pixel height")?,
        ),
    );
    let placement = Placement::new(0, 0, terminal_size.columns, terminal_size.rows)?;
    let terminal = BenchmarkTerminal::enter()?;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let capabilities = probe(
        &mut input,
        &mut output,
        Duration::from_millis(RESPONSE_TIMEOUT_MS as u64),
    )?;
    validate_capabilities(args.suite, capabilities)?;

    let image_id = (std::process::id() | 0x4000_0000).max(1);
    let barrier_id = image_id.wrapping_add(1).max(1);
    let results = run_benchmarks(
        &args,
        capabilities,
        native,
        placement,
        image_id,
        barrier_id,
        &mut input,
        &mut output,
    )?;

    drop(output);
    drop(input);
    drop(terminal);

    write_results(&args, native, &results)?;
    print_summary(&args, &results);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_benchmarks(
    args: &Args,
    capabilities: GraphicsCapabilities,
    native: (u32, u32),
    placement: Placement,
    image_id: u32,
    barrier_id: u32,
    input: &mut (impl Read + AsRawFd),
    output: &mut impl Write,
) -> io::Result<Vec<ResultGroup>> {
    let mut results = Vec::new();
    for (width, height) in benchmark_sizes(args.suite, native) {
        let pixels = Arc::<[u8]>::from(benchmark_frame(width, height));
        for config in benchmark_configs(args.suite, capabilities) {
            let modes: &[UpdateMode] = match args.suite {
                Suite::Focused => &[UpdateMode::RootEdit, UpdateMode::FullReplace],
                Suite::Matrix => &[UpdateMode::RootEdit],
            };
            for &mode in modes {
                for case in damage_cases(width, height) {
                    let mut presenter = KittyPresenter::new(image_id, true);
                    presenter.set_transfer_options(config.transfer_options());

                    if matches!(mode, UpdateMode::RootEdit) {
                        let initial = Frame::rgb(
                            0,
                            width,
                            height,
                            width as usize * 3,
                            pixels.clone(),
                            Vec::new(),
                        )?;
                        presenter.present(output, &initial, placement)?;
                        terminal_barrier(output, input, barrier_id)?;
                    }

                    let frame = Frame::rgb(
                        1,
                        width,
                        height,
                        width as usize * 3,
                        pixels.clone(),
                        case.rects.clone(),
                    )?;
                    for _ in 0..args.warmups {
                        if matches!(mode, UpdateMode::FullReplace) {
                            presenter.invalidate();
                        }
                        presenter.present(output, &frame, placement)?;
                        terminal_barrier(output, input, barrier_id)?;
                    }

                    let mut samples = Vec::with_capacity(args.samples);
                    let mut presented_pixels = 0;
                    let mut wire_bytes = 0;
                    let mut medium = None;
                    for _ in 0..args.samples {
                        if matches!(mode, UpdateMode::FullReplace) {
                            presenter.invalidate();
                        }
                        let started = Instant::now();
                        let stats = presenter.present(output, &frame, placement)?;
                        terminal_barrier(output, input, barrier_id)?;
                        samples.push(started.elapsed());
                        presented_pixels = stats.pixels;
                        wire_bytes = stats.wire_bytes;
                        medium = stats.medium;
                    }

                    presenter.delete(output)?;
                    terminal_barrier(output, input, barrier_id)?;
                    results.push(ResultGroup {
                        suite: args.suite,
                        width,
                        height,
                        config,
                        mode,
                        case,
                        samples,
                        presented_pixels,
                        wire_bytes,
                        medium,
                    });
                }
            }
        }
    }
    Ok(results)
}

fn validate_capabilities(suite: Suite, capabilities: GraphicsCapabilities) -> io::Result<()> {
    if !capabilities.graphics {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Kitty graphics support was not detected",
        ));
    }
    if !capabilities.animation {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Kitty animation-frame support was not detected",
        ));
    }
    if suite == Suite::Focused && !capabilities.shared_memory {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the focused suite requires verified POSIX shared-memory transfer",
        ));
    }
    Ok(())
}

fn benchmark_sizes(suite: Suite, native: (u32, u32)) -> Vec<(u32, u32)> {
    let mut sizes = match suite {
        Suite::Focused => vec![(1280, 720), (1920, 1080), native],
        Suite::Matrix => vec![(320, 200), (640, 400), (1280, 720), (1920, 1080), native],
    };
    sizes.sort_by_key(|&(width, height)| (u64::from(width) * u64::from(height), width));
    sizes.dedup();
    sizes
}

fn benchmark_configs(suite: Suite, capabilities: GraphicsCapabilities) -> Vec<BenchmarkConfig> {
    if suite == Suite::Focused {
        return vec![BenchmarkConfig {
            transport: GraphicsTransport::SharedMemory,
            transport_name: "shared-memory",
            zlib: ZlibPolicy::Never,
            zlib_name: "never",
            chunk_size: 4096,
        }];
    }

    let mut configs = Vec::new();
    for (zlib, zlib_name) in [
        (ZlibPolicy::Never, "never"),
        (ZlibPolicy::Adaptive, "adaptive"),
        (ZlibPolicy::Always, "always"),
    ] {
        for chunk_size in [1024, 4096] {
            configs.push(BenchmarkConfig {
                transport: GraphicsTransport::Direct,
                transport_name: "direct",
                zlib,
                zlib_name,
                chunk_size,
            });
        }
        if capabilities.temporary_file {
            configs.push(BenchmarkConfig {
                transport: GraphicsTransport::TemporaryFile,
                transport_name: "temporary-file",
                zlib,
                zlib_name,
                chunk_size: 4096,
            });
        }
        if capabilities.shared_memory {
            configs.push(BenchmarkConfig {
                transport: GraphicsTransport::SharedMemory,
                transport_name: "shared-memory",
                zlib,
                zlib_name,
                chunk_size: 4096,
            });
        }
    }
    configs
}

fn damage_cases(width: u32, height: u32) -> Vec<DamageCase> {
    let cursor_width = width.min(16);
    let cursor_height = height.min(16);
    let line_width = width.min(512);
    let line_height = height.min(32);
    let quarter_width = (width / 2).max(1);
    let quarter_height = (height / 2).max(1);
    let tile_width = width.min(32);
    let tile_height = height.min(32);
    vec![
        DamageCase {
            name: "cursor",
            rects: vec![centered(width, height, cursor_width, cursor_height)],
        },
        DamageCase {
            name: "text-line",
            rects: vec![centered(width, height, line_width, line_height)],
        },
        DamageCase {
            name: "four-corners",
            rects: vec![
                Rect::new(0, 0, tile_width, tile_height),
                Rect::new(width - tile_width, 0, tile_width, tile_height),
                Rect::new(0, height - tile_height, tile_width, tile_height),
                Rect::new(
                    width - tile_width,
                    height - tile_height,
                    tile_width,
                    tile_height,
                ),
            ],
        },
        DamageCase {
            name: "quarter",
            rects: vec![centered(width, height, quarter_width, quarter_height)],
        },
        DamageCase {
            name: "full",
            rects: vec![Rect::full(width, height)],
        },
    ]
}

fn centered(width: u32, height: u32, rect_width: u32, rect_height: u32) -> Rect {
    Rect::new(
        (width - rect_width) / 2,
        (height - rect_height) / 2,
        rect_width,
        rect_height,
    )
}

fn benchmark_frame(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
    for y in 0..height {
        for x in 0..width {
            let noise = x
                .wrapping_mul(0x9e37_79b9)
                .rotate_left(y & 31)
                .wrapping_add(y.wrapping_mul(0x85eb_ca6b));
            pixels.extend_from_slice(&[(noise >> 16) as u8, (noise >> 8) as u8, noise as u8]);
        }
    }
    pixels
}

fn terminal_barrier(
    output: &mut impl Write,
    input: &mut (impl Read + AsRawFd),
    image_id: u32,
) -> io::Result<()> {
    write!(
        output,
        "\x1b_Gi={image_id},s=1,v=1,a=q,t=d,f=24,q=0;AAAA\x1b\\"
    )?;
    output.flush()?;
    wait_for_response(input, image_id)
}

fn wait_for_response(input: &mut (impl Read + AsRawFd), image_id: u32) -> io::Result<()> {
    let mut response = Vec::new();
    let mut byte = [0_u8; 1];
    let mut in_response = false;
    let mut previous_escape = false;
    loop {
        let mut pollfd = libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pollfd, 1, RESPONSE_TIMEOUT_MS) };
        if ready == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "timed out waiting for Kitty graphics acknowledgement; partial input={response:?}"
                ),
            ));
        }
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        let count = unsafe { libc::read(input.as_raw_fd(), byte.as_mut_ptr().cast(), 1) };
        if count != 1 {
            return Err(if count < 0 {
                io::Error::last_os_error()
            } else {
                io::Error::new(io::ErrorKind::UnexpectedEof, "terminal input closed")
            });
        }
        if !in_response {
            response.push(byte[0]);
            if response.ends_with(b"\x1b_G") {
                response.clear();
                response.extend_from_slice(b"\x1b_G");
                in_response = true;
            } else if response.len() > 3 {
                response.remove(0);
            }
            continue;
        }
        response.push(byte[0]);
        if previous_escape && byte[0] == b'\\' {
            let text = String::from_utf8_lossy(&response);
            if !text.contains(&format!("i={image_id}")) {
                response.clear();
                in_response = false;
                previous_escape = false;
                continue;
            }
            if !text.contains(";OK") {
                return Err(io::Error::other(format!(
                    "Kitty graphics barrier failed: {text:?}"
                )));
            }
            return Ok(());
        }
        previous_escape = byte[0] == 0x1b;
    }
}

fn write_results(args: &Args, native: (u32, u32), results: &[ResultGroup]) -> io::Result<()> {
    if let Some(parent) = args
        .output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut output = fs::File::create(&args.output)?;
    writeln!(output, "# Tilcayo Kitty presentation benchmark")?;
    writeln!(output, "# crate_version={}", env!("CARGO_PKG_VERSION"))?;
    writeln!(
        output,
        "# git_revision={}",
        command_output("git", &["rev-parse", "HEAD"])
    )?;
    writeln!(
        output,
        "# git_dirty={}",
        !command_output("git", &["status", "--porcelain"]).is_empty()
    )?;
    writeln!(
        output,
        "# rustc={}",
        command_output("rustc", &["--version"])
    )?;
    writeln!(
        output,
        "# kitty={}",
        command_output("kitty", &["--version"])
    )?;
    writeln!(output, "# uname={}", command_output("uname", &["-a"]))?;
    writeln!(output, "# native_terminal_pixels={}x{}", native.0, native.1)?;
    writeln!(output, "# suite={}", args.suite.name())?;
    writeln!(output, "# warmups={}", args.warmups)?;
    writeln!(output, "# samples={}", args.samples)?;
    writeln!(
        output,
        "row,suite,screen_width,screen_height,transport,zlib,chunk_size,mode,case,damage_rects,damage_pixels,bounds_width,bounds_height,presented_pixels,wire_bytes,actual_medium,sample_index,elapsed_ms,p50_ms,p90_ms,p99_ms"
    )?;

    for result in results {
        let bounds = result.case.bounds();
        let chunk = result.config.chunk_name();
        let medium = medium_name(result.medium);
        for (index, elapsed) in result.samples.iter().enumerate() {
            writeln!(
                output,
                "sample,{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{:.6},,,",
                result.suite.name(),
                result.width,
                result.height,
                result.config.transport_name,
                result.config.zlib_name,
                chunk,
                result.mode.name(),
                result.case.name,
                result.case.rects.len(),
                result.case.pixels(),
                bounds.width,
                bounds.height,
                result.presented_pixels,
                result.wire_bytes,
                medium,
                index,
                elapsed.as_secs_f64() * 1000.0,
            )?;
        }
        let (p50, p90, p99) = percentiles(&result.samples);
        writeln!(
            output,
            "summary,{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},,,{:.6},{:.6},{:.6}",
            result.suite.name(),
            result.width,
            result.height,
            result.config.transport_name,
            result.config.zlib_name,
            chunk,
            result.mode.name(),
            result.case.name,
            result.case.rects.len(),
            result.case.pixels(),
            bounds.width,
            bounds.height,
            result.presented_pixels,
            result.wire_bytes,
            medium,
            p50.as_secs_f64() * 1000.0,
            p90.as_secs_f64() * 1000.0,
            p99.as_secs_f64() * 1000.0,
        )?;
    }
    output.flush()
}

fn print_summary(args: &Args, results: &[ResultGroup]) {
    eprintln!("benchmark CSV: {}", args.output.display());
    eprintln!("p50 / p90 / p99 acknowledgement latency (ms)");
    for result in results {
        let (p50, p90, p99) = percentiles(&result.samples);
        eprintln!(
            "{:>4}x{:<4} {:<13} {:<15} {:<12} {:>9.3} / {:>9.3} / {:>9.3}",
            result.width,
            result.height,
            result.mode.name(),
            result.config.transport_name,
            result.case.name,
            p50.as_secs_f64() * 1000.0,
            p90.as_secs_f64() * 1000.0,
            p99.as_secs_f64() * 1000.0,
        );
    }
}

fn percentiles(samples: &[Duration]) -> (Duration, Duration, Duration) {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    (
        percentile(&sorted, 50),
        percentile(&sorted, 90),
        percentile(&sorted, 99),
    )
}

fn percentile(sorted: &[Duration], percentile: usize) -> Duration {
    let rank = (sorted.len() * percentile).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn medium_name(medium: Option<TransferMedium>) -> &'static str {
    match medium {
        Some(TransferMedium::Direct) => "direct",
        Some(TransferMedium::TemporaryFile) => "temporary-file",
        Some(TransferMedium::SharedMemory) => "shared-memory",
        None => "none",
    }
}

fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .replace('\n', " ")
        })
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn parse_args() -> Result<Args, Box<dyn std::error::Error>> {
    let mut suite = Suite::Focused;
    let mut warmups = 5;
    let mut samples = 30;
    let mut output = None;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--suite" => {
                suite = parse_suite(&arguments.next().ok_or("--suite requires a value")?)?;
            }
            "--warmups" => {
                warmups = arguments
                    .next()
                    .ok_or("--warmups requires a value")?
                    .parse()?;
            }
            "--samples" => {
                samples = arguments
                    .next()
                    .ok_or("--samples requires a value")?
                    .parse()?;
            }
            "--output" => {
                output = Some(PathBuf::from(
                    arguments.next().ok_or("--output requires a value")?,
                ));
            }
            "-h" | "--help" => {
                println!(
                    "usage: cargo run --release --example benchmark -- [--suite focused|matrix] [--warmups N] [--samples N] [--output PATH]"
                );
                std::process::exit(0);
            }
            _ => return Err(format!("unknown benchmark argument: {argument}").into()),
        }
    }
    if samples == 0 {
        return Err("--samples must be greater than zero".into());
    }
    Ok(Args {
        suite,
        warmups,
        samples,
        output: output.unwrap_or_else(|| {
            PathBuf::from(format!(
                "/tmp/tilcayo-kitty-benchmark-{}.csv",
                std::process::id()
            ))
        }),
    })
}

fn parse_suite(value: &str) -> Result<Suite, Box<dyn std::error::Error>> {
    match value {
        "focused" => Ok(Suite::Focused),
        "matrix" => Ok(Suite::Matrix),
        _ => Err(format!("unknown benchmark suite: {value}").into()),
    }
}
