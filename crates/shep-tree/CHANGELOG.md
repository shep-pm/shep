# Changelog

All notable changes to this crate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.20.0] - 2026-10-31

### Added

- Sweep what a process left running through a step machine

### Changed

- Drive lamb sweeps through shep-tree
- Sweep lambs through shep-tree

### Fixed

- Take a sweep look's own time out of its sleep


### Added

- Re-export `ProcInstant` and the lamb sweep types at the crate root
- Sweep what a process left running: `sweep::Sweeper` ends a `sweep::LambSnapshot`'s
  lambs a look at a time or in one blocking call, `sweep::ProcessTable` reads the
  real process table, and `instant` dates a snapshot and re-checks a pid against it
- `sysinfo` feature, on by default, carrying the process table. Off, the crate pulls
  no process-table dependency

## [0.19.0] - 2026-10-28

### Fixed

- Reap the child only after the final group kill
- Probe the child again when the final kill is denied


## [0.18.2] - 2026-10-26


## [0.18.1] - 2026-10-26


## [0.18.0] - 2026-10-26

# Changelog

All notable changes to this crate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
### Added

- Add shep-tree, signal a process group, own a Windows job and stop a child
### Fixed

- Stop a child that left its group instead of waiting on it forever
- Signal a child that left its group even while the group survives
