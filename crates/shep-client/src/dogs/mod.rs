//! What every dog shares: answering shep's probes, starting up, talking to
//! its shepherd, and stopping. `docs/dogs.md` has the full contract.
//!
//! [`probe`] answers the flags shep uses to read a dog before adopting it.
//! It is the first line of a dog's `main`. [`Probe`] declares a dog's
//! schema, permissions and verbs. [`Stop`] carries a stop request, and
//! [`DogRuntime`] reaches the shepherd and its config section.
//!
//! A dog that wants `shep trigger` asks for the channel through
//! [`Probe::ask_for_channel`]. It serves that with `shep_channel::serve`.
//! ```no_run
//! use std::process::ExitCode;
//!
//! use shep_client::dogs::{self, DogAction, DogIdentity, DogRuntime, Stop};
//!
//! # #[shep_client::dogs::dog_config]
//! # #[derive(Default, serde::Deserialize)]
//! # #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
//! # struct Settings {}
//! #[tokio::main]
//! async fn main() -> ExitCode {
//!     dogs::probe::<Settings>(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
//!
//!     let args: Vec<String> = std::env::args().skip(1).collect();
//!     match dogs::parse_args("my-dog", args.iter().map(String::as_str)) {
//!         Ok(DogAction::Run) => {}
//!         Ok(DogAction::PrintConfig) => {
//!             println!("#interval = \"1h\"");
//!             return ExitCode::SUCCESS;
//!         }
//!         Err(usage) => {
//!             eprintln!("{usage}");
//!             return usage.exit_code().into();
//!         }
//!     }
//!
//!     let mut stop = Stop::on_stop_signals();
//!     let paths = match dogs::resolve_paths(&|key| std::env::var_os(key)) {
//!         Ok(paths) => paths,
//!         Err(err) => {
//!             eprintln!("my-dog: {err}");
//!             return err.exit_code().into();
//!         }
//!     };
//!     let identity = DogIdentity::from_env(&|key| std::env::var(key).ok(), "my-dog");
//!     let runtime = match DogRuntime::start(identity, paths).await {
//!         Ok(runtime) => runtime,
//!         Err(err) => {
//!             eprintln!("my-dog: {err}");
//!             return err.exit_code().into();
//!         }
//!     };
//!     let _settings: Settings = match runtime.config() {
//!         Ok(settings) => settings,
//!         Err(err) => {
//!             eprintln!("my-dog: {err}");
//!             return err.exit_code().into();
//!         }
//!     };
//!
//!     // The dog's own loop, watching `stop` beside its work.
//!     stop.wait().await;
//!     ExitCode::SUCCESS
//! }
//! ```

use core::fmt;

mod error;
mod identity;
mod link;
mod probes;
mod runtime;
mod section;
mod startup;
mod stop;
mod validate;

pub use error::ShepherdError;
pub use identity::DogIdentity;
pub use probes::{Probe, probe, probe_with_sheep};
pub use runtime::DogRuntime;
pub use section::{SectionError, parse_section};
use serde::de::DeserializeOwned;
use shep_core::config::DogTable;
pub use shep_core::dogs::manifest::{DogDependency, Manifest, Permission, Subcommand, Verb};
pub use shep_core::dogs::validate::{Proposed, VALIDATE_ACTION, Verdict};
pub use shep_core::dogs::{SECRET_KEY, SHEEP_SCHEMA_KEY};
/// The attribute that implements [`DogConfig`], re-exported so a dog takes
/// one dependency rather than two.
///
/// Its own documentation carries the rules: which shapes accept
/// `#[shep(secret)]`, which refuse it, and what the expansion looks like.
pub use shep_macros::dog_config;
pub use startup::{DogAction, HomeError, PRINT_CONFIG_FLAG, UsageError, parse_args, resolve_paths};
pub use stop::{Interrupted, Stop, StopRequest};
pub use validate::validate_answer;

/// That a type's config schema has been through [`dog_config`], so every
/// field marked `#[shep(secret)]` carries [`SECRET_KEY`] wherever `schemars`
/// puts that field.
///
/// The bound on [`probe`]: a dog cannot answer shep's schema flag with a type
/// nothing marked. Apply the attribute rather than writing the impl by hand,
/// which claims the marking without doing it.
pub trait DogConfig {}

