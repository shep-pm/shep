//! `shep enable`/`shep disable`/`shep adopt`/`shep delete <dog>`: turning a
//! registered dog on and off, and registering or forgetting a third-party
//! one.
//!
//! None takes a connected [`Client`] and none autostarts a shepherd: all
//! four must work against a `$SHEP_HOME` with none running.
//!
//! Config first, then the daemon: [`ShepToml::edit`] runs before the socket
//! is touched, so a failed RPC still leaves a config the next boot honours.
//! `adopt` puts [`vet::vet_binary`] ahead of both.

use std::path::Path;

use shep_client::{Client, ConnectError};
use shep_core::paths::ShepPaths;
use shep_core::permissions::Grants;
use shep_core::protocol::DogSource;

use crate::commands::shep_toml::{ShepToml, ShepTomlError};
use crate::exit::ExitCode;
use crate::output::{DogActionRow, DogActionStatus, Streams, emit, write_outcome};

mod adopt;
mod barks;
mod consent;
mod depends;
mod disable;
mod enable;
mod grant;
mod grants;
mod remove;
mod second_look;
mod upgrade;
mod vet;

#[cfg(all(test, unix))]
mod adopt_consent_tests;
#[cfg(all(test, unix))]
mod adopt_declined_tests;
#[cfg(test)]
mod consent_tests;
#[cfg(all(test, unix))]
mod defaults_adoption_tests;
#[cfg(all(test, unix))]
mod emit_dog_action_tests;
#[cfg(test)]
mod enable_builtin_tests;
#[cfg(test)]
mod grant_builtin_tests;
#[cfg(all(test, unix))]
mod grant_record_tests;
#[cfg(all(test, unix))]
mod grant_tests;
#[cfg(all(test, unix))]
mod manifest_adoption_tests;
#[cfg(test)]
mod second_look_race_tests;
#[cfg(test)]
mod second_look_tests;
#[cfg(test)]
mod test_support;
#[cfg(all(test, unix))]
mod upgrade_tests;

pub use adopt::adopt;
pub use barks::barks;
pub use disable::disable;
pub use enable::enable;
pub(crate) use grant::{Change, GrantRefusal, change, record_change};
pub use remove::rehome;
pub(crate) use remove::{Removal, remove_dog};
pub(crate) use upgrade::{upgrade_grants, upgrade_notice};
pub use vet::{DogSchema, warn_of_a_dog_a_restart_would_break};
// `adopted_help` lists the registry's dogs in `--help` and in a completion
// script from the cache `adopt` already filled, and skips a name a verb
// already owns. Both reach this directory's own machinery.
pub(crate) use adopt::collides_with_a_verb;
pub(crate) use vet::cached_manifest;

// `pub(crate)` re-exports: reached from other modules by their original
// `dogs::<name>` path, so the split stays invisible to every caller outside
// this directory. The lookout config pane calls `ask_schema` and renders
// `EnableRefusal`; `hook::run_on_remove` calls `dog_env`.
pub(crate) use disable::disable_in_config;
pub(crate) use enable::{EnableRefusal, enable_in_config, join_with_and};
pub(crate) use vet::{ManifestRead, VERSION_BUDGET, ask_schema, dog_env, read_manifest};

/// [`DogActionRow`]'s status when
/// `enable` wrote the config and no shepherd answered. A success outcome:
/// `enable` never autostarts one.
const NO_SHEPHERD_ENABLE_STATUS: DogActionStatus =
    DogActionStatus::NoShepherd("will start with the next shepherd");

/// [`DogActionRow`]'s status when the operator declined to grant a dog its
/// permissions: nothing was granted and the dog was not enabled.
const DECLINED_STATUS: DogActionStatus =
    DogActionStatus::NoShepherd("not enabled; nothing was granted");

/// [`DogActionRow`]'s status when
/// `disable` wrote the config and no shepherd answered: the mirror of
/// [`NO_SHEPHERD_ENABLE_STATUS`].
const NO_SHEPHERD_DISABLE_STATUS: DogActionStatus =
    DogActionStatus::NoShepherd("not running; will not start with the next shepherd");

