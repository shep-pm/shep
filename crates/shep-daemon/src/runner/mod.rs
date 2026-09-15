//! Spawn seam between the daemon engine and the OS
//!
//! [`ProcessRunner`] spawns a child; the [`RunningProcess`] it returns owns
//! that one live child. Spawn also hands back a [`ProcIo`] bundle of
//! channels, so the sheep task pumps stdout/stderr and shepherd-channel
//! messages without the runner blocking on delivery.
//!
//! Also owns the log plane's vocabulary ([`LogCtl`] and its two errors) and
//! this crate's only opener of a sheep's log file, `open_log_path`, with the
//! ancestry guard that runs ahead of it. The log pump and `shep flush` both
//! go through the pair, so neither can drift on what it will open. The
//! `#[cfg(unix)]` items are the handover's; Windows has no `execve`.

/// Re-exported so [`AdoptSpec`]'s public signature can name it: the reaper
/// itself lives in the crate-private `handover` module.
#[cfg(unix)]
pub use crate::handover::reap::AdoptedReaper;
mod log_path_security;
mod log_protocol;
mod path_advisories;
mod process_runner;
#[cfg(test)]
mod testing;
/// Named here only so the two test modules that assert on the refusal text
/// can reach it; the lib build has no other reader and warns on the import.
#[cfg(test)]
pub(crate) use log_path_security::SYMLINK_REFUSED;
pub(crate) use log_path_security::{check_log_ancestry, open_log_path};
pub use log_protocol::{ExitOutcome, FlushError, LogCtl, LogLine, ReopenError};
pub(crate) use path_advisories::{cwd_advisory, log_path_advisory};
pub use process_runner::{
    AdoptSpec, Preflight, ProcIo, ProcessRunner, RunnerError, RunningProcess, SpawnSpec,
    StdinWrite, StopSignal,
};
