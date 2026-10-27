# Changelog

All notable changes to this crate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
