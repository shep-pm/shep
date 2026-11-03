//! A dog's one settings type, set for every sheep in `[<dog>]` and per sheep
//! in `[app.dogs.<dog>]`.
//!
//! [`DogRuntime::sheep_settings`] does the layering, so a dog author writes
//! one type and calls one function with nothing to merge by hand.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use serde::de::DeserializeOwned;
use shep_core::config::DogTable;
use shep_core::dog_defaults::layered;
use shep_core::dogs::manifest::Serves;
use shep_core::protocol::{Request, Response};

use super::{DogConfig, DogRuntime, SectionError};
use crate::RequestError;

/// One sheep's settings could not be built.
///
/// Carries no parser message and no table value, which can hold a credential.
/// A key name is a field of the dog's own type, so it is safe to print.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServedSheepError {
    /// The sheep's table sets a key the dog marks `#[shep(dog_wide)]`, which
    /// is set in `[<dog>]` only.
    DogWide {
        /// The sheep.
        sheep: String,
        /// Each dog-wide key the sheep's table sets, in order.
        keys: Vec<String>,
    },
    /// The section with the sheep's table over it does not fit the dog's
    /// settings type.
    DoesNotFit {
        /// The dog.
        dog: String,
        /// The sheep.
        sheep: String,
    },
    /// The shepherd could not resolve a `{{secret:...}}` in the sheep's
    /// table.
    Unresolved {
        /// The sheep.
        sheep: String,
        /// The shepherd's reason.
        reason: String,
    },
}

impl ServedSheepError {
    /// The sheep whose settings this is about.
    #[must_use]
    pub fn sheep(&self) -> &str {
        match self {
            Self::DogWide { sheep, .. }
            | Self::DoesNotFit { sheep, .. }
            | Self::Unresolved { sheep, .. } => sheep,
        }
    }
}

impl fmt::Display for ServedSheepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DogWide { sheep, keys } => write!(
                f,
                "the table on {sheep} sets {}, which is dog-wide: set it in the dog's own section",
                keys.join(", ")
            ),
            Self::DoesNotFit { dog, sheep } => write!(
                f,
                "the settings for {sheep}, its [app.dogs.{dog}] table over [{dog}], do not fit this dog's settings"
            ),
            Self::Unresolved { sheep, reason } => {
                write!(
                    f,
                    "a secret in the table on {sheep} did not resolve: {reason}"
                )
            }
        }
    }
}

impl core::error::Error for ServedSheepError {}

/// The settings of no sheep could be read.
#[derive(Debug)]
#[non_exhaustive]
pub enum SheepSettingsReadError {
    /// The dog's own section is not a TOML table.
    Section(SectionError),
    /// The shepherd could not list the sheep or their tables.
    Request(RequestError),
}

impl fmt::Display for SheepSettingsReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Section(err) => write!(f, "{err}"),
            Self::Request(err) => write!(f, "a request to the shepherd failed: {err}"),
        }
    }
}

impl core::error::Error for SheepSettingsReadError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Section(err) => Some(err),
            Self::Request(err) => Some(err),
        }
    }
}

impl From<SectionError> for SheepSettingsReadError {
    fn from(err: SectionError) -> Self {
        Self::Section(err)
    }
}

impl From<RequestError> for SheepSettingsReadError {
    fn from(err: RequestError) -> Self {
        Self::Request(err)
    }
}

/// `section` without the keys in `dog_wide`, with the sheep's `own` table
/// over it, read as `T`.
fn settings_for<T: DeserializeOwned>(
    dog: &str,
    dog_wide: &BTreeSet<String>,
    section: &DogTable,
    sheep: &str,
    own: Option<&DogTable>,
) -> Result<T, ServedSheepError> {
    let set_here: Vec<String> = own
        .into_iter()
        .flat_map(|own| own.as_map().keys())
        .filter(|key| dog_wide.contains(*key))
        .cloned()
        .collect();
    if !set_here.is_empty() {
        return Err(ServedSheepError::DogWide {
            sheep: sheep.to_owned(),
            keys: set_here,
        });
    }
    let mut shared = section.as_map().clone();
    shared.retain(|key, _| !dog_wide.contains(key));
    let shared = DogTable::from(shared);
    let merged = match own {
        Some(own) => layered(&[&shared, own]),
        None => shared,
    };
    serde_json::from_value(serde_json::Value::Object(merged.into_map())).map_err(|_| {
        ServedSheepError::DoesNotFit {
            dog: dog.to_owned(),
            sheep: sheep.to_owned(),
        }
    })
}