/// The JSON Schema a dog answers the schema flag with: what `schemars`
/// generates for `T`. Every `#[shep(secret)]` field carries [`SECRET_KEY`]
/// already, because [`dog_config`] put the extension on the field itself.
/// Public so a dog or test can read the marks without spawning itself.
#[cfg(feature = "schema")]
pub fn config_schema<T: DogConfig + schemars::JsonSchema>() -> schemars::Schema {
    schemars::SchemaGenerator::default().into_root_schema_for::<T>()
}

/// [`config_schema`], plus [`SHEEP_SCHEMA_KEY`] holding `S`'s schema: what a
/// dog that acts per sheep answers the schema flag with.
///
/// One [`schemars::SchemaGenerator`] produces both halves, so `S`'s
/// definitions land in `T`'s own `$defs` and the key's `$ref` resolves the
/// way every other one in the document does. Every `#[shep(secret)]` field
/// of `S` carries [`SECRET_KEY`] exactly as one of `T`'s does.
#[cfg(feature = "schema")]
pub fn config_schema_with_sheep<T, S>() -> schemars::Schema
where
    T: DogConfig + schemars::JsonSchema,
    S: DogConfig + schemars::JsonSchema,
{
    let mut generator = schemars::SchemaGenerator::default();
    // Registers `S` in the generator's definitions before `T` is consumed,
    // so `into_root_schema_for` carries both into the same `$defs`.
    let sheep = generator.subschema_for::<S>();
    let mut schema = generator.into_root_schema_for::<T>();
    schema.insert(SHEEP_SCHEMA_KEY.to_owned(), sheep.to_value());
    schema
}

/// One sheep's `[app.dogs.<dog>]` table did not fit `dog`'s own settings
/// type.
///
/// Carries no parser message and no table value: a field of the wrong
/// type, or an unknown key under `deny_unknown_fields`, can hold a
/// credential, and quoting either would print exactly what [`DogTable`]'s
/// own `Debug` refuses to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheepSettingsError {
    dog: String,
    sheep: String,
}

impl fmt::Display for SheepSettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the [app.dogs.{}] table on {} does not fit this dog's settings",
            self.dog, self.sheep
        )
    }
}

impl core::error::Error for SheepSettingsError {}

