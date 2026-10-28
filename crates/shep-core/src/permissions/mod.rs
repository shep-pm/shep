//! What a dog may ask of the shepherd: [`Permission`] and the [`Grants`]
//! an operator gave it. `need()` is the table saying which permission
//! each [`Request`] takes.
//!
//! Grants keep an honest dog in bounds and make its requests attributable.
//! They do not confine it. A dog runs as the operator and could open the
//! control socket like the CLI.
//!
//! Names are stable: never renamed or split. A request joins a group only
//! when it is no more powerful than what the group already holds.

use core::fmt;
use core::str::FromStr;
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::secrets::is_name;

mod need;

pub use need::{Need, need};

#[cfg(test)]
mod need_tests;
#[cfg(test)]
mod tests;

// Named by an intra-doc link and by nothing rustc compiles.
#[cfg(doc)]
use crate::protocol::Request;

/// The first protocol version whose shepherd refuses a dog's request outside
/// its grants. An older shepherd records grants and ignores them.
pub const ENFORCED_SINCE: u32 = 12;

/// One permission, as a request needs it and a grant names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Permission {
    /// `flock.read`: look at the flock and its configuration.
    FlockRead,
    /// `flock.control`: start, stop, restart and signal sheep.
    FlockControl,
    /// `flock.edit`: add and delete sheep and change their configuration.
    FlockEdit,
    /// `logs.read`: read the end of a sheep's log.
    LogsRead,
    /// `logs.manage`: reopen and flush logs.
    LogsManage,
    /// `events:<prefix>`: subscribe to topics that begin with the prefix.
    Events(String),
    /// `secrets.read:<scope>`: read a table with its secrets resolved, in the
    /// environment named by the scope.
    SecretsRead(String),
    /// `secrets.write:<namespace>`: push values into a namespace.
    SecretsWrite(String),
    /// `dogs.control`: enable, disable and delete dogs, and configure other
    /// dogs.
    DogsControl,
    /// `dogs.trigger:<name>`: send an action to the named dog.
    DogsTrigger(String),
    /// `shepherd.control`: stop the shepherd.
    ShepherdControl,
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FlockRead => f.write_str("flock.read"),
            Self::FlockControl => f.write_str("flock.control"),
            Self::FlockEdit => f.write_str("flock.edit"),
            Self::LogsRead => f.write_str("logs.read"),
            Self::LogsManage => f.write_str("logs.manage"),
            Self::Events(prefix) => write!(f, "events:{prefix}"),
            Self::SecretsRead(scope) => write!(f, "secrets.read:{scope}"),
            Self::SecretsWrite(namespace) => write!(f, "secrets.write:{namespace}"),
            Self::DogsControl => f.write_str("dogs.control"),
            Self::DogsTrigger(name) => write!(f, "dogs.trigger:{name}"),
            Self::ShepherdControl => f.write_str("shepherd.control"),
        }
    }
}

/// A permission name this build does not know, or knows with a scope it
/// cannot read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPermission(String);

impl UnknownPermission {
    /// The name as it was written.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UnknownPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` is not a permission shep knows", self.0)
    }
}

impl core::error::Error for UnknownPermission {}

impl FromStr for Permission {
    type Err = UnknownPermission;

    /// Reads a permission name. A scoped name (`secrets.read:staging`,
    /// `secrets.write:vercel`, `dogs.trigger:jobs`) takes a scope in the
    /// secrets-name grammar; `events:` takes any prefix, the empty one
    /// included.
    ///
    /// # Errors
    /// [`UnknownPermission`]: a name outside the list, a scoped name with no
    /// scope or a bad one, or a scope on a name that takes none.
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        let unknown = || UnknownPermission(name.to_string());
        let (head, scope) = match name.split_once(':') {
            Some((head, scope)) => (head, Some(scope)),
            None => (name, None),
        };
        let scoped = |make: fn(String) -> Self| match scope {
            Some(scope) if is_name(scope) => Ok(make(scope.to_string())),
            _ => Err(unknown()),
        };
        let bare = |permission: Self| match scope {
            None => Ok(permission),
            Some(_) => Err(unknown()),
        };
        match head {
            "flock.read" => bare(Self::FlockRead),
            "flock.control" => bare(Self::FlockControl),
            "flock.edit" => bare(Self::FlockEdit),
            "logs.read" => bare(Self::LogsRead),
            "logs.manage" => bare(Self::LogsManage),
            "dogs.control" => bare(Self::DogsControl),
            "shepherd.control" => bare(Self::ShepherdControl),
            "events" => scope
                .map(|prefix| Self::Events(prefix.to_string()))
                .ok_or_else(unknown),
            "secrets.read" => scoped(Self::SecretsRead),
            "secrets.write" => scoped(Self::SecretsWrite),
            "dogs.trigger" => scoped(Self::DogsTrigger),
            _ => Err(unknown()),
        }
    }
}

/// The permissions an operator granted one dog.
///
/// Held as the names the operator wrote. A name from a newer shep reads
/// and matches nothing here. [`Self::parse`] is the strict door for a
/// name typed by hand.
///
/// A dog with no `Grants` keeps full access; an empty one grants nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Grants(BTreeSet<String>);

impl Grants {
    /// Grants made of typed permissions.
    #[must_use]
    pub fn new(permissions: impl IntoIterator<Item = Permission>) -> Self {
        Self(permissions.into_iter().map(|p| p.to_string()).collect())
    }

    /// Grants read from names an operator wrote, refusing any this build
    /// cannot read, so a typo is caught where it is made.
    ///
    /// # Errors
    /// [`UnknownPermission`] for the first name that is not a [`Permission`].
    pub fn parse<I, S>(names: I) -> Result<Self, UnknownPermission>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        names
            .into_iter()
            .map(|name| {
                let name = name.as_ref();
                name.parse::<Permission>().map(|_| name.to_string())
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }

    /// Grants made of names as they are, unchecked. What a peer's row
    /// carries, where an unknown name must not cost the whole row.
    #[must_use]
    pub fn from_names(names: impl IntoIterator<Item = String>) -> Self {
        Self(names.into_iter().collect())
    }

    /// The granted names, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }

    /// Whether `permission` was granted, exactly as named.
    #[must_use]
    pub fn allows(&self, permission: &Permission) -> bool {
        self.0.contains(&permission.to_string())
    }

    /// Whether a subscription to the topic glob `pattern` stays inside a
    /// granted `events:` prefix.
    ///
    /// Judged on the pattern's literal head, the part before its first glob
    /// character: `process.*` can only reach topics beginning `process.`, so
    /// a grant of `process.` covers it and a grant of `process.exit` does not.
    #[must_use]
    pub fn allows_topic(&self, pattern: &str) -> bool {
        let head = pattern
            .find(['*', '?', '[', '{', '\\'])
            .map_or(pattern, |at| &pattern[..at]);
        self.0
            .iter()
            .filter_map(|name| name.strip_prefix("events:"))
            .any(|prefix| head.starts_with(prefix))
    }
}
