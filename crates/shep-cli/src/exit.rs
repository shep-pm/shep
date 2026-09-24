//! The process exit-code taxonomy: [`ExitCode`] and its wire-error
//! conversions. `docs/specs/shep-v1.md` §9 is the source of truth for the
//! numbers and their meanings.
//!
//! Every code a dog can also exit on is defined from [`shep_core::exit`], and
//! every conversion defers to the error's own `exit_code()`, so a built-in
//! dog and an adopted one report a cause with one number.

use shep_core::exit;
use shep_core::protocol::RpcErrorCode;

/// A `shep` process exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitCode {
    /// The command did what it was asked.
    #[cfg_attr(windows, allow(dead_code))]
    Success = 0,
    /// An error with no more specific code.
    Failure = exit::FAILURE,
    /// Bad arguments. clap's own convention.
    Usage = exit::USAGE,
    /// A selector matched no registered sheep.
    NotFound = exit::NOT_FOUND,
    /// A Flockfile or daemon config failed validation.
    InvalidConfig = exit::INVALID_CONFIG,
    /// No daemon answered, and none could be started.
    #[cfg_attr(windows, allow(dead_code))]
    DaemonUnreachable = exit::DAEMON_UNREACHABLE,
    /// The daemon refused this client's handshake: its protocol version is
    /// below the daemon's `MIN_SUPPORTED` floor.
    ProtocolMismatch = exit::PROTOCOL_MISMATCH,
    /// The daemon could not spawn a sheep.
    SpawnFailed = exit::SPAWN_FAILED,
    /// The request outlived its deadline.
    DeadlineExceeded = exit::DEADLINE_EXCEEDED,
    /// An unexpected daemon-side failure.
    Internal = exit::INTERNAL,
    /// Another daemon already holds this `$SHEP_HOME`. Read across the
    /// process boundary by `shep_client::spawn::DAEMON_ALREADY_RUNNING`,
    /// which must stay equal to 10.
    ///
    /// How a losing child in a cold-start race tells the probing parent it
    /// lost.
    #[cfg_attr(windows, allow(dead_code))]
    DaemonAlreadyRunning = 10,
    /// The flock emptied and something in it had failed.
    ///
    /// `runtime`'s fail-fast status: nothing is online any more and at least
    /// one sheep ended `errored`. A flock that emptied cleanly, every sheep
    /// `stopped`, exits `Success` instead.
    FlockEmpty = 11,
    /// This binary and the running shepherd are different versions of shep.
    ///
    /// The handshake succeeded and only the crate versions differ, the state
    /// `cargo install shep` leaves behind. A wire disagreement is
    /// [`ExitCode::ProtocolMismatch`] instead. Never returned for `kill`,
    /// `daemon reload` or `ping`, which are how an operator gets out of it.
    VersionSkew = 12,
    /// The daemon understood the handshake but not the request itself.
    ///
    /// [`RpcErrorCode::Unsupported`]: the verb this binary sent is not one
    /// the connected daemon implements, so a newer shepherd is the remedy
    /// rather than different arguments.
    Unsupported = exit::UNSUPPORTED,
}

impl ExitCode {
    /// The stable machine-readable spelling of this code, as it appears in
    /// `--format json`'s `error.code` field (`"not_found"`, `"usage"`, …).
    ///
    /// The single place those strings are written: `emit_error` takes the
    /// code as a `&str`, so no verb invents its own spelling.
    #[must_use]
    pub const fn code_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Usage => "usage",
            Self::NotFound => "not_found",
            Self::InvalidConfig => "invalid_config",
            Self::DaemonUnreachable => "daemon_unreachable",
            Self::ProtocolMismatch => "protocol_mismatch",
            Self::SpawnFailed => "spawn_failed",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::Internal => "internal",
            Self::DaemonAlreadyRunning => "daemon_already_running",
            Self::FlockEmpty => "flock_empty",
            Self::VersionSkew => "version_skew",
            Self::Unsupported => "unsupported",
        }
    }
}

impl ExitCode {
    /// The variant [`shep_core::exit`] spells `code`, and
    /// [`ExitCode::Failure`] for a number it does not define.
    ///
    /// For the conversions below, whose numbers shep-core and shep-client
    /// decide.
    const fn from_shared(code: u8) -> Self {
        match code {
            exit::USAGE => Self::Usage,
            exit::NOT_FOUND => Self::NotFound,
            exit::INVALID_CONFIG => Self::InvalidConfig,
            exit::DAEMON_UNREACHABLE => Self::DaemonUnreachable,
            exit::PROTOCOL_MISMATCH => Self::ProtocolMismatch,
            exit::SPAWN_FAILED => Self::SpawnFailed,
            exit::DEADLINE_EXCEEDED => Self::DeadlineExceeded,
            exit::INTERNAL => Self::Internal,
            exit::UNSUPPORTED => Self::Unsupported,
            _ => Self::Failure,
        }
    }
}

