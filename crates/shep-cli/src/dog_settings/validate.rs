//! The dog's own say in a write to its settings.
//!
//! A dog whose manifest declares
//! [`validate`](shep_core::dogs::manifest::Manifest::validate) runs checks its
//! schema cannot state, so a write carrying one of its tables asks the dog
//! first: a `shep.validate` trigger, carrying the table the write would put
//! in place. A refusal, a timeout or an unreadable reply all stop the write.
//!
//! One call, [`ask_dog`], and every door that writes a dog's settings uses it.
//! A dog that declared nothing is never asked.

use serde_json::{Map, Value as Json};
use shep_client::Client;
use shep_client::RequestError;
use shep_core::dogs::validate::{Proposed, VALIDATE_ACTION, Verdict};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{ActionOutcome, Request, Response, RpcError, RpcErrorCode, SelectorSpec};

use crate::commands::dogs::{ManifestRead, VERSION_BUDGET, read_manifest};
use crate::commands::rpc::deadline_for;
use crate::commands::settings_write::value::table_json;
use crate::commands::shep_toml::ShepToml;

/// Why a write stopped at the dog rather than reaching the shepherd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AskFailure {
    /// The dog refused the table, in its own words.
    Refused {
        /// The sentence the operator reads.
        reason: String,
    },
    /// Whether the dog wanted to be consulted is unknown: what says so, or what
    /// answers it, could not be read. Not [`Self::Refused`]: no dog said this.
    Unknown {
        /// The sentence the operator reads.
        reason: String,
    },
    /// The ask itself never produced an answer.
    Transport(RequestError),
}

impl AskFailure {
    /// The refusal as the dashboard reports one: a daemon-side refusal of the
    /// write, carrying the dog's own sentence.
    ///
    /// [`RpcErrorCode::InvalidConfig`] is the code the shepherd itself uses
    /// for a write it will not make, so a pane reads a dog's refusal and a
    /// daemon's refusal alike.
    pub(crate) fn request_error(&self) -> RequestError {
        match self {
            Self::Refused { reason } | Self::Unknown { reason } => RequestError::Rpc(RpcError {
                code: RpcErrorCode::InvalidConfig,
                message: reason.clone(),
                daemon_version: None,
            }),
            Self::Transport(err) => err.clone(),
        }
    }
}

/// Asks `dog` whether it accepts `table`, and answers for it when there is
/// no dog to ask.
///
/// `sheep` names the sheep whose table this is, and `None` says it is the
/// dog's own section. `Ok(())` covers the dog accepting and the three cases
/// with nobody to ask: not adopted here, not in this flock, and in it with no
/// live channel.
///
/// # Errors
/// [`AskFailure::Refused`] for a refusal and for a dog that answered nothing
/// inside its own `action_timeout`. [`AskFailure::Unknown`] for an answer with
/// no verdict in it, and for a manifest that could not be read.
/// [`AskFailure::Transport`] for an ask that never reached the shepherd.
pub(crate) async fn ask_dog(
    client: &Client,
    paths: &ShepPaths,
    dog: &str,
    table: &Map<String, Json>,
    sheep: Option<&str>,
) -> Result<(), AskFailure> {
    match declares_validate(dog, paths) {
        Declaration::Silent => return Ok(()),
        Declaration::Declared => {}
        // A dog that asked to be consulted and cannot be reached as one is not
        // a dog that cleared the write.
        Declaration::Unknown { reason } => return Err(AskFailure::Unknown { reason }),
    }
    let proposed = Proposed::new(table.clone(), sheep.map(str::to_owned));
    let ask = Request::Trigger {
        selector: SelectorSpec::Name(dog.to_string()),
        action: VALIDATE_ACTION.to_string(),
        params: Some(proposed.params()),
    };
    let deadline = deadline_for(&ask);
    match client.request_with_deadline(ask, deadline).await {
        Ok(Response::Triggered(rows)) => {
            if let Some(failure) = rows.iter().find_map(|row| refusal_for(dog, &row.outcome)) {
                return Err(failure);
            }
            // The shepherd matched the dog and answered with no row, so no
            // verdict came back, and a write is cleared by a verdict.
            if rows.is_empty() {
                return Err(AskFailure::Unknown {
                    reason: format!(
                        "`{dog}` was asked to validate the write and the shepherd answered \
                         with no row for it",
                    ),
                });
            }
            Ok(())
        }
        // No such dog in this flock: nothing to ask, and a dog an operator has
        // not enabled yet is the ordinary case for a first write.
        Err(RequestError::Rpc(err)) if err.code == RpcErrorCode::NotFound => Ok(()),
        Err(err) => Err(AskFailure::Transport(err)),
        Ok(other) => Err(AskFailure::Transport(RequestError::UnexpectedReply {
            asked: "Trigger",
            answered: other.name(),
        })),
    }
}

