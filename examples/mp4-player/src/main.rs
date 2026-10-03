use std::{
    collections::VecDeque,
    env, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use ffmpeg::{
    codec, format, frame,
    media::Type,
    software::{resampling, scaling},
    ChannelLayout,
};
use ffmpeg_next as ffmpeg;
use rodio::{OutputStream, Sink, Source};
use tilcayo::{
    Event, Frame, KeyCode, KeyEventKind, Placement, Rect, Runtime, RuntimeConfig, TerminalSize,
};

const VIDEO_QUEUE_CAPACITY: usize = 8;
const AUDIO_CHUNK_CAPACITY: usize = 32;
const AUDIO_RATE: u32 = 48_000;
const AUDIO_CHANNELS: u16 = 2;
const AUDIO_PREBUFFER_SAMPLES: usize = AUDIO_RATE as usize * AUDIO_CHANNELS as usize / 4;
const DECODER_POLL: Duration = Duration::from_millis(10);
const DUE_SLOP: Duration = Duration::from_millis(2);

fn main() -> io::Result<()> {
    let options = Options::parse()?;
    if options.help {
        print_usage();
        return Ok(());
    }
    let input = options.input.as_ref().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "missing MP4 path (try --help)")
    })?;

    ffmpeg::init().map_err(media_error)?;
    ffmpeg::log::set_level(ffmpeg::log::Level::Error);
    let media = probe_video(input, options.max_width, options.max_height)?;
    let mut video = VideoDecoder::spawn(input.clone(), media)?;
    let first = video.first_frame()?;
    let mut audio = if options.no_audio || !has_audio(input)? {
        None
    } else {
        Some(AudioPlayback::start(input.clone())?)
    };

    let mut runtime = Runtime::enter(RuntimeConfig::default())?;
    let result = run(&mut runtime, &mut video, first, audio.as_mut());
    let shutdown = runtime.shutdown();
    result.and(shutdown)
}

fn run(
    runtime: &mut Runtime,
    video: &mut VideoDecoder,
    first: DecodedFrame,
    mut audio: Option<&mut AudioPlayback>,
) -> io::Result<()> {
    let media = video.media;
    let mut fallback_clock = PlaybackClock::start();
    let mut terminal_size = runtime.capabilities().size;
    runtime.set_placement(video_placement(terminal_size, media.width, media.height)?);

    let mut serial = 1_u64;
    submit_video(runtime, serial, media, first.pixels)?;
    let mut ready = false;
    let mut paused = false;
    let mut video_finished = false;

    loop {
        let position = playback_position(audio.as_deref(), &fallback_clock);
        if ready && !paused {
            match video.take_due(position + DUE_SLOP)? {
                DueFrame::Frame(frame) => {
                    serial = serial.wrapping_add(1).max(1);
                    submit_video(runtime, serial, media, frame.pixels)?;
                    ready = false;
                }
                DueFrame::Waiting => {}
                DueFrame::Finished => video_finished = true,
            }
        }

        let audio_finished = match audio.as_deref() {
            Some(audio) => audio.finished()?,
            None => true,
        };
        if video_finished && audio_finished && ready {
            return Ok(());
        }

        let timeout = if paused || !ready {
            None
        } else {
            video
                .next_timestamp()?
                .map(|pts| pts.saturating_sub(playback_position(audio.as_deref(), &fallback_clock)))
                .or(Some(DECODER_POLL))
        };
        if runtime.wakeup().wait(timeout)? {
            runtime.wakeup().clear()?;
        }

        while let Some(event) = runtime.try_event()? {
            match event {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char(' ') => {
                        if paused {
                            if let Some(audio) = audio.as_deref_mut() {
                                audio.resume();
                            }
                            fallback_clock.resume();
                        } else {
                            if let Some(audio) = audio.as_deref_mut() {
                                audio.pause();
                            }
                            fallback_clock.pause();
                        }
                        paused = !paused;
                    }
                    _ => {}
                },
                Event::Resize(size) => {
                    terminal_size = size;
                    runtime.set_placement(video_placement(
                        terminal_size,
                        media.width,
                        media.height,
                    )?);
                    if ready {
                        serial = serial.wrapping_add(1).max(1);
                        runtime
                            .submit(video.last_frame(serial)?)
                            .map_err(runtime_stopped)?;
                        ready = false;
                    }
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

fn playback_position(audio: Option<&AudioPlayback>, fallback: &PlaybackClock) -> Duration {
    audio.map_or_else(|| fallback.position(), AudioPlayback::position)
}

fn submit_video(
    runtime: &Runtime,
    serial: u64,
    media: VideoInfo,
    pixels: Arc<[u8]>,
) -> io::Result<()> {
    let frame = Frame::rgb(
        serial,
        media.width,
        media.height,
        media.width as usize * 3,
        pixels,
        vec![Rect::full(media.width, media.height)],
    )?;
    runtime.submit(frame).map_err(runtime_stopped)
}

fn runtime_stopped<T>(_: T) -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "terminal runtime stopped")
}

fn media_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

#[derive(Debug)]
struct Options {
    input: Option<PathBuf>,
    max_width: u32,
    max_height: u32,
    no_audio: bool,
    help: bool,
}

impl Options {
    fn parse() -> io::Result<Self> {
        let mut options = Self {
            input: None,
            max_width: 1280,
            max_height: 720,
            no_audio: false,
            help: false,
        };
        let mut args = env::args_os().skip(1);
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("-h" | "--help") => options.help = true,
                Some("--no-audio") => options.no_audio = true,
                Some("--max-width") => {
                    options.max_width = parse_dimension(args.next(), "--max-width")?;
                }
                Some("--max-height") => {
                    options.max_height = parse_dimension(args.next(), "--max-height")?;
                }
                Some(value) if value.starts_with('-') => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unknown option: {value}"),
                    ));
                }
                _ if options.input.is_none() => options.input = Some(arg.into()),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "only one input path is supported",
                    ));
                }
            }
        }
        Ok(options)
    }
}