/// Maps a daemon-reported [`RpcErrorCode`] to the exit code that reports it,
/// through [`RpcErrorCode::exit_code`].
impl From<RpcErrorCode> for ExitCode {
    fn from(code: RpcErrorCode) -> Self {
        Self::from_shared(code.exit_code())
    }
}

/// Maps a failure to reach the daemon at all to the exit code that reports
/// it, through [`shep_client::ConnectError::exit_code`].
impl From<&shep_client::ConnectError> for ExitCode {
    fn from(err: &shep_client::ConnectError) -> Self {
        Self::from_shared(err.exit_code())
    }
}

/// Maps a failed request against an already-connected daemon to the exit
/// code that reports it, through [`shep_client::RequestError::exit_code`].
impl From<&shep_client::RequestError> for ExitCode {
    fn from(err: &shep_client::RequestError) -> Self {
        Self::from_shared(err.exit_code())
    }
}

/// Maps a dog giving up on its shepherd to the exit code that reports it,
/// through [`shep_client::LinkLost::exit_code`].
impl From<&shep_client::LinkLost> for ExitCode {
    fn from(lost: &shep_client::LinkLost) -> Self {
        Self::from_shared(lost.exit_code())
    }
}

/// Maps a dog failing on its shepherd to the exit code that reports it,
/// through [`shep_client::dogs::ShepherdError::exit_code`].
impl From<&shep_client::dogs::ShepherdError> for ExitCode {
    fn from(err: &shep_client::dogs::ShepherdError) -> Self {
        Self::from_shared(err.exit_code())
    }
}

/// Maps a failed `connect_or_spawn` attempt to the exit code that reports
/// it, through [`shep_client::spawn::SpawnError::exit_code`].
impl From<&shep_client::spawn::SpawnError> for ExitCode {
    fn from(err: &shep_client::spawn::SpawnError) -> Self {
        Self::from_shared(err.exit_code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rpc_error_code_maps_to_a_distinct_nonzero_exit_code() {
        // `ALL` and `RpcErrorCode::exit_code` are both exhaustive inside
        // shep-core, so a new variant lands here with a number of its own,
        // and one reusing another's fails the distinctness assertion.
        let codes = shep_core::protocol::RpcErrorCode::ALL;
        let mapped: Vec<u8> = codes.iter().map(|c| ExitCode::from(*c) as u8).collect();
        assert!(
            mapped.iter().all(|&c| c != 0),
            "no error may map to Success"
        );
        let unique: std::collections::HashSet<_> = mapped.iter().collect();
        assert_eq!(
            unique.len(),
            mapped.len(),
            "distinct causes need distinct exit codes: {mapped:?}"
        );
    }

    /// fails if a shared number reads back as a different variant, which
    /// would report one cause under another's name.
    #[test]
    fn every_shared_code_reads_back_as_the_variant_it_defines() {
        let shared = [
            ExitCode::Failure,
            ExitCode::Usage,
            ExitCode::NotFound,
            ExitCode::InvalidConfig,
            ExitCode::DaemonUnreachable,
            ExitCode::ProtocolMismatch,
            ExitCode::SpawnFailed,
            ExitCode::DeadlineExceeded,
            ExitCode::Internal,
            ExitCode::Unsupported,
        ];
        for code in shared {
            assert_eq!(ExitCode::from_shared(code as u8), code);
        }
    }

    /// Distinctness is the property; the exact words are pinned by a later
    /// snapshot.
    #[test]
    fn every_exit_code_has_its_own_machine_readable_spelling() {
        let all = [
            ExitCode::Success,
            ExitCode::Failure,
            ExitCode::Usage,
            ExitCode::NotFound,
            ExitCode::InvalidConfig,
            ExitCode::DaemonUnreachable,
            ExitCode::ProtocolMismatch,
            ExitCode::SpawnFailed,
            ExitCode::DeadlineExceeded,
            ExitCode::Internal,
            ExitCode::DaemonAlreadyRunning,
            ExitCode::FlockEmpty,
            ExitCode::VersionSkew,
            ExitCode::Unsupported,
        ];
        let strings: Vec<&str> = all.iter().map(|c| c.code_str()).collect();
        assert!(strings.iter().all(|s| !s.is_empty()));
        assert!(
            strings
                .iter()
                .all(|s| s.chars().all(|c| c.is_ascii_lowercase() || c == '_')),
            "these go on the JSON surface: {strings:?}"
        );
        let unique: std::collections::HashSet<_> = strings.iter().collect();
        assert_eq!(
            unique.len(),
            strings.len(),
            "duplicated spelling: {strings:?}"
        );
    }

    /// The one number both crates hard-code. If they diverge, the cold-start
    /// race in `connect_or_spawn` becomes a fatal error.
    #[cfg(unix)]
    #[test]
    fn the_already_running_exit_code_matches_the_clients_constant() {
        assert_eq!(
            ExitCode::DaemonAlreadyRunning as i32,
            shep_client::spawn::DAEMON_ALREADY_RUNNING
        );
    }
}
