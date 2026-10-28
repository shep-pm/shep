//! `shep dogs grant` and `shep dogs revoke`: widening or tightening one
//! permission of a dog that has already been through its first question.
//!
//! Only a permission the dog's manifest declares can be granted. A dog with
//! no `dog_grants` entry is not touched: it is either undeclared, with full
//! access that one name cannot be carved out of, or it was never asked, and
//! `shep enable` asks.

use std::str::FromStr as _;

use shep_client::Client;
use shep_core::paths::ShepPaths;
use shep_core::permissions::{Grants, Permission};
use shep_core::protocol::{Request, Response};

use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::shep_toml::{GrantRecord, ShepToml, ShepTomlError};
use crate::exit::ExitCode;
use crate::output::{DogActionStatus, Streams};

use super::enable::unknown_dog_message;
use super::grants::warn_if_unenforced;
use super::upgrade::upgrade_one;
use super::vet::{ManifestRead, VERSION_BUDGET, read_manifest};
use super::{DogEntry, connect_or_absent, dog_entry, emit_dog_action, fail_config};

/// Which of the two verbs ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    /// `shep dogs grant`
    Grant,
    /// `shep dogs revoke`
    Revoke,
}

impl Change {
    /// The verb as the operator types it.
    const fn verb(self) -> &'static str {
        match self {
            Self::Grant => "grant",
            Self::Revoke => "revoke",
        }
    }
}

/// What the change left in `shep.toml`.
///
/// `pub(crate)`: [`record_change`] hands it to lookout too, which reports the
/// same record the verb wrote.
#[derive(Debug)]
pub(crate) struct Applied {
    /// The dog as the next enable would send it: the source carrying the
    /// grants now recorded, and the `env` `shep.toml` holds for it.
    pub entry: DogEntry,
    /// Whether the dog is in `enabled_dogs`.
    pub enabled: bool,
    /// What the dog's build asks for that is still not granted.
    pub waiting: Vec<String>,
}

/// Why [`record_change`] wrote nothing, and what to tell the operator.
///
/// `Debug` is derived: [`Self::Config`] forwards to [`ShepTomlError`]'s own
/// redacted `Debug`, and [`Self::Usage`]'s message names a dog and a
/// permission, never a value.
#[derive(Debug)]
pub(crate) enum GrantRefusal {
    /// A name or permission the operator has to be told about: not a
    /// permission shep can read, a name that answers to no dog, a permission
    /// the dog never declared, or a dog with no recorded grants. Carries the
    /// exit code the verb answers with.
    Usage {
        /// Which kind of refusal, so a caller cannot turn an unknown dog into
        /// a usage error.
        code: ExitCode,
        /// The sentence, already worded for the operator.
        message: String,
    },
    /// The read-modify-write underneath failed.
    Config(ShepTomlError),
}

impl From<ShepTomlError> for GrantRefusal {
    fn from(err: ShepTomlError) -> Self {
        Self::Config(err)
    }
}

