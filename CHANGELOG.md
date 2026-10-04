# Changelog

All notable changes to Tilcayo will be documented in this file.

The format is based on [Keep a Changelog], and this project adheres to
[Semantic Versioning].

## [Unreleased]

### Fixed

- Prevented maximum placement origins from overflowing when converted to terminal coordinates.

## [0.2.0] - 2026-10-04

### Added

- Initial damage-aware Kitty graphics presentation pipeline.
- Atomic frame-and-placement presentation requests.
- Pollable input, presentation-completion, failure, and shutdown wakeups.
- Bounded terminal input with adjacent pointer-motion coalescing.
- Completion-paced, damage-tracked Boing Ball example.
- Native MP4 player package with bounded FFmpeg audio/video decoding, audio-clock synchronization, late-frame dropping, resizing, and pause/resume.

### Changed

- Raised the minimum supported Rust version from 1.80 to 1.85.
- Replaced the split Termwiz/Crossterm probe input path with Crossterm terminal queries, preserving startup input in the normal event queue without a parser handoff.
- Simplified `kitty::probe` and `kitty::probe_terminal` to use Crossterm's terminal input reader; removed the separate `*_with_events` and `wait_for_ack` APIs.
- Made validated `Frame` layout and damage fields private and exposed read-only accessors.
- Automatic graphics transport now falls back to direct transfer when local media fail.

### Fixed

- Presenter exit wakeups and completion publication when instrumentation panics.
- Ordered runtime shutdown after worker errors and presenter invalidation after failed deletion.

[Unreleased]: https://github.com/pgavlin/tilcayo/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/pgavlin/tilcayo/releases/tag/v0.2.0
[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[Semantic Versioning]: https://semver.org/spec/v2.0.0.html
