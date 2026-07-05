# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-07-04

### Fixed
- A half-closed connection no longer hangs. When one direction reached EOF while the other was
  still open but had no data ready, the poll loop spun internally instead of yielding — a busy-spin
  that deadlocks on a single-threaded executor (e.g. proxying a websocket with one side closed).
  The opposite direction now keeps relaying and the future yields correctly.

### Changed
- Relaxed the transport bounds: `full_duplex_copy` no longer requires `Send + Sync` on its generic
  parameters.
- Poll order is now deterministic (strict alternation) rather than a `fastrand` coin flip, so error
  surfacing is reproducible. Also replaced the vendored `BufReader` with `futures_lite`'s.