/// One permission change's file half, with no streams and no shepherd: the
/// check, the record, and what the record now says.
///
/// `shep dogs grant`, `shep dogs revoke` and lookout's settings screen all
/// reach the record through here, so a surface cannot write a grant another
/// one would refuse. What the caller does with `Applied` is its own: the verb
/// also tells a running shepherd, and lookout reports the change on screen.
///
/// # Errors
/// [`GrantRefusal::Usage`] for a permission shep cannot read, a name that
/// answers to no dog, a permission the dog's manifest never declared, or a
/// dog with no recorded grants. [`GrantRefusal::Config`] when the
/// read-modify-write failed.
// Windows only, for the same reason as `commands::shep_toml`'s module-wide
// allow: `GrantRefusal::Config` carries `ShepTomlError`, and `Err` crosses
// clippy's 128-byte threshold there.
#[cfg_attr(windows, allow(clippy::result_large_err))]
pub(crate) fn record_change(
    paths: &ShepPaths,
    change: Change,
    name: &str,
    permission: &str,
) -> Result<Applied, GrantRefusal> {
    let usage = |message: String| GrantRefusal::Usage {
        code: ExitCode::Usage,
        message,
    };
    let permission = match Permission::from_str(permission) {
        Ok(permission) => permission.to_string(),
        Err(err) => return Err(usage(err.to_string())),
    };
    let cfg = ShepToml::read_only(&paths.daemon_config)?;
    let binary = cfg.adopted_dog_path(name);
    if binary.is_none() && !crate::dog::BUILT_IN_DOGS.contains(&name) {
        return Err(GrantRefusal::Usage {
            code: ExitCode::InvalidConfig,
            message: unknown_dog_message(name, &cfg.adopted_dog_names()),
        });
    }
    if change == Change::Grant {
        let manifest = match &binary {
            Some(binary) => match read_manifest(binary, &paths.home, name, VERSION_BUDGET) {
                Ok(ManifestRead::Unreadable) => return Err(usage(unreadable(name))),
                Ok(read) => read.manifest().cloned(),
                // `fail_adopt`'s own sentence, since this is the same refusal
                // seen from a caller with no streams to write it through.
                Err(refusal) => {
                    return Err(GrantRefusal::Usage {
                        code: ExitCode::InvalidConfig,
                        message: format!("{}: {refusal}", binary.display()),
                    });
                }
            },
            None => crate::dog::builtin_manifest(name),
        };
        let declared = manifest.map_or_else(Vec::new, |manifest| {
            manifest
                .permissions
                .into_iter()
                .map(|permission| permission.name)
                .collect()
        });
        if !declared.contains(&permission) {
            return Err(usage(not_declared(name, &permission, &declared)));
        }
    }
    // Before the edit, so a permission the dog's build has just come to ask
    // for is already waiting when the operator grants it. Only for a dog that
    // has a record: writing one here would mark a whole declared list
    // reviewed on the strength of one permission named.
    if matches!(cfg.grant_record(name), Ok(Some(_))) {
        upgrade_one(paths, name)?;
    }
    ShepToml::try_edit(&paths.daemon_config, |cfg| {
        let Some(held) = cfg.grant_record(name)? else {
            return Err(Refusal::Unrecorded);
        };
        let without = |grants: &Grants| {
            Grants::from_names(
                grants
                    .names()
                    .filter(|kept| *kept != permission)
                    .map(str::to_owned),
            )
        };
        let names = |grants: &Grants| grants.names().any(|held| held == permission);
        let now = match change {
            // Naming a permission is the operator looking at the dog, so the
            // not-reviewed mark goes with it. What else waits stays waiting.
            Change::Grant => GrantRecord {
                granted: Grants::from_names(
                    held.granted
                        .names()
                        .map(str::to_owned)
                        .chain([permission.clone()]),
                ),
                waiting: without(&held.waiting),
                revoked: without(&held.revoked),
                reviewed: true,
            },
            // Nothing to take away: neither held nor waiting.
            Change::Revoke if !names(&held.granted) && !names(&held.waiting) => held.clone(),
            // Remembered as revoked, so a build that still declares it is not
            // read as asking for something new.
            Change::Revoke => GrantRecord {
                granted: without(&held.granted),
                waiting: without(&held.waiting),
                revoked: Grants::from_names(
                    held.revoked
                        .names()
                        .map(str::to_owned)
                        .chain([permission.clone()]),
                ),
                reviewed: held.reviewed,
            },
        };
        if now != held {
            cfg.set_grant_record(name, &now)?;
        }
        Ok(Applied {
            entry: dog_entry(cfg, name, Some(now.granted.clone())),
            enabled: cfg.enabled_dog_names().iter().any(|dog| dog == name),
            waiting: now.waiting.names().map(str::to_owned).collect(),
        })
    })
    .map_err(|refusal: Refusal| match refusal {
        Refusal::Config(err) => GrantRefusal::Config(err),
        Refusal::Unrecorded => usage(unrecorded(name)),
    })
}

