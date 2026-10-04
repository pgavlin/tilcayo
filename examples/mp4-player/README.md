# Tilcayo MP4 player

This is a separate package because native video decoding and audio output are
outside Tilcayo's core responsibilities. It uses `ffmpeg-next` for in-process
demuxing, decoding, scaling, and resampling, and Rodio for host audio output.
No FFmpeg command-line programs are launched.

Install FFmpeg development libraries and ensure `pkg-config` can find them. On
macOS with Homebrew:

```sh
brew install ffmpeg pkg-config
```

Then run from the repository root:

```sh
cargo run --release --manifest-path examples/mp4-player/Cargo.toml -- video.mp4
```

Space pauses or resumes playback; `q` and Escape exit. Use `--no-audio` when
no output device is available. `--max-width` and `--max-height` bound the RGB
framebuffer size.

The player uses a bounded video queue and bounded audio channel. Audio is the
master clock when enabled, and video frames that become late are discarded
rather than allowed to accumulate. Tilcayo presentation completions provide
submission backpressure, but indicate terminal writes rather than display
acknowledgements.

The player package and Tilcayo library require Rust 1.85. The FFmpeg 9 bindings
use Cargo's 2024-edition support.
