# Rust house style: shep addendum

shep's local layer over [shep-pm/rust-house-style](https://github.com/shep-pm/rust-house-style)
(rules IR-1 to IR-48). Where this file and the rules disagree, this file wins.
`shep` below means the CLI crate in `crates/shep-cli`, the one binary crate.

## Rules made specific

- **IR-7** `#![forbid(unsafe_code)]` in shep-core, shep-client, shep-macros and
  `shep`. shep-daemon, shep-channel, shep-handover and shep-tree deny it and allow it only
  in the modules IR-22 names.
- **IR-9** `doc-valid-idents` lists `"PID"`, `"SIGKILL"`, `"systemd"`, `"OTel"`.
- **IR-13** Each request type registers its response type once, in shep-core.
- **IR-18** `ProtocolError` lives in shep-core, `SpawnError` in shep-daemon,
  `ConnectError` in shep-client. `anyhow` is allowed only in `shep`.
- **IR-20** shep-core, shep-daemon and shep-client are library crates, so
  their `pub` error enums are `#[non_exhaustive]`. `shep` has a `[lib]` target
  whose surface is three `ExitCode`-returning entry points (`main`,
  `main_runtime`, `main_dev`) over private modules, so its error enums are not.
  `CronScheduleError` is the model comment for the negative case. `ProcessInfo`,
  the one wire struct, carries the attribute like every wire enum.
- **IR-21** The panicking conveniences allowed in binary crates live only in
  `shep`.
- **IR-22, IR-23** Unsafe lives only in shep-daemon's `sys.rs` and
  `sys_windows.rs`, shep-channel's `endpoint.rs`, shep-handover's `sys.rs` and shep-tree's
  `job.rs`.
  Each has its own `allow(unsafe_code)` and a `// SAFETY:` comment per block.
- **IR-29** The canonical `# Security` writeup sits on the daemon/socket type.
- **IR-33** shep-daemon's fixtures are a paused clock and a two-tier fake
  process runner, `const_proc(exit)` and `script_proc(vec![...])`, both
  implementing the real `ProcessRunner` trait.
- **IR-39** Each E2E test gets a fresh temp `SHEP_HOME`.
- **IR-42** The daemon socket is a privilege boundary, so `SECURITY.md`
  matters more here than for most crates.
- **IR-48** Known offenders are listed in `.github/rust-file-size-baseline.txt`,
  each tied to its issue. #301 tracks them, with the regenerate command and two
  traps to read before splitting one.

## Deviations

- **IR-31** Private items in shep-daemon and shep-cli keep their rationale in
  `///`, not `//`. Each file is consistent about it, and converting some items
  would split a file's voice for no reader's benefit: a private item has no
  rendered-docs user. Settled, not reopened per review.

## Terminology

User-facing naming follows [terminology.md](terminology.md): flock, fold,
Flockfile, bleats, bark, whistle. Destructive ops and error text stay plain.