/// [`DogActionRow`]'s status when a
/// shepherd stopped the dog.
const DISABLED_STATUS: DogActionStatus = DogActionStatus::Acted("stopped");

/// Writes `verb`'s [`DogActionRow`] to stdout in the invocation's format
/// and returns the exit code the write earns.
///
/// Every dog verb ends here, from the branch where no shepherd answered
/// ([`NO_SHEPHERD_ENABLE_STATUS`] or [`NO_SHEPHERD_DISABLE_STATUS`]) and from
/// the one where it did ([`DISABLED_STATUS`], or a status the shepherd
/// reported).
fn emit_dog_action(
    streams: &mut Streams<'_>,
    verb: &str,
    name: &str,
    source: impl Into<Option<DogSource>>,
    status: DogActionStatus,
) -> ExitCode {
    write_outcome(emit(
        &mut *streams.out,
        streams.fmt,
        verb,
        DogActionRow::new(name, source, status),
        streams.style,
    ))
}

/// Renders `err` and returns the exit code a config-write failure reports.
///
/// [`ShepTomlError::Parse`] and [`ShepTomlError::WrongShape`] are
/// config-validation failures, so [`ExitCode::InvalidConfig`];
/// [`ShepTomlError::Io`] has no more specific code than
/// [`ExitCode::Failure`].
pub(crate) fn fail_config(streams: &mut Streams<'_>, err: &ShepTomlError) -> ExitCode {
    let code = match err {
        ShepTomlError::Io { .. } => ExitCode::Failure,
        ShepTomlError::Parse { .. }
        | ShepTomlError::WrongShape { .. }
        | ShepTomlError::BadGrants { .. } => ExitCode::InvalidConfig,
    };
    streams.fail(code, &err.to_string())
}

/// `[daemon] boot_first_dogs`: put `name` in, or take it out.
///
/// `pub(crate)`: `shep lookout`'s settings screen writes this key through
/// here. There is no verb for it, because `boot_first_dogs` is a `shep.toml`
/// key the boot reads rather than a thing you ask a running shepherd to do,
/// and an operator editing the file by hand and one editing it on screen should
/// not end up with two different shapes of the same file. Same
/// read-modify-write under the config lock every other dog edit uses.
///
/// # Errors
/// [`ShepTomlError`] from the lock, the read, `daemon` or `boot_first_dogs`
/// holding something other than an array of names, or the write.
pub(crate) fn boot_first_in_config(
    path: &Path,
    name: &str,
    first: bool,
) -> Result<(), ShepTomlError> {
    ShepToml::try_edit(path, |cfg| cfg.set_boot_first_dog(name, first))
}

/// Where `name`'s binary comes from, according to `cfg`.
///
/// A name present in `[daemon] adopted_dogs` is an adopted dog and the path
/// recorded there is its binary; a name absent is a built-in dog.
///
/// The verbs that send the source read `grants` with `?` so a typo stops
/// them; the ones that only report it pass `None`.
fn dog_source(cfg: &ShepToml, name: &str, grants: Option<Grants>) -> DogSource {
    adopted_source(cfg, name, grants.clone()).unwrap_or(DogSource::BuiltIn { grants })
}

/// [`dog_source`] for an adopted `name`, carrying its recorded channel ask
/// and `grants`, and `None` for any other name.
fn adopted_source(cfg: &ShepToml, name: &str, grants: Option<Grants>) -> Option<DogSource> {
    cfg.adopted_dog_path(name).map(|path| DogSource::Adopted {
        grants,
        path: path.display().to_string(),
        channel: cfg.asked_for_channel(name),
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
pub(crate) async fn connect_or_absent(
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
                &format!(
                    "{err}; run `shep {}`",
                    crate::version_guard::VERSION_SKEW_REMEDY
                ),
            ))
        }
    }
}
