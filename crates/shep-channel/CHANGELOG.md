# Changelog

All notable changes to this crate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] - 2026-09-07


## [0.4.6] - 2026-09-07


## [0.4.5] - 2026-09-06

### Fixed

- Refuse an inherited descriptor that is not a socket


## [0.4.4] - 2026-09-06

### Added

- Frame the wire and find the descriptor
- Bound the outbound queue, drop metrics before readiness
- Always reply, including to a name nobody registered
- Serve the channel, and do nothing well without one

### Changed

- Move the shepherd-channel wire into its own crate
- Split lib.rs into error, channel, and serve modules

### Fixed

- Declare serde/std, not inherit it by accident
- Guard against taking the descriptor twice
- Count a zero-capacity drop honestly, retain nothing
- Stop lying about readiness, and catch a shutdown panic
- Make the reader loop testable, and stop dispatch's lock spanning app code
- Stop a metric evicting readiness, and say what is live
- Keep a published path alive, and ship the licences
- Address CodeRabbit review on #103
- Release the Windows channel claim when cloning the writer fails
- Drop the Windows handle before releasing the claim
- Stop the Windows channel deadlocking against its own reader
- Gate a unix-only test constant so Windows clippy is clean
- Stop overstating PeekNamedPipe, and guard the child earlier
- A frame that is not UTF-8 is Malformed, not Io
- Redact Shepherd's Debug, which printed queued payloads