/// [`change`]'s own failure, so its `try_edit` closure can refuse from
/// inside the lock.
///
/// `Debug` is derived: [`Self::Config`] forwards to [`ShepTomlError`]'s own
/// redacted `Debug`.
#[derive(Debug)]
enum Refusal {
    /// The read-modify-write underneath the closure failed.
    Config(ShepTomlError),
    /// The dog has no `dog_grants` entry.
    Unrecorded,
}

impl From<ShepTomlError> for Refusal {
    fn from(err: ShepTomlError) -> Self {
        Self::Config(err)
    }
}

/// `shep dogs grant|revoke <name> <permission>`: records the change in
/// `shep.toml`.
pub(crate) async fn change(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    change: Change,
    name: &str,
    permission: &str,
) -> ExitCode {
    let applied = match record_change(paths, change, name, permission) {
        Ok(applied) => applied,
        Err(GrantRefusal::Usage { code, message }) => return streams.fail(code, &message),
        Err(GrantRefusal::Config(err)) => return fail_config(streams, &err),
    };
    // An enable would start a dog the operator switched off; a disabled
    // dog picks the grants up from `shep.toml` when it is enabled.
    let client = if applied.enabled {
        match connect_or_absent(paths, streams).await {
            Ok(client) => client,
            Err(code) => return code,
        }
    } else {
        None
    };
    if !applied.waiting.is_empty() {
        streams.aside(
            "dog_grants_waiting",
            &format!(
                "{name} still waits on {}: `shep dogs grant {name} <permission>` or `shep enable \
                 {name}` approves",
                applied.waiting.join(", ")
            ),
        );
    }
    reach_shepherd(streams, change, name, &applied, client.as_ref()).await
}

/// Tells a running shepherd what `name` now holds, the way a re-enable does:
/// the shepherd records the grants a source carries before it finds the dog
/// already running, so the dog's next request is judged by them.
async fn reach_shepherd(
    streams: &mut Streams<'_>,
    change: Change,
    name: &str,
    applied: &Applied,
    client: Option<&Client>,
) -> ExitCode {
    let report = |streams: &mut Streams<'_>, status| {
        emit_dog_action(
            streams,
            change.verb(),
            name,
            applied.entry.source.clone(),
            status,
        )
    };
    if !applied.enabled {
        return report(
            streams,
            DogActionStatus::NoShepherd("recorded; the dog is disabled and gets it when enabled"),
        );
    }
    let Some(client) = client else {
        return report(
            streams,
            DogActionStatus::NoShepherd(
                "recorded; takes effect when the next shepherd starts the dog",
            ),
        );
    };
    let request = Request::EnableDog {
        name: name.to_string(),
        source: applied.entry.source.clone(),
        env: applied.entry.env.clone(),
    };
    match client.request(request).await {
        Ok(Response::DogStarted(_)) => {
            warn_if_unenforced(
                streams,
                name,
                &applied.entry.source,
                client.daemon().protocol,
            );
            report(
                streams,
                DogActionStatus::Acted("applied to the running dog"),
            )
        }
        Ok(_unrecognised) => unexpected_response(streams),
        Err(err) => client_error(streams, &err),
    }
}

/// The refusal of a grant the manifest never asked for.
fn not_declared(name: &str, permission: &str, declared: &[String]) -> String {
    if declared.is_empty() {
        return format!(
            "{name} declares no permissions, so `{permission}` cannot be granted to it"
        );
    }
    let mut sorted = declared.to_vec();
    sorted.sort();
    format!(
        "{name} did not declare `{permission}`, so it cannot be granted. It declares: {}",
        sorted.join(", ")
    )
}

/// The refusal of a grant when the record of what the dog declares is lost.
fn unreadable(name: &str) -> String {
    format!(
        "{name}'s manifest record could not be read, so what it declares is unknown. \
         Nothing was changed; the cache under `manifests/` in the shep home is safe to delete"
    )
}

/// The refusal of a dog with no recorded grants.
fn unrecorded(name: &str) -> String {
    format!(
        "{name} has no recorded grants: it either declares nothing and runs with full access, \
         or was never asked. Nothing was changed; `shep enable {name}` lists its \
         permissions and asks"
    )
}
