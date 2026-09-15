//! `shep enable`/`shep disable`/`shep adopt`/`shep rehome`: turning a
//! registered dog on and off, and registering or forgetting a third-party
//! one.
//!
//! None takes a connected [`Client`] and none autostarts a shepherd: all
//! four must work against a `$SHEP_HOME` with none running, so each
//! connects for itself and tolerates a failure to reach one, writing the
//! config and reporting what the next shepherd will do.
//!
//! Config first, then the daemon: [`ShepToml::edit`] runs before the socket
//! is touched, so a failed RPC still leaves a config the next boot honours.
//! `adopt` puts [`vet::vet_binary`] ahead of both.
//!
//! One file per verb, plus [`vet`] for the binary-vetting machinery
//! `adopt` alone needs: [`enable`], [`disable`], [`adopt`], [`rehome`],
//! [`barks`]. This module holds only what every verb shares: the
//! connect-or-absent dance, the config-error mapping, and the two status
//! strings a verb's own report reaches for when no shepherd answered.

use shep_client::{Client, ConnectError};
use shep_core::paths::ShepPaths;
use shep_core::protocol::DogSource;

use crate::commands::shep_toml::{ShepToml, ShepTomlError};
use crate::exit::ExitCode;
use crate::output::Streams;

mod adopt;
mod barks;
mod disable;
mod enable;
mod rehome;
mod vet;

pub use adopt::adopt;
pub use barks::barks;
pub use disable::disable;
pub use enable::enable;
pub use rehome::rehome;
pub use vet::{DogSchema, warn_of_a_dog_a_restart_would_break};

// `pub(crate)` re-exports: reached from other modules by their original
// `dogs::<name>` path, so the split stays invisible to every caller outside
// this directory. `shep lookout`'s config pane calls `ask_schema`, matches on
// `DogSchema` (re-exported above), and renders `EnableRefusal`;
// `hook::run_on_remove` calls `dog_env`; `enable_in_config`/`disable_in_config`
// back `shep lookout`'s own dog-toggle edits; `lifecycle::restart` reads
// `VERSION_BUDGET`.
pub(crate) use disable::disable_in_config;
pub(crate) use enable::{EnableRefusal, enable_in_config};
pub(crate) use vet::{VERSION_BUDGET, ask_schema, dog_env};

/// [`DogEnabledRow::status`] when `enable` wrote the config and no shepherd
/// answered. A success outcome: `enable` never autostarts one.
const NO_SHEPHERD_ENABLE_STATUS: &str = "will start with the next shepherd";

/// [`DogDisabledRow::status`] when `disable` wrote the config and no
/// shepherd answered: the mirror of [`NO_SHEPHERD_ENABLE_STATUS`].
const NO_SHEPHERD_DISABLE_STATUS: &str = "not running; will not start with the next shepherd";

/// [`DogDisabledRow::status`] when a shepherd stopped the dog.
const DISABLED_STATUS: &str = "stopped";

/// Renders `err` and returns the exit code a config-write failure reports.
///
/// [`ShepTomlError::Parse`] and [`ShepTomlError::WrongShape`] are
/// config-validation failures, so [`ExitCode::InvalidConfig`];
/// [`ShepTomlError::Io`] has no more specific code than
/// [`ExitCode::Failure`].
fn fail_config(streams: &mut Streams<'_>, err: &ShepTomlError) -> ExitCode {
    let code = match err {
        ShepTomlError::Io { .. } => ExitCode::Failure,
        ShepTomlError::Parse { .. } | ShepTomlError::WrongShape { .. } => ExitCode::InvalidConfig,
    };
    streams.fail(code, &err.to_string())
}

/// Where `name`'s binary comes from, according to `cfg`.
///
/// A name present in `[daemon] adopted_dogs` is an adopted dog and the path
/// recorded there is its binary; a name absent from that map is a built-in
/// dog, an argv branch of this binary. `shep.toml` is the only place either
/// verb can learn it.
fn dog_source(cfg: &ShepToml, name: &str) -> DogSource {
    cfg.adopted_dog_path(name)
        .map_or(DogSource::BuiltIn, |path| DogSource::Adopted {
            path: path.display().to_string(),
        })
}

/// Connects to `paths.socket`, distinguishing a genuine absence from a
/// shepherd that is there and refused.
///
/// `Ok(None)` is only [`ConnectError::Connect`], nothing listening: the one
/// case the four verbs tolerate silently, since none may autostart a
/// shepherd. Every other variant means a connection was established, so the
/// refusal is reported rather than folded in.
///
/// # Errors
/// The exit code and message [`Streams::fail`] already wrote, when the
/// shepherd answered and refused rather than being absent.
async fn connect_or_absent(
    paths: &ShepPaths,
    streams: &mut Streams<'_>,
) -> Result<Option<Client>, ExitCode> {
    match Client::connect(&paths.socket).await {
        Ok(client) => Ok(Some(client)),
        Err(ConnectError::Connect { .. }) => Ok(None),
        Err(err) => {
            let code = ExitCode::from(&err);
            Err(streams.fail(
                code,
                &format!("{err}; run `shep {}`", crate::VERSION_SKEW_REMEDY),
            ))
        }
    }
}