/// Parses one sheep's `[app.dogs.<dog>]` table into `dog`'s own settings
/// type.
///
/// Call this once per sheep in a `Request::DogSheepSettings` answer's map,
/// so a table that does not fit `S` never hides the rest of the sheep.
/// `dog` and `sheep` name the table in the refusal; neither is read from
/// `table` itself.
///
/// # Errors
///
/// [`SheepSettingsError`] when `table` does not fit `S`. It never carries
/// the parser's own message, which can quote a value the table held.
///
/// # Examples
///
/// ```no_run
/// use shep_client::Client;
/// use shep_client::dogs::parse_sheep_settings;
/// use shep_client::shep_core::protocol::{Request, Response};
///
/// #[derive(serde::Deserialize, Default)]
/// struct JobsSettings {
///     #[serde(default)]
///     concurrency: u32,
/// }
///
/// # async fn read(client: &Client) -> Result<(), Box<dyn core::error::Error>> {
/// let Response::DogSheepSettings { tables } = client
///     .request(Request::DogSheepSettings {
///         dog: "jobs".to_string(),
///     })
///     .await?
/// else {
///     return Err("the shepherd answered with something else".into());
/// };
/// for (sheep, table) in &tables {
///     let settings: JobsSettings = parse_sheep_settings("jobs", sheep, table)?;
///     println!("{sheep}: concurrency {}", settings.concurrency);
/// }
/// # Ok(())
/// # }
/// # let _ = read;
/// ```
pub fn parse_sheep_settings<S: DeserializeOwned>(
    dog: &str,
    sheep: &str,
    table: &DogTable,
) -> Result<S, SheepSettingsError> {
    serde_json::from_value(serde_json::Value::Object(table.as_map().clone())).map_err(|_| {
        SheepSettingsError {
            dog: dog.to_string(),
            sheep: sheep.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, PartialEq, serde::Deserialize)]
    #[serde(deny_unknown_fields, default)]
    struct JobsSettings {
        concurrency: u32,
    }

    fn table(pairs: impl IntoIterator<Item = (&'static str, serde_json::Value)>) -> DogTable {
        DogTable::from(
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect::<serde_json::Map<String, serde_json::Value>>(),
        )
    }

    #[test]
    fn a_table_that_fits_parses() {
        let table = table([("concurrency", serde_json::json!(2))]);
        let settings: JobsSettings = parse_sheep_settings("jobs", "web", &table).unwrap();
        assert_eq!(settings, JobsSettings { concurrency: 2 });
    }

    /// A credential written into a field whose type refuses it: the
    /// refusal names the dog and the sheep and quotes neither.
    #[test]
    fn a_wrongly_typed_field_is_refused_and_never_quotes_the_value() {
        let table = table([("concurrency", serde_json::json!("hunter2"))]);
        let err = parse_sheep_settings::<JobsSettings>("jobs", "web", &table).unwrap_err();

        assert_eq!(
            err.to_string(),
            "the [app.dogs.jobs] table on web does not fit this dog's settings"
        );
        assert!(!err.to_string().contains("hunter2"));
        assert!(!format!("{err:?}").contains("hunter2"));
    }

    /// A credential under a key `JobsSettings` never declared: refused by
    /// `deny_unknown_fields`, quoting nothing.
    #[test]
    fn an_unknown_key_is_refused_and_never_quotes_the_value() {
        let table = table([
            ("concurrency", serde_json::json!(2)),
            ("token", serde_json::json!("hunter2")),
        ]);
        let err = parse_sheep_settings::<JobsSettings>("jobs", "web", &table).unwrap_err();

        assert_eq!(
            err.to_string(),
            "the [app.dogs.jobs] table on web does not fit this dog's settings"
        );
        assert!(!err.to_string().contains("hunter2"));
        assert!(!format!("{err:?}").contains("hunter2"));
    }

    /// Everything the `schema` feature gates, gated the same way: with the
    /// feature off there is no `config_schema` to name here either.
    #[cfg(feature = "schema")]
    mod schema {
        use super::*;

        #[dog_config]
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
        struct Webhook {
            #[shep(secret)]
            url: String,
            channel: String,
        }

        #[dog_config]
        #[derive(schemars::JsonSchema)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
        enum Sink {
            Discord {
                #[shep(secret)]
                url: String,
                quiet: bool,
            },
            Slack {
                #[shep(secret)]
                url: String,
            },
        }

        #[dog_config]
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
        struct Renamed {
            #[shep(secret)]
            #[serde(rename = "webhook_url")]
            url: String,
        }

        #[dog_config]
        #[derive(schemars::JsonSchema)]
        #[serde(tag = "kind", rename_all_fields = "SCREAMING-KEBAB-CASE")]
        #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
        enum RenamedFields {
            One {
                #[shep(secret)]
                api_token: String,
            },
        }

        /// A type the root only mentions, so `schemars` hoists it into `$defs`.
        /// Its `token` is an ordinary string, and it is named to collide with
        /// the credential [`Outer`] marks.
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
        struct Inner {
            token: String,
        }

        #[dog_config]
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
        struct Outer {
            #[shep(secret)]
            token: String,
            inner: Inner,
        }

        /// A nested type that marks a credential of its own, reached by
        /// [`Host`] through a map, which is bark's exact shape.
        #[dog_config]
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
        struct NestedSink {
            #[shep(secret)]
            url: String,
            quiet: bool,
        }

        #[dog_config]
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
        struct Host {
            sinks: std::collections::BTreeMap<String, NestedSink>,
        }

        /// Both halves in one test on purpose: an implementation that marked
        /// every property would pass a test that only checked the marked one.
        #[test]
        fn a_secret_field_carries_the_marker_and_a_plain_one_does_not() {
            let schema = config_schema::<Webhook>();
            let props = schema
                .as_value()
                .get("properties")
                .expect("a derived struct schema has properties");

            assert_eq!(
                props.get("url").and_then(|url| url.get(SECRET_KEY)),
                Some(&serde_json::Value::Bool(true)),
                "the marked field carries the marker"
            );
            assert_eq!(
                props.get("channel").and_then(|it| it.get(SECRET_KEY)),
                None,
                "the unmarked field carries nothing"
            );
        }

        /// The key the attribute writes is a literal, since `schemars` accepts
        /// only a literal there. This is what fails if it drifts from the
        /// constant shep itself reads marks with.
        #[test]
        fn the_extension_key_is_the_one_shep_core_publishes() {
            let schema = config_schema::<Webhook>();
            assert_eq!(
                schema
                    .as_value()
                    .pointer(&format!("/properties/url/{SECRET_KEY}")),
                Some(&serde_json::Value::Bool(true)),
                "`shep-macros` writes `{SECRET_KEY}` verbatim and has no way to \
                 name this constant"
            );
        }

        /// A tagged enum has no top-level `properties` at all: it is a `oneOf`
        /// of one object per variant, and the mark has to be in each.
        #[test]
        fn a_marker_reaches_every_variant_of_a_tagged_enum_and_no_plain_field() {
            let schema = config_schema::<Sink>();
            let variants = schema
                .as_value()
                .get("oneOf")
                .and_then(|it| it.as_array())
                .expect("a tagged enum is a oneOf");
            assert_eq!(variants.len(), 2);

            for variant in variants {
                let props = variant
                    .get("properties")
                    .expect("each variant carries its own properties");
                assert_eq!(
                    props.get("url").and_then(|url| url.get(SECRET_KEY)),
                    Some(&serde_json::Value::Bool(true)),
                    "every marked occurrence is marked"
                );
                assert_eq!(
                    props.get("quiet").and_then(|it| it.get(SECRET_KEY)),
                    None,
                    "a plain field in the same variant carries nothing"
                );
            }
        }

        /// The marker rides the field, so a rename carries it along instead of
        /// leaving it behind on a property name nothing has. Both spellings of
        /// the rename, since a per-field one and a whole-type one reach the
        /// property by different paths inside `schemars`.
        #[test]
        fn a_rename_moves_the_marker_onto_the_renamed_property() {
            let renamed = config_schema::<Renamed>();
            let renamed = renamed.as_value();
            assert_eq!(
                renamed.pointer("/properties/webhook_url/x-shep-secret"),
                Some(&serde_json::Value::Bool(true)),
                "`#[serde(rename)]` renames the property the mark is on"
            );
            assert_eq!(
                renamed.pointer("/properties/url"),
                None,
                "nothing is left under the Rust identifier"
            );

            let fields = config_schema::<RenamedFields>();
            assert_eq!(
                fields
                    .as_value()
                    .pointer("/oneOf/0/properties/API-TOKEN/x-shep-secret"),
                Some(&serde_json::Value::Bool(true)),
                "`rename_all_fields` does the same to a variant's field"
            );
        }

        /// The marked field of a nested type reaches the schema of a config
        /// that merely holds it, at whatever depth `schemars` puts it. This is
        /// shep#280: the mark used to be dropped here, in silence, and bark
        /// worked around it by marking its whole sinks map.
        #[test]
        fn a_nested_types_marked_field_is_marked_in_the_hosts_schema() {
            let schema = config_schema::<Host>();
            let schema = schema.as_value();

            assert_eq!(
                schema.pointer("/$defs/NestedSink/properties/url/x-shep-secret"),
                Some(&serde_json::Value::Bool(true)),
                "the nested credential carries the marker through the map"
            );
            assert_eq!(
                schema.pointer("/$defs/NestedSink/properties/quiet/x-shep-secret"),
                None,
                "its plain neighbour carries nothing"
            );
        }

        /// A mark belongs to the field that carries it, so a like-named
        /// property of another type is not marked on its behalf.
        /// `Rule::sinks` is the live case: it lists sink NAMES, one level
        /// under a `BarkConfig::sinks` that really does hold credentials.
        #[test]
        fn a_like_named_property_of_a_nested_type_is_left_plain() {
            let schema = config_schema::<Outer>();
            let schema = schema.as_value();

            assert_eq!(
                schema.pointer("/properties/token/x-shep-secret"),
                Some(&serde_json::Value::Bool(true)),
                "the root's own marked field carries the marker"
            );
            assert_eq!(
                schema.pointer("/$defs/Inner/properties/token/x-shep-secret"),
                None,
                "a stranger that shares the name is not the marked field"
            );
        }

        #[dog_config]
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
        struct JobsConfig {
            channel: String,
        }

        /// A dog's per-sheep `[app.dogs.jobs]` table: a plain field, a
        /// credential, and a nested table two levels deep.
        #[dog_config]
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
        struct JobsSheepSettings {
            concurrency: u32,
            #[shep(secret)]
            api_key: String,
            hours: WorkingHours,
        }

        #[dog_config]
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
        struct WorkingHours {
            start: String,
        }

        /// The `$ref`'s own definition name, read off the sheep key without
        /// assuming it is the type's Rust name: `schemars` is free to
        /// rename it.
        fn sheep_def_name(schema: &serde_json::Value) -> &str {
            schema
                .pointer(&format!("/{SHEEP_SCHEMA_KEY}/$ref"))
                .and_then(serde_json::Value::as_str)
                .and_then(|r| r.strip_prefix("#/$defs/"))
                .expect("the sheep key holds a $ref into $defs")
        }

        #[test]
        fn the_combined_schema_carries_a_sheep_ref_that_resolves_in_root_defs() {
            let schema = config_schema_with_sheep::<JobsConfig, JobsSheepSettings>();
            let schema = schema.as_value();

            let def_name = sheep_def_name(schema);
            assert!(
                schema.pointer(&format!("/$defs/{def_name}")).is_some(),
                "the sheep key's $ref names a definition in the root's own $defs"
            );
        }

        #[test]
        fn a_secret_field_of_the_sheep_type_is_marked_in_the_resolved_schema() {
            let schema = config_schema_with_sheep::<JobsConfig, JobsSheepSettings>();
            let schema = schema.as_value();
            let def_name = sheep_def_name(schema);

            assert_eq!(
                schema.pointer(&format!(
                    "/$defs/{def_name}/properties/api_key/{SECRET_KEY}"
                )),
                Some(&serde_json::Value::Bool(true)),
                "the sheep type's marked field carries the marker, exactly as a root field does"
            );
            assert_eq!(
                schema.pointer(&format!(
                    "/$defs/{def_name}/properties/concurrency/{SECRET_KEY}"
                )),
                None,
                "its plain neighbour carries nothing"
            );
        }

        #[test]
        fn a_struct_nested_inside_the_sheep_type_resolves_in_root_defs_too() {
            let schema = config_schema_with_sheep::<JobsConfig, JobsSheepSettings>();
            let schema = schema.as_value();
            let def_name = sheep_def_name(schema);
            let hours = schema
                .pointer(&format!("/$defs/{def_name}/properties/hours/$ref"))
                .and_then(serde_json::Value::as_str)
                .and_then(|r| r.strip_prefix("#/$defs/"))
                .expect("the nested field holds a $ref into $defs");

            assert!(
                schema
                    .pointer(&format!("/$defs/{hours}/properties/start"))
                    .is_some(),
                "a type nested inside the sheep type is hoisted into the same $defs, \
                 so lookout can flatten it"
            );
        }

        #[test]
        fn the_plain_schema_carries_no_sheep_key() {
            let with_sheep = config_schema_with_sheep::<JobsConfig, JobsSheepSettings>();
            let plain = config_schema::<JobsConfig>();

            assert!(with_sheep.as_value().get(SHEEP_SCHEMA_KEY).is_some());
            assert_eq!(
                plain.as_value().get(SHEEP_SCHEMA_KEY),
                None,
                "config_schema stays exactly what it was: no sheep key"
            );
        }
    }
}