fn parse_dimension(value: Option<std::ffi::OsString>, option: &str) -> io::Result<u32> {
    value
        .and_then(|value| value.to_str().and_then(|value| value.parse().ok()))
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{option} requires a positive integer"),
            )
        })
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run --release --manifest-path examples/mp4-player/Cargo.toml -- [OPTIONS] FILE\n\
         \n\
         Options:\n\
           --no-audio          Disable audio output\n\
           --max-width PIXELS  Maximum decoded width (default: 1280)\n\
           --max-height PIXELS Maximum decoded height (default: 720)\n\
           -h, --help          Show this help\n\
         \n\
         Controls: Space pauses/resumes; q or Escape exits."
    );
}

#[derive(Clone, Copy, Debug)]
struct VideoInfo {
    width: u32,
    height: u32,
}

fn probe_video(path: &Path, max_width: u32, max_height: u32) -> io::Result<VideoInfo> {
    let input = format::input(path).map_err(media_error)?;
    let stream = input
        .streams()
        .best(Type::Video)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no video stream"))?;
    let context =
        codec::context::Context::from_parameters(stream.parameters()).map_err(media_error)?;
    let decoder = context.decoder().video().map_err(media_error)?;
    let (width, height) = fit_dimensions(decoder.width(), decoder.height(), max_width, max_height);
    Ok(VideoInfo { width, height })
}

fn has_audio(path: &Path) -> io::Result<bool> {
    let input = format::input(path).map_err(media_error)?;
    Ok(input.streams().best(Type::Audio).is_some())
}

fn fit_dimensions(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    let scale = (max_width as f64 / width as f64)
        .min(max_height as f64 / height as f64)
        .min(1.0);
    (
        (width as f64 * scale).round().max(1.0) as u32,
        (height as f64 * scale).round().max(1.0) as u32,
    )
}

#[derive(Debug)]
struct DecodedFrame {
    pts: Duration,
    pixels: Arc<[u8]>,
}

struct VideoState {
    frames: VecDeque<DecodedFrame>,
    finished: bool,
    cancelled: bool,
    error: Option<String>,
    last_pixels: Option<Arc<[u8]>>,
}

struct VideoShared {
    state: Mutex<VideoState>,
    changed: Condvar,
}

