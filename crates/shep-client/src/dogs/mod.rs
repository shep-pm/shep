//! The dog side of the probe contract: one call a dog makes as its first
//! line, which answers every question shep asks its binary.
//!
//! A dog is a plugin process the shepherd supervises. Before shep adopts one,
//! and again whenever it needs the dog's config schema, it spawns the binary
//! with a flag and reads what comes back. [`probe`] answers both flags, so
//! neither the answer's format nor the flag names are ever typed by a dog
//! author:
//!
//! ```no_run
//! # #[shep_client::dogs::dog_config]
//! # #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
//! # struct MyDogConfig {}
//! fn main() {
//!     shep_client::dogs::probe::<MyDogConfig>(
//!         env!("CARGO_PKG_NAME"),
//!         env!("CARGO_PKG_VERSION"),
//!     );
//!     // ...normal startup, reached only when this run is not a probe.
//! }
//! ```
//!
//! `name` and `version` are arguments rather than `env!` calls in this
//! crate, since `env!` expands where it is written and would report
//! `shep-client`'s own version instead of the dog's.
//!
//! Answering is optional: with the `schema` feature off, [`probe`] still
//! answers the version flag, and the schema flag exits without printing,
//! which shep reads as a dog with no schema and refuses nothing for.

use std::io::Write as _;

mod identity;
mod section;
mod stop;

pub use identity::DogIdentity;
pub use section::{SectionError, parse_section};
pub use shep_core::dogs::SECRET_KEY;
use shep_core::dogs::{SCHEMA_FLAG, SHEP_PROTOCOL_KEY, VERSION_FLAG};
/// The attribute that implements [`DogConfig`], re-exported so a dog takes
/// one dependency rather than two.
///
/// Its own documentation carries the rules: which shapes accept
/// `#[shep(secret)]`, which refuse it, and what the expansion looks like.
pub use shep_macros::dog_config;
pub use stop::{Interrupted, Stop, StopRequest};

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

/// Answers shep's probes, and returns when this run is not a probe, so a
/// dog calls it as the first line of `main`.
///
/// `name` and `version` are ordinarily `env!("CARGO_PKG_NAME")` and
/// `env!("CARGO_PKG_VERSION")`; only the version is read by shep.
///
/// # Exits
///
/// Ends the process with [`process::exit`](std::process::exit), status 0,
/// before `main` opens anything.
#[cfg(feature = "schema")]
pub fn probe<T: DogConfig + schemars::JsonSchema>(name: &str, version: &str) {
    match first_argument().as_deref() {
        Some(VERSION_FLAG) => answer(&version_answer(name, version)),
        Some(SCHEMA_FLAG) => answer(&schema_answer::<T>()),
        _ => (),
    }
}

/// Answers shep's probes, and returns when this run is not a probe, so a
/// dog calls it as the first line of `main`.
///
/// `name` and `version` are ordinarily `env!("CARGO_PKG_NAME")` and
/// `env!("CARGO_PKG_VERSION")`; only the version is read by shep. With the
/// `schema` feature off, the schema flag exits without printing, so shep
/// records a dog with no schema instead of waiting out a timeout.
///
/// # Exits
///
/// Ends the process with [`process::exit`](std::process::exit), status 0,
/// before `main` opens anything.
#[cfg(not(feature = "schema"))]
pub fn probe<T: DogConfig>(name: &str, version: &str) {
    match first_argument().as_deref() {
        Some(VERSION_FLAG) => answer(&version_answer(name, version)),
        Some(SCHEMA_FLAG) => std::process::exit(0),
        _ => (),
    }
}

/// The argument shep spawns a probe with, which is the only one it passes.
///
/// Only the first: `docs/dogs.md` publishes that contract, and a dog's
/// own arguments are its business.
fn first_argument() -> Option<String> {
    std::env::args().nth(1)
}

/// Prints an answer and ends the process.
///
/// The explicit flush matters: [`std::process::exit`] runs no destructor,
/// so nothing else would push a partial buffer out.
fn answer(text: &str) -> ! {
    let mut stdout = std::io::stdout();
    // A write to a closed stdout is not something a dog can do anything
    // about, and shep reads it as silence, which is a legal answer.
    let _ = write!(stdout, "{text}");
    let _ = stdout.flush();
    std::process::exit(0);
}

/// The `--version` answer, whole, ending in a newline.
///
/// Split out from [`probe`] so the format can be tested against
/// [`shep_core::dogs::parse_version_answer`], the code that reads it.
fn version_answer(name: &str, version: &str) -> String {
    format!(
        "{name} {version}\n{SHEP_PROTOCOL_KEY}: {}\n",
        crate::PROTOCOL_VERSION
    )
}

/// The `--schema` answer, whole, ending in a newline.
#[cfg(feature = "schema")]
fn schema_answer<T: DogConfig + schemars::JsonSchema>() -> String {
    let schema = config_schema::<T>();
    // The same expectation shep-core's own schema printer holds: a schemars
    // `Schema` is a `serde_json::Value` already, so serializing it cannot
    // meet a type serde_json has no representation for.
    let json = serde_json::to_string_pretty(&schema).expect("a schemars Schema always serializes");
    format!("{json}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two ends of the grammar: what a dog prints and what shep reads.
    /// Pinned as a round trip rather than as a string, because the format is
    /// only ever interesting to the parser.
    #[test]
    fn the_version_answer_parses_with_the_shepherds_own_parser() {
        let answer = version_answer("shep-otel", "0.1.3");
        let parsed = shep_core::dogs::parse_version_answer(&answer)
            .expect("the answer shep's own parser cannot read is the bug this pins");

        assert_eq!(parsed.version, "0.1.3");
        assert_eq!(parsed.protocol, Some(crate::PROTOCOL_VERSION));
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
    }
}
