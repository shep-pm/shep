//! What the supervisor refuses, and why.
//!
//! [`SupervisorError`] is the public refusal type: every fallible
//! [`SupervisorHandle`] method returns it, and `rpc` maps each variant onto a
//! wire error. `AdoptError` is internal to handover and reports why a
//! surviving process could not be taken back over.

use super::*;

/// Error type returned from supervisor commands.
///
/// `#[non_exhaustive]`: shep-daemon is a published library, so a new variant
/// must not break an out-of-tree matcher.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorError {
    /// The selector matched no registered sheep.
    NotFound,
    /// Spawn failed (carries the runner's message).
    SpawnFailed(String),
    /// A command that would have started processes was refused before any of
    /// them was registered, because what it was asked to start provably could
    /// not run. Carries `"<name>: <reason>"` per app that could not.
    ///
    /// Unlike [`Self::SpawnFailed`], which can leave earlier apps registered
    /// and running, this guarantees an untouched flock for the batch it
    /// refuses. That is not the whole flock when the caller is
    /// `boot_order::start_in_stages`, which sends one batch per stage and
    /// leaves an earlier stage running; the message it answers with names
    /// those apps. Both map to
    /// [`RpcErrorCode::SpawnFailed`](shep_core::protocol::RpcErrorCode::SpawnFailed).
    CannotStart(String),
    /// The selector reached an app that is already being reloaded; carries
    /// that app's name.
    ///
    /// A second reload would put a third entry in an instance slot two are
    /// already sharing. The whole command is refused, never half of it.
    ReloadInFlight(String),
    /// A `Scale` the engine will not perform; carries the refusal in plain
    /// English, naming what to do instead.
    ///
    /// Four shapes: a count of `0`, a target that is a dog, an app with
    /// departures still in flight, and a rescaled config that failed
    /// `normalize`. Maps to
    /// [`RpcErrorCode::InvalidConfig`](shep_core::protocol::RpcErrorCode::InvalidConfig),
    /// since each is something the caller can ask differently.
    InvalidScale(String),
    /// At least one log pump could not open a log path again, so that stream
    /// has no file to write to. Carries one
    /// `"<name> (id <id>): <paths and reasons>"` entry per such sheep, joined
    /// by `"; "`, with one sheep's own two paths joined by `", "` (see
    /// [`ReopenError::message`](crate::runner::ReopenError::message)). Every
    /// other pump was reopened.
    ///
    /// A named sheep can be one the selector did not match: the reach is every
    /// writer to a matched path.
    ReopenFailed(String),
    /// At least one matched log file could not be flushed or truncated.
    /// Carries one `"<path>: <reason>"` entry per such file, joined by `"; "`,
    /// where a pump that failed on both streams contributes one entry with the
    /// two paths joined by `", "`. Every other matched path was emptied.
    ///
    /// A failed truncate leaves its file as it was; a failed flush does not
    /// stop the truncate. Keyed by path where [`Self::ReopenFailed`] is keyed
    /// by sheep, one file belonging to several sheep (see
    /// [`FlushError::message`](crate::runner::FlushError::message)).
    FlushFailed(String),
    /// A request whose target is a dog, and which a dog is not a valid
    /// target for; carries the refusal in plain English.
    ///
    /// A dog runs at the daemon's own trust level and its binary is what
    /// `shep adopt` vetted, so its config is not an operator's to edit
    /// through a pane and not a pane's to read. `Actor::apply_one` refuses
    /// a Flockfile the same way and in the same words, and
    /// `Actor::handle_scale` refuses a scale for the neighbouring reason.
    ///
    /// Maps to
    /// [`RpcErrorCode::InvalidConfig`](shep_core::protocol::RpcErrorCode::InvalidConfig),
    /// like [`Self::InvalidScale`] and for its reason: this is something
    /// the caller asked for that it can ask differently.
    IsADog(String),
    /// A `SetSheepEnv` whose result is not a config this build accepts;
    /// carries `normalize`'s own refusal.
    ///
    /// One shape reaches it today: `SHEP_INSTANCE` and `SHEP_NAME` are
    /// injected per instance and refused in a hand-written `env`, so a pane
    /// offering a free-text key can be asked to set one. Nothing is written
    /// on it: the config is checked before the store is touched, so the
    /// operator's stored env is exactly what it was.
    ///
    /// Separate from [`Self::Overrides`] because the two want opposite
    /// things of the operator: this one is a request to change, and that
    /// one is a store to fix.
    InvalidEnv(String),
    /// A `SetSheepField` this build will not take; carries the reason.
    ///
    /// [`Self::InvalidEnv`]'s twin, separate for the same reason that one
    /// is separate from [`Self::Overrides`]: the two want opposite things
    /// of the operator. Four shapes reach it, and all four are the caller's
    /// own request rather than a fault: a key [`AppConfig`] has no field
    /// for, a value that will not deserialize into the field it names, a
    /// config `normalize` refuses once the value is in, and the three keys
    /// this door does not own (`env`, which has
    /// `Handle::set_sheep_env`, and the two Structural ones). Nothing is
    /// written on any of them: the config is checked before the store is
    /// touched.
    InvalidField(String),
    /// The override store at `$SHEP_HOME/overrides.json` could not be read
    /// or written, so an operator's edit was not recorded. Carries
    /// [`OverridesError`](shep_core::overrides::OverridesError)'s own
    /// message, which names which of the three (I/O, a parse, a future
    /// version) it was.
    ///
    /// Its own variant rather than [`Self::SpawnFailed`] or an `Internal`
    /// string, because it is the one failure here that leaves the flock
    /// exactly as it was while telling the operator their change did not
    /// land. Nothing was spawned, nothing was killed, and the file on disk
    /// is what it was before the request.
    Overrides(String),
    /// The actor has shut down; its mailbox is closed.
    EngineStopped,
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("selector matched no registered sheep"),
            Self::SpawnFailed(msg) => write!(f, "spawn failed: {msg}"),
            Self::CannotStart(msg) => write!(f, "start refused: {msg}"),
            Self::ReloadInFlight(name) => write!(f, "{name} is already being reloaded"),
            Self::InvalidScale(msg) => write!(f, "cannot scale: {msg}"),
            Self::ReopenFailed(msg) => write!(f, "log reopen failed: {msg}"),
            Self::FlushFailed(msg) => write!(f, "log flush failed: {msg}"),
            Self::IsADog(msg) => write!(f, "refused: {msg}"),
            Self::InvalidEnv(msg) => write!(f, "cannot set that env key: {msg}"),
            Self::InvalidField(msg) => write!(f, "cannot set that field: {msg}"),
            Self::Overrides(msg) => write!(f, "overrides store unusable: {msg}"),
            Self::EngineStopped => f.write_str("supervisor engine has shut down"),
        }
    }
}