struct VideoDecoder {
    media: VideoInfo,
    shared: Arc<VideoShared>,
    worker: Option<thread::JoinHandle<()>>,
}

impl VideoDecoder {
    fn spawn(path: PathBuf, media: VideoInfo) -> io::Result<Self> {
        let shared = Arc::new(VideoShared {
            state: Mutex::new(VideoState {
                frames: VecDeque::with_capacity(VIDEO_QUEUE_CAPACITY),
                finished: false,
                cancelled: false,
                error: None,
                last_pixels: None,
            }),
            changed: Condvar::new(),
        });
        let thread_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("tilcayo-video-decoder".into())
            .spawn(move || decode_video(&path, media, thread_shared))?;
        Ok(Self {
            media,
            shared,
            worker: Some(worker),
        })
    }

    fn first_frame(&self) -> io::Result<DecodedFrame> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        loop {
            if let Some(frame) = state.frames.pop_front() {
                state.last_pixels = Some(Arc::clone(&frame.pixels));
                self.shared.changed.notify_all();
                return Ok(frame);
            }
            check_video_state(&state)?;
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    fn take_due(&self, position: Duration) -> io::Result<DueFrame> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        check_video_state(&state)?;
        let mut selected = None;
        while state
            .frames
            .front()
            .is_some_and(|frame| frame.pts <= position)
        {
            selected = state.frames.pop_front();
        }
        if let Some(frame) = selected {
            state.last_pixels = Some(Arc::clone(&frame.pixels));
            self.shared.changed.notify_all();
            Ok(DueFrame::Frame(frame))
        } else if state.finished && state.frames.is_empty() {
            Ok(DueFrame::Finished)
        } else {
            Ok(DueFrame::Waiting)
        }
    }

    fn next_timestamp(&self) -> io::Result<Option<Duration>> {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        check_video_state(&state)?;
        Ok(state.frames.front().map(|frame| frame.pts))
    }

    fn last_frame(&self, serial: u64) -> io::Result<Frame> {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let pixels = state.last_pixels.clone().ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "video has no decoded frame")
        })?;
        Frame::rgb(
            serial,
            self.media.width,
            self.media.height,
            self.media.width as usize * 3,
            pixels,
            Vec::new(),
        )
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.cancelled = true;
        self.shared.changed.notify_all();
        drop(state);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

enum DueFrame {
    Frame(DecodedFrame),
    Waiting,
    Finished,
}

fn check_video_state(state: &VideoState) -> io::Result<()> {
    if let Some(error) = &state.error {
        Err(io::Error::other(error.clone()))
    } else if state.finished && state.frames.is_empty() && state.last_pixels.is_none() {
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "video has no frames",
        ))
    } else {
        Ok(())
    }
}

fn decode_video(path: &Path, media: VideoInfo, shared: Arc<VideoShared>) {
    let result = decode_video_inner(path, media, &shared);
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Err(error) = result {
        if !state.cancelled {
            state.error = Some(error.to_string());
        }
    }
    state.finished = true;
    shared.changed.notify_all();
}

fn decode_video_inner(path: &Path, media: VideoInfo, shared: &VideoShared) -> io::Result<()> {
    let mut input = format::input(path).map_err(media_error)?;
    let stream = input
        .streams()
        .best(Type::Video)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no video stream"))?;
    let stream_index = stream.index();
    let time_base = stream.time_base();
    let context =
        codec::context::Context::from_parameters(stream.parameters()).map_err(media_error)?;
    let mut decoder = context.decoder().video().map_err(media_error)?;
    let mut scaler = scaling::Context::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ffmpeg::format::Pixel::RGB24,
        media.width,
        media.height,
        scaling::Flags::BILINEAR,
    )
    .map_err(media_error)?;
    let mut origin = None;
    let mut frame_index = 0_u64;

    for (packet_stream, packet) in input.packets() {
        if cancelled(shared) {
            return Ok(());
        }
        if packet_stream.index() == stream_index {
            decoder.send_packet(&packet).map_err(media_error)?;
            receive_video_frames(
                &mut decoder,
                &mut scaler,
                time_base,
                &mut origin,
                &mut frame_index,
                shared,
            )?;
        }
    }
    decoder.send_eof().map_err(media_error)?;
    receive_video_frames(
        &mut decoder,
        &mut scaler,
        time_base,
        &mut origin,
        &mut frame_index,
        shared,
    )
}

