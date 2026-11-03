//! Manifest v2: everything a dog says about itself, answered to
//! [`MANIFEST_FLAG`](super::MANIFEST_FLAG) as one JSON object on stdout.
//!
//! Every type ignores a field it does not know when read, so a manifest a
//! later shep's dog wrote still reads on this one. Every type is
//! `#[non_exhaustive]`, so a later version adds fields without breaking a
//! dog built against this one: build one with its `new` and assign the
//! optional fields.

use serde::{Deserialize, Serialize};

/// The `manifest` field's value for the format this module describes.
pub const MANIFEST_VERSION: u32 = 2;

/// What a dog answers [`MANIFEST_FLAG`](super::MANIFEST_FLAG) with.
///
/// `manifest`, `name`, `version` and `protocol` are required to read one:
/// output without them is not a manifest, and the asker falls back to the
/// version and schema flags. Everything else defaults to nothing declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Manifest {
    /// The manifest's own version, [`MANIFEST_VERSION`] for this format.
    ///
    /// Reading refuses 0, which no format has. A higher one is read: later
    /// versions add fields and never change what an earlier field means, so
    /// what this shep knows of them stays true. A v1 manifest declares no
    /// [`events`](Self::events).
    #[serde(deserialize_with = "version_from_one")]
    pub manifest: u32,
    /// The dog's registered name.
    pub name: String,
    /// The dog's build version.
    pub version: String,
    /// The wire protocol the dog speaks, as `--version`'s `shep-protocol`
    /// line states it.
    pub protocol: u32,
    /// Whether the dog asks for the shepherd channel, as `--version`'s
    /// `shep-channel: true` does.
    #[serde(default)]
    pub channel: bool,
    /// The JSON Schema of the dog's settings, byte for byte what the schema
    /// flag answers, `x-shep-sheep` included. `None` for a dog with no
    /// schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<serde_json::Value>,
    /// TOML text for the dog's own section, written when it has none yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<String>,
    /// What the dog asks of shep.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<Permission>,
    /// How `shep <name>` is described in `--help` and completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verb: Option<Verb>,
    /// What the dog needs installed beside it.
    #[serde(default, skip_serializing_if = "Depends::is_empty")]
    pub depends: Depends,
    /// Whether the dog validates a proposed settings table before it is
    /// written, over its channel.
    #[serde(default)]
    pub validate: bool,
    /// Whether the dog runs a sheep's command, as
    /// `<path> launch <sheep> -- <cmd>`.
    #[serde(default)]
    pub launch: bool,
    /// Which sheep the dog's settings apply to. Absent reads as
    /// [`Serves::OptedIn`].
    #[serde(default, skip_serializing_if = "Serves::is_opted_in")]
    pub serves: Serves,
    /// The actions the dog answers when triggered, which `shep trigger <dog>`
    /// lists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    /// The events the dog publishes on the shepherd's bus, under
    /// `dog.<name>.<kind>`. Added in manifest v2; a v1 manifest declares
    /// none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<DogEvent>,
}

impl Manifest {
    /// A manifest of version [`MANIFEST_VERSION`] declaring nothing beyond
    /// who the dog is.
    pub fn new(name: impl Into<String>, version: impl Into<String>, protocol: u32) -> Self {
        Self {
            manifest: MANIFEST_VERSION,
            name: name.into(),
            version: version.into(),
            protocol,
            channel: false,
            settings: None,
            defaults: None,
            permissions: Vec::new(),
            verb: None,
            depends: Depends::default(),
            validate: false,
            launch: false,
            serves: Serves::OptedIn,
            actions: Vec::new(),
            events: Vec::new(),
        }
    }
}

/// One event a dog publishes on the shepherd's bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DogEvent {
    /// The kind, the last segment of the topic `dog.<name>.<kind>`.
    pub kind: String,
    /// One line saying what the event reports.
    pub description: String,
}

impl DogEvent {
    /// An event of `kind`, described by `description`.
    pub fn new(kind: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            description: description.into(),
        }
    }
}

/// One action a dog answers when it is triggered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Action {
    /// The name `shep trigger <dog> <name>` sends.
    pub name: String,
    /// One line saying what the action does.
    pub description: String,
}

impl Action {
    /// An action `name`, described by `description`.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
        }
    }
}

/// Which sheep a dog's settings apply to.
///
/// The dog's `[<dog>]` section is the value for every sheep it serves, and a
/// sheep's own `[app.dogs.<dog>]` table overrides any key that is not
/// dog-wide.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Serves {
    /// Only a sheep carrying an `[app.dogs.<dog>]` table.
    #[default]
    OptedIn,
    /// Every sheep in the flock, one with no table of its own included.
    EverySheep,
}

impl Serves {
    /// Whether this is [`Self::OptedIn`], so a manifest leaves the field out.
    #[must_use]
    pub const fn is_opted_in(&self) -> bool {
        matches!(self, Self::OptedIn)
    }
}

/// Reads the `manifest` field, refusing 0.
fn version_from_one<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    match u32::deserialize(deserializer)? {
        0 => Err(serde::de::Error::custom(
            "manifest versions start at 1, not 0",
        )),
        version => Ok(version),
    }
}

/// One permission a dog asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Permission {
    /// The permission's name, such as `flock.read` or `events:sheep.`.
    pub name: String,
    /// Whether the dog cannot run without it. A required permission this
    /// shep does not know refuses the dog; an optional one is dropped.
    pub required: bool,
    /// Why the dog asks, shown to the operator.
    pub reason: String,
}

impl Permission {
    /// A permission named `name`, `required` or optional, asked for `reason`.
    pub fn new(name: impl Into<String>, required: bool, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            required,
            reason: reason.into(),
        }
    }
}

/// How a dog's verb, `shep <name>`, is described.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Verb {
    /// The line `shep --help` lists the dog with.
    pub description: String,
    /// The dog's subcommands, for completion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subcommands: Vec<Subcommand>,
}

impl Verb {
    /// A verb described by `description`, with no subcommands.
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            subcommands: Vec::new(),
        }
    }
}

/// One subcommand of a dog's verb.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Subcommand {
    /// The word typed after `shep <name>`.
    pub name: String,
    /// The one line completion shows beside it.
    pub about: String,
}

impl Subcommand {
    /// A subcommand `name`, described by `about`.
    pub fn new(name: impl Into<String>, about: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            about: about.into(),
        }
    }
}

/// What a dog needs beside it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Depends {
    /// The other dogs it needs, each with the versions it accepts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dogs: Vec<DogDependency>,
    /// The oldest shep it supports, as a version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shep: Option<String>,
}

impl Depends {
    /// Whether nothing is declared, so a manifest leaves the field out.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dogs.is_empty() && self.shep.is_none()
    }
}

/// A dog another dog needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DogDependency {
    /// The needed dog's name.
    pub name: String,
    /// The versions of it that are accepted, as a version range such as
    /// `>=0.2, <0.4`.
    pub version: String,
}

impl DogDependency {
    /// A dependency on the dog `name`, at the versions `version` accepts.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }
}

#[cfg(test)]
mod tests;
