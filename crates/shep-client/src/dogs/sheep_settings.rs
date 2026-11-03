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
    /// [`Serves::OptedIn`] lists the sheep carrying a table, and
    /// [`Serves::EverySheep`] lists every sheep in the flock, a sheep with no
    /// table of its own getting the section alone. A key `T` marks
    /// `#[shep(dog_wide)]` is left out of every sheep's settings, so give
    /// that field a default and read it with [`Self::config`]. A
    /// `{{secret:...}}` in the section resolves in the dog's environment,
    /// and one in a sheep's table in that sheep's.
    ///
    /// Each sheep's entry is its own `Result`: one bad table never hides the
    /// rest.
    ///
    /// # Errors
    ///
    /// - [`SheepSettingsReadError::Section`]: the section is not a TOML table.
    /// - [`SheepSettingsReadError::Request`]: the shepherd failed to list the
    ///   sheep or their tables.
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
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use serde_json::json;
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use super::*;
    use crate::dogs::{DogIdentity, dog_config};
    use crate::testing::sample_ack;
    use crate::testing::{Handovers, Handshake, control_address, fake_daemon_across_handovers};

    #[dog_config]
    #[derive(Debug, Default, PartialEq, serde::Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields, default)]
    struct Jobs {
        #[shep(dog_wide)]
        poll: String,
        model: String,
        concurrency: u32,
    }

    fn table(value: serde_json::Value) -> DogTable {
        match value {
            serde_json::Value::Object(map) => DogTable::from(map),
            other => panic!("not a table: {other}"),
        }
    }

    fn sheep(id: u32, name: &str) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online).build()
    }

    /// A runtime for the dog `jobs` against a fake shepherd that serves
    /// `section`, a flock of `flock`, and `tables`.
    async fn started(
        dir: &Path,
        section: &str,
        flock: &[&str],
        tables: &[(&str, serde_json::Value)],
    ) -> (DogRuntime, Handovers) {
        let socket = control_address(dir);
        let shepherd = fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
        shepherd.reply_to_dog_config(section);
        shepherd.reply_to_list(
            flock
                .iter()
                .zip(1..)
                .map(|(name, id)| sheep(id, name))
                .collect(),
        );
        shepherd.reply_to_dog_sheep_settings(
            tables
                .iter()
                .map(|(name, value)| ((*name).to_owned(), table(value.clone())))
                .collect(),
        );
        let mut paths = shep_core::paths::ShepPaths::resolve(&|_| None, dir);
        paths.socket = socket;
        let runtime = tokio::time::timeout(
            Duration::from_secs(5),
            DogRuntime::start(DogIdentity::named("jobs"), paths),
        )
        .await
        .expect("DogRuntime::start hung instead of connecting")
        .unwrap();
        (runtime, shepherd)
    }

    async fn read(
        runtime: &DogRuntime,
        serves: Serves,
    ) -> BTreeMap<String, Result<Jobs, ServedSheepError>> {
        tokio::time::timeout(
            Duration::from_secs(5),
            runtime.sheep_settings::<Jobs>(serves),
        )
        .await
        .expect("sheep_settings hung instead of answering")
        .unwrap()
    }

    fn jobs(model: &str, concurrency: u32) -> Jobs {
        Jobs {
            poll: String::new(),
            model: model.to_owned(),
            concurrency,
        }
    }

    #[tokio::test]
    async fn a_sheeps_table_overrides_one_key_and_the_rest_come_from_the_section() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _shepherd) = started(
            dir.path(),
            "model = \"opus\"\nconcurrency = 2\npoll = \"1m\"\n",
            &["web", "api"],
            &[("web", json!({ "model": "sonnet" }))],
        )
        .await;

        let settings = read(&runtime, Serves::OptedIn).await;

        assert_eq!(
            settings.keys().collect::<Vec<_>>(),
            ["web"],
            "a dog served opted-in sheep only hears of the ones with a table"
        );
        assert_eq!(settings["web"], Ok(jobs("sonnet", 2)));
    }

    #[tokio::test]
    async fn a_dog_wide_key_in_a_sheeps_table_is_that_sheeps_error_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _shepherd) = started(
            dir.path(),
            "model = \"opus\"\n",
            &["web", "api", "db"],
            &[
                ("web", json!({ "concurrency": 3 })),
                ("api", json!({ "poll": "5s", "model": "haiku" })),
                ("db", json!({ "model": "sonnet" })),
            ],
        )
        .await;

        let settings = read(&runtime, Serves::OptedIn).await;

        assert_eq!(settings["web"], Ok(jobs("opus", 3)));
        assert_eq!(settings["db"], Ok(jobs("sonnet", 0)));
        let refused = settings["api"].as_ref().unwrap_err();
        assert_eq!(
            refused,
            &ServedSheepError::DogWide {
                sheep: "api".to_owned(),
                keys: vec!["poll".to_owned()],
            }
        );
        assert!(refused.to_string().contains("poll"), "{refused}");
    }

    #[tokio::test]
    async fn a_dog_serving_every_sheep_gets_the_section_for_a_sheep_with_no_table() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _shepherd) = started(
            dir.path(),
            "model = \"opus\"\nconcurrency = 2\npoll = \"1m\"\n",
            &["web", "api"],
            &[("web", json!({ "concurrency": 4 }))],
        )
        .await;

        let settings = read(&runtime, Serves::EverySheep).await;

        assert_eq!(settings.keys().collect::<Vec<_>>(), ["api", "web"]);
        assert_eq!(
            settings["api"],
            Ok(jobs("opus", 2)),
            "the section without `poll`, which is dog-wide"
        );
        assert_eq!(settings["web"], Ok(jobs("opus", 4)));
    }

    #[tokio::test]
    async fn a_dog_in_the_flock_is_not_a_sheep_it_serves() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, shepherd) = started(dir.path(), "", &["web"], &[]).await;
        let mut dog = sheep(2, "bark");
        dog.dog = Some(shep_core::protocol::DogSource::BuiltIn { grants: None });
        shepherd.reply_to_list(vec![sheep(1, "web"), dog]);

        let settings = read(&runtime, Serves::EverySheep).await;

        assert_eq!(settings.keys().collect::<Vec<_>>(), ["web"]);
    }

    #[tokio::test]
    async fn a_dog_with_no_section_and_no_dog_wide_key_gets_the_tables_as_they_are() {
        #[dog_config]
        #[derive(Debug, Default, PartialEq, serde::Deserialize, schemars::JsonSchema)]
        #[serde(deny_unknown_fields, default)]
        struct Plain {
            model: String,
            concurrency: u32,
        }
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _shepherd) = started(
            dir.path(),
            "",
            &["web", "api"],
            &[
                ("web", json!({ "model": "sonnet", "concurrency": 3 })),
                ("api", json!({})),
            ],
        )
        .await;

        let settings = tokio::time::timeout(
            Duration::from_secs(5),
            runtime.sheep_settings::<Plain>(Serves::OptedIn),
        )
        .await
        .expect("sheep_settings hung instead of answering")
        .unwrap();

        assert_eq!(
            settings["web"],
            Ok(Plain {
                model: "sonnet".to_owned(),
                concurrency: 3
            })
        );
        assert_eq!(settings["api"], Ok(Plain::default()));
        assert_eq!(settings.len(), 2);
    }

    #[tokio::test]
    async fn a_table_that_does_not_fit_is_that_sheeps_error_and_quotes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _shepherd) = started(
            dir.path(),
            "",
            &["web", "api"],
            &[
                ("web", json!({ "concurrency": "hunter2" })),
                ("api", json!({ "concurrency": 1 })),
            ],
        )
        .await;

        let settings = read(&runtime, Serves::OptedIn).await;

        let refused = settings["web"].as_ref().unwrap_err();
        assert_eq!(
            refused.to_string(),
            "the settings for web, its [app.dogs.jobs] table over [jobs], do not fit this dog's settings"
        );
        assert!(!format!("{refused:?}").contains("hunter2"));
        assert_eq!(settings["api"], Ok(jobs("", 1)));
    }

    #[tokio::test]
    async fn a_section_that_is_not_toml_fails_the_whole_read() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _shepherd) = started(dir.path(), "model = = \n", &["web"], &[]).await;

        let err = tokio::time::timeout(
            Duration::from_secs(5),
            runtime.sheep_settings::<Jobs>(Serves::EverySheep),
        )
        .await
        .expect("sheep_settings hung instead of answering")
        .unwrap_err();

        assert!(matches!(err, SheepSettingsReadError::Section(_)), "{err}");
    }

    #[test]
    fn a_dog_wide_error_names_every_key_and_no_value() {
        let err = ServedSheepError::DogWide {
            sheep: "api".to_owned(),
            keys: vec!["poll".to_owned(), "region".to_owned()],
        };

        assert_eq!(
            err.to_string(),
            "the table on api sets poll, region, which is dog-wide: set it in the dog's own section"
        );
        assert_eq!(err.sheep(), "api");
    }

    #[tokio::test]
    async fn a_secret_the_shepherd_cannot_resolve_is_that_sheeps_error_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, shepherd) = started(
            dir.path(),
            "",
            &["web", "api"],
            &[
                ("web", json!({ "model": "{{secret:api_key}}" })),
                ("api", json!({ "model": "haiku" })),
            ],
        )
        .await;
        shepherd.refuse_dog_config_resolved(shep_core::protocol::RpcError {
            code: shep_core::protocol::RpcErrorCode::Unsupported,
            message: "this shepherd does not implement the request".to_owned(),
            daemon_version: None,
        });

        let settings = read(&runtime, Serves::OptedIn).await;

        assert!(
            matches!(&settings["web"], Err(ServedSheepError::Unresolved { sheep, .. }) if sheep == "web"),
            "{:?}",
            settings["web"]
        );
        assert_eq!(settings["api"], Ok(jobs("haiku", 0)));
    }
}