/// One row of a `Trigger` answer, read as the dog's verdict.
fn refusal_for(dog: &str, outcome: &ActionOutcome) -> Option<AskFailure> {
    match outcome {
        ActionOutcome::Replied { body } => match Verdict::read(body) {
            Verdict::Accept => None,
            Verdict::Refuse { reason } => Some(AskFailure::Refused { reason }),
        },
        ActionOutcome::TimedOut => Some(AskFailure::Refused {
            reason: format!(
                "`{dog}` was asked to validate the write and answered nothing within its \
                 `action_timeout`"
            ),
        }),
        // A dog with no live channel is a dog that is stopped, errored, or was
        // started before it asked for one. There is no process to ask, and a
        // settings write is what makes a dog worth enabling.
        ActionOutcome::NoChannel | ActionOutcome::DogNoChannel | ActionOutcome::Skipped => None,
        // An outcome this build does not know carries no verdict this build can
        // read, and a write is cleared by a verdict.
        _other => Some(AskFailure::Unknown {
            reason: format!(
                "`{dog}` answered the ask with something this shep cannot read: {_other:?}",
            ),
        }),
    }
}

/// The dog a request writes settings for, the table it would leave behind,
/// and the sheep holding that table.
///
/// Read off the request rather than off a caller's own plan, so every client
/// that sends one of the two dog-settings writes is asked the same question.
/// [`None`] for a request that is no dog's settings, or a dog's section whose
/// text will not read as TOML.
pub(crate) fn dog_write(request: &Request) -> Option<DogWrite<'_>> {
    match request {
        Request::SetSheepDogSettings {
            name, dog, table, ..
        } => Some(DogWrite {
            dog,
            sheep: Some(name),
            table: table
                .as_ref()
                .map_or_else(Map::new, |table| table.as_map().clone()),
        }),
        Request::SetDogConfig { name, toml } => Some(DogWrite {
            dog: name,
            sheep: None,
            table: section_table(toml.as_str())?,
        }),
        _other => None,
    }
}

/// What [`dog_write`] found.
pub(crate) struct DogWrite<'a> {
    /// The dog whose settings the request writes.
    pub dog: &'a str,
    /// The sheep holding the table, `None` when it is the dog's own section.
    pub sheep: Option<&'a str>,
    /// The table the request would leave behind.
    pub table: Map<String, Json>,
}

/// A dog's `dogs.toml` section as the table the shepherd parses out of it.
pub(crate) fn section_table(toml: &str) -> Option<Map<String, Json>> {
    Some(table_json(
        toml.parse::<toml_edit::DocumentMut>().ok()?.as_table(),
    ))
}

/// What this machine says about whether `dog` wants to be consulted.
enum Declaration {
    /// Its manifest declares `validate`.
    Declared,
    /// Nothing declares it: `dog` is not adopted here, or its manifest answers
    /// none. A manifest is the only place `validate` can be declared, so a dog
    /// that answers no manifest, in any of the ways a dog answers none, cannot
    /// ask for this.
    Silent,
    /// A record of a manifest is here and was not readable, so whether the dog
    /// wants to be consulted is unknown rather than answered.
    Unknown {
        /// The sentence the operator reads.
        reason: String,
    },
}

/// Whether `dog` declared `validate`: from its cached manifest when this exact
/// adopted binary has been read before, and from the binary otherwise. A
/// built-in dog has no manifest and is never asked.
///
/// A dog that gave no manifest is [`Declaration::Silent`], in every way a dog
/// gives none, and none of them ever declared `validate`. Only a manifest this
/// read could not reach is [`Declaration::Unknown`].
fn declares_validate(dog: &str, paths: &ShepPaths) -> Declaration {
    let doc = match ShepToml::read_only(&paths.daemon_config) {
        Ok(doc) => doc,
        Err(err) => {
            return Declaration::Unknown {
                reason: format!(
                    "shep cannot tell whether `{dog}` wants to be consulted before this write: \
                     {} is unreadable ({err})",
                    paths.daemon_config.display()
                ),
            };
        }
    };
    let Some(path) = doc.adopted_dog_path(dog) else {
        return Declaration::Silent;
    };
    match read_manifest(&path, &paths.home, dog, VERSION_BUDGET) {
        Ok(ManifestRead::Read(manifest)) => {
            if manifest.validate {
                Declaration::Declared
            } else {
                Declaration::Silent
            }
        }
        // A dog that gave no manifest, however it gave none, has no
        // declaration to lose: nothing was ever recorded for this build.
        Ok(ManifestRead::Absent) => Declaration::Silent,
        // A cache file for this exact binary is a manifest some earlier read of
        // it printed, and neither that file nor this probe says what it said.
        // Silence here is not the dog's: the declaration is on this binary, and
        // this read could not reach it.
        Ok(ManifestRead::Unreadable) => Declaration::Unknown {
            reason: format!(
                "shep cannot tell whether `{dog}` wants to be consulted before this write: the \
                 manifest cached for {} cannot be read, and the binary answered nothing when \
                 asked again",
                path.display()
            ),
        },
        Err(err) => Declaration::Unknown {
            reason: format!(
                "shep cannot tell whether `{dog}` wants to be consulted before this write: its \
                 manifest at {} could not be read ({err})",
                path.display()
            ),
        },
    }
}