fn receive_video_frames(
    decoder: &mut ffmpeg::decoder::Video,
    scaler: &mut scaling::Context,
    time_base: ffmpeg::Rational,
    origin: &mut Option<f64>,
    frame_index: &mut u64,
    shared: &VideoShared,
) -> io::Result<()> {
    let mut decoded = frame::Video::empty();
    while decoder.receive_frame(&mut decoded).is_ok() {
        let mut rgb = frame::Video::empty();
        scaler.run(&decoded, &mut rgb).map_err(media_error)?;
        let width = rgb.width() as usize;
        let height = rgb.height() as usize;
        let mut pixels = Vec::with_capacity(width * height * 3);
        for row in 0..height {
            let start = row * rgb.stride(0);
            pixels.extend_from_slice(&rgb.data(0)[start..start + width * 3]);
        }
        let raw_seconds = decoded
            .timestamp()
            .map(|timestamp| timestamp as f64 * f64::from(time_base))
            .unwrap_or(*frame_index as f64 / 30.0);
        let first = *origin.get_or_insert(raw_seconds);
        let pts = Duration::from_secs_f64((raw_seconds - first).max(0.0));
        *frame_index += 1;

        let mut state = shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while state.frames.len() >= VIDEO_QUEUE_CAPACITY && !state.cancelled {
            state = shared
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        if state.cancelled {
            return Ok(());
        }
        state.frames.push_back(DecodedFrame {
            pts,
            pixels: pixels.into(),
        });
        shared.changed.notify_all();
    }
    Ok(())
}

fn cancelled(shared: &VideoShared) -> bool {
    shared
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .cancelled
}

struct AudioSource {
    receiver: Receiver<Vec<f32>>,
    current: Vec<f32>,
    offset: usize,
}

impl Iterator for AudioSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset == self.current.len() {
            self.current = self.receiver.recv().ok()?;
            self.offset = 0;
        }
        let sample = self.current[self.offset];
        self.offset += 1;
        Some(sample)
    }
}

