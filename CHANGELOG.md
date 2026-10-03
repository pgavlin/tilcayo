# Changelog

All notable changes to Tilcayo will be documented in this file.

The format is based on [Keep a Changelog], and this project adheres to
[Semantic Versioning].

## [Unreleased]

### Added

- Initial damage-aware Kitty graphics presentation pipeline.
- Atomic frame-and-placement presentation requests.
- Pollable input, presentation-completion, failure, and shutdown wakeups.
- Bounded terminal input with adjacent pointer-motion coalescing.

### Changed

- Made validated `Frame` layout and damage fields private and exposed read-only accessors.
- Automatic graphics transport now falls back to direct transfer when local media fail.

### Fixed

- Presenter exit wakeups and completion publication when instrumentation panics.
- Ordered runtime shutdown after worker errors and presenter invalidation after failed deletion.

[Unreleased]: https://github.com/pgavlin/tilcayo/commits/main
[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[Semantic Versioning]: https://semver.org/spec/v2.0.0.html