impl core::error::Error for SupervisorError {}

/// Why a flock this image inherited could not be installed.
///
/// Both variants name the sheep: that is which process is now unsupervised.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) enum AdoptError {
    /// The config the blob carried for this sheep does not normalize.
    ///
    /// Reachable when a successor's `normalize` has tightened since the
    /// predecessor accepted the app; the muster roll meets the same refusal.
    Spec {
        /// The sheep whose config was refused.
        sheep: String,
        /// What `normalize` said about it.
        source: shep_core::config::NormalizeError,
    },
    /// The runner would not take this sheep's inherited handles.
    ///
    /// A runner that never took part in a handover refuses by default, and
    /// a real one refuses a handle it cannot wire to a pump.
    Runner {
        /// The sheep whose adoption was refused.
        sheep: String,
        /// What the runner said about it.
        source: RunnerError,
    },
}

#[cfg(unix)]
impl fmt::Display for AdoptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spec { sheep, source } => {
                write!(
                    f,
                    "sheep '{sheep}' carried a config that no longer validates: {source}"
                )
            }
            Self::Runner { sheep, source } => {
                write!(f, "sheep '{sheep}' could not be adopted: {source}")
            }
        }
    }
}

#[cfg(unix)]
impl core::error::Error for AdoptError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Spec { source, .. } => Some(source),
            Self::Runner { source, .. } => Some(source),
        }
    }
}