impl Source for AudioSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        AUDIO_CHANNELS
    }

    fn sample_rate(&self) -> u32 {
        AUDIO_RATE
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

struct AudioPlayback {
    sink: Option<Sink>,
    _stream: OutputStream,
    error: Arc<Mutex<Option<String>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl AudioPlayback {
    fn start(path: PathBuf) -> io::Result<Self> {
        let (stream, handle) = OutputStream::try_default().map_err(media_error)?;
        let sink = Sink::try_new(&handle).map_err(media_error)?;
        sink.pause();
        let (sender, receiver) = mpsc::sync_channel(AUDIO_CHUNK_CAPACITY);
        let produced = Arc::new(AtomicUsize::new(0));
        let error = Arc::new(Mutex::new(None));
        let decoder_done = Arc::new(AtomicBool::new(false));
        let thread_produced = Arc::clone(&produced);
        let thread_error = Arc::clone(&error);
        let thread_done = Arc::clone(&decoder_done);
        let worker = thread::Builder::new()
            .name("tilcayo-audio-decoder".into())
            .spawn(move || {
                if let Err(decode_error) = decode_audio(&path, sender, &thread_produced) {
                    *thread_error
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) =
                        Some(decode_error.to_string());
                }
                thread_done.store(true, Ordering::Release);
            })?;
        sink.append(AudioSource {
            receiver,
            current: Vec::new(),
            offset: 0,
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while produced.load(Ordering::Acquire) < AUDIO_PREBUFFER_SAMPLES
            && !decoder_done.load(Ordering::Acquire)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(2));
        }
        if let Some(error) = error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
        {
            return Err(io::Error::other(error));
        }
        sink.play();
        Ok(Self {
            sink: Some(sink),
            _stream: stream,
            error,
            worker: Some(worker),
        })
    }

    fn position(&self) -> Duration {
        self.sink.as_ref().map_or(Duration::ZERO, Sink::get_pos)
    }

    fn pause(&self) {
        if let Some(sink) = &self.sink {
            sink.pause();
        }
    }

    fn resume(&self) {
        if let Some(sink) = &self.sink {
            sink.play();
        }
    }

    fn finished(&self) -> io::Result<bool> {
        if let Some(error) = self
            .error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
        {
            Err(io::Error::other(error))
        } else {
            Ok(self.sink.as_ref().is_none_or(Sink::empty))
        }
    }
}

impl Drop for AudioPlayback {
    fn drop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
            drop(sink);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn decode_audio(
    path: &Path,
    sender: SyncSender<Vec<f32>>,
    produced: &AtomicUsize,
) -> io::Result<()> {
    let mut input = format::input(path).map_err(media_error)?;
    let stream = input
        .streams()
        .best(Type::Audio)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no audio stream"))?;
    let stream_index = stream.index();
    let context =
        codec::context::Context::from_parameters(stream.parameters()).map_err(media_error)?;
    let mut decoder = context.decoder().audio().map_err(media_error)?;
    let source_layout = if decoder.channel_layout().is_empty() {
        ChannelLayout::default(i32::from(decoder.channels()))
    } else {
        decoder.channel_layout()
    };
    let mut resampler = resampling::Context::get(
        decoder.format(),
        source_layout,
        decoder.rate(),
        ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
        ChannelLayout::STEREO,
        AUDIO_RATE,
    )
    .map_err(media_error)?;

    for (packet_stream, packet) in input.packets() {
        if packet_stream.index() == stream_index {
            decoder.send_packet(&packet).map_err(media_error)?;
            receive_audio_frames(&mut decoder, &mut resampler, &sender, produced)?;
        }
    }
    decoder.send_eof().map_err(media_error)?;
    receive_audio_frames(&mut decoder, &mut resampler, &sender, produced)
}

fn receive_audio_frames(
    decoder: &mut ffmpeg::decoder::Audio,
    resampler: &mut resampling::Context,
    sender: &SyncSender<Vec<f32>>,
    produced: &AtomicUsize,
) -> io::Result<()> {
    let mut decoded = frame::Audio::empty();
    while decoder.receive_frame(&mut decoded).is_ok() {
        let mut output = frame::Audio::empty();
        resampler.run(&decoded, &mut output).map_err(media_error)?;
        let sample_count = output.samples() * AUDIO_CHANNELS as usize;
        let samples = output.plane::<f32>(0)[..sample_count].to_vec();
        produced.fetch_add(samples.len(), Ordering::Release);
        if sender.send(samples).is_err() {
            return Ok(());
        }
    }
    Ok(())
}

struct PlaybackClock {
    started: Instant,
    paused_at: Option<Instant>,
    paused_total: Duration,
}

impl PlaybackClock {
    fn start() -> Self {
        Self {
            started: Instant::now(),
            paused_at: None,
            paused_total: Duration::ZERO,
        }
    }

    fn position(&self) -> Duration {
        self.paused_at
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(self.started)
            .saturating_sub(self.paused_total)
    }

    fn pause(&mut self) {
        self.paused_at.get_or_insert_with(Instant::now);
    }

    fn resume(&mut self) {
        if let Some(paused_at) = self.paused_at.take() {
            self.paused_total += Instant::now().saturating_duration_since(paused_at);
        }
    }
}

fn video_placement(size: TerminalSize, width: u32, height: u32) -> io::Result<Placement> {
    let columns = size.columns.max(1);
    let rows = size.rows.max(1);
    let cell_width =
        f64::from(size.width_px.unwrap_or(columns.saturating_mul(8))) / f64::from(columns);
    let cell_height =
        f64::from(size.height_px.unwrap_or(rows.saturating_mul(16))) / f64::from(rows);
    let available_width = cell_width * f64::from(columns);
    let available_height = cell_height * f64::from(rows);
    let aspect = f64::from(width) / f64::from(height);
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
    fn fits_without_upscaling() {
        assert_eq!(fit_dimensions(1920, 1080, 1280, 720), (1280, 720));
        assert_eq!(fit_dimensions(640, 480, 1280, 720), (640, 480));
        assert_eq!(fit_dimensions(1080, 1920, 1280, 720), (405, 720));
    }
}