impl DogRuntime {
    /// Each served sheep's settings, read as `T`: this dog's `[<dog>]`
    /// section without its dog-wide keys, then the sheep's own
    /// `[app.dogs.<dog>]` table over it.
    ///
    /// `serves` is what the dog declared with [`Probe::serves`](super::Probe::serves).
    /// A `#[shep(dog_wide)]` field needs a default; read it with
    /// [`Self::config`].
    ///
    /// # Errors
    ///
    /// - [`SheepSettingsReadError::Section`]: the section is not a TOML table.
    /// - [`SheepSettingsReadError::Request`]: the sheep or tables failed to list.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use shep_client::dogs::{DogRuntime, Serves};
    ///
    /// #[shep_client::dogs::dog_config]
    /// #[derive(Default, serde::Deserialize, schemars::JsonSchema)]
    /// #[serde(default)]
    /// struct Jobs {
    ///     #[shep(dog_wide)]
    ///     poll: String,
    ///     model: String,
    /// }
    ///
    /// # async fn read(runtime: &DogRuntime) -> Result<(), Box<dyn core::error::Error>> {
    /// for (sheep, settings) in runtime.sheep_settings::<Jobs>(Serves::EverySheep).await? {
    ///     match settings {
    ///         Ok(settings) => println!("{sheep}: {}", settings.model),
    ///         Err(err) => eprintln!("{sheep}: {err}"),
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// # let _ = read;
    /// ```
    pub async fn sheep_settings<T>(
        &self,
        serves: Serves,
    ) -> Result<BTreeMap<String, Result<T, ServedSheepError>>, SheepSettingsReadError>
    where
        T: DogConfig + schemars::JsonSchema + DeserializeOwned,
    {
        let dog = self.identity().section();
        let dog_wide = super::dog_wide::dog_wide_keys::<T>();
        let section = super::section::section_table(dog, self.section().as_str())?;
        let tables = self.tables_of_every_sheep().await?;
        let sheep = match serves {
            Serves::EverySheep => self
                .client()
                .list_flock()
                .await?
                .into_iter()
                .filter(|entry| entry.dog.is_none())
                .map(|entry| entry.name)
                .collect(),
            _ => tables.keys().cloned().collect::<BTreeSet<_>>(),
        };
        let mut settings = BTreeMap::new();
        for name in sheep {
            let own = match tables.get(&name) {
                Some(table) if !table.references().is_empty() => {
                    match self.client().sheep_dog_table(&name, dog).await {
                        Ok(resolved) => resolved,
                        Err(err) => {
                            let fault = ServedSheepError::Unresolved {
                                sheep: name.clone(),
                                reason: err.to_string(),
                            };
                            settings.insert(name, Err(fault));
                            continue;
                        }
                    }
                }
                other => other.cloned(),
            };
            let one = settings_for(dog, &dog_wide, &section, &name, own.as_ref());
            settings.insert(name, one);
        }
        Ok(settings)
    }

    /// Every sheep's `[app.dogs.<dog>]` table, references unresolved.
    async fn tables_of_every_sheep(&self) -> Result<BTreeMap<String, DogTable>, RequestError> {
        let asked = Request::DogSheepSettings {
            dog: self.identity().section().to_owned(),
        };
        match self.client().request(asked).await? {
            Response::DogSheepSettings { tables } => Ok(tables),
            other => Err(RequestError::UnexpectedReply {
                asked: "DogSheepSettings",
                answered: other.name(),
            }),
        }
    }
}

#[cfg(test)]
mod tests;
