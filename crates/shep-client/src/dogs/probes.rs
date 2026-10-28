//! Answering shep's probes: the manifest flag, the version flag and the
//! schema flag.

use std::io::Write as _;

use shep_core::dogs::manifest::{DogDependency, Manifest, Permission, Verb};
use shep_core::dogs::{
    MANIFEST_FLAG, SCHEMA_FLAG, SHEP_CHANNEL_KEY, SHEP_PROTOCOL_KEY, VERSION_FLAG,
};

use super::DogConfig;
#[cfg(feature = "schema")]
use super::{config_schema, config_schema_with_sheep};

/// Answers shep's probes, and returns when this run is not a probe.
///
/// Shorthand for <code>[Probe::new]\(name, version).[answer::\<T>](Probe::answer)()</code>.
///
/// # Exits
///
/// Ends the process with [`process::exit`](std::process::exit), status 0, before `main` opens.
#[cfg(feature = "schema")]
pub fn probe<T: DogConfig + schemars::JsonSchema>(name: &str, version: &str) {
    Probe::new(name, version).answer::<T>();
}

/// [`probe`], for a dog whose per-sheep `[app.dogs.<name>]` table has its own schema.
/// The schema flag answers with [`config_schema_with_sheep::<T, S>`](config_schema_with_sheep).
///
/// # Exits
///
/// Ends the process with [`process::exit`](std::process::exit), status 0, before `main` opens.
///
/// # Examples
///
/// ```no_run
/// # #[shep_client::dogs::dog_config]
/// # #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
/// # struct MyDogConfig {}
/// # #[shep_client::dogs::dog_config]
/// # #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
/// # struct MySheepSettings {}
/// fn main() {
///     shep_client::dogs::probe_with_sheep::<MyDogConfig, MySheepSettings>(
///         env!("CARGO_PKG_NAME"),
///         env!("CARGO_PKG_VERSION"),
///     );
///     // ...normal startup, reached only when this run is not a probe.
/// }
/// ```
#[cfg(feature = "schema")]
pub fn probe_with_sheep<T, S>(name: &str, version: &str)
where
    T: DogConfig + schemars::JsonSchema,
    S: DogConfig + schemars::JsonSchema,
{
    Probe::new(name, version).answer_with_sheep::<T, S>();
}

/// Answers shep's probes, and returns when this run is not a probe.
///
/// With the `schema` feature off, the schema flag exits without printing.
///
/// # Exits
///
/// Ends the process with [`process::exit`](std::process::exit), status 0, before `main` opens.
#[cfg(not(feature = "schema"))]
pub fn probe<T: DogConfig>(name: &str, version: &str) {
    Probe::new(name, version).answer::<T>();
}

/// [`probe_with_sheep`] with the `schema` feature off: behaves exactly like
/// [`probe`] does without the feature.
///
/// # Exits
///
/// Ends the process with [`process::exit`](std::process::exit), status 0, before `main` opens.
#[cfg(not(feature = "schema"))]
pub fn probe_with_sheep<T: DogConfig, S: DogConfig>(name: &str, version: &str) {
    Probe::new(name, version).answer_with_sheep::<T, S>();
}

/// shep's probes, answered with something the dog asks of shep as well as
/// its name and version.
///
/// [`probe`] asks for nothing; build one of these only to ask. Call
/// [`Self::answer`] as the first line of `main`, as [`probe`] would be. It
/// answers [`MANIFEST_FLAG`] with a [`Manifest`] built from what the dog gives it.
///
/// # Examples
///
/// ```no_run
/// # #[shep_client::dogs::dog_config]
/// # #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
/// # struct MyDogConfig {}
/// use shep_client::dogs::{Permission, Probe};
///
/// fn main() {
///     Probe::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
///         .ask_for_channel()
///         .permission(Permission::new("flock.read", true, "lists the flock"))
///         .answer::<MyDogConfig>();
///     // ...normal startup, reached only when this run is not a probe.
/// }
/// ```
#[derive(Debug, Clone)]
#[must_use = "a Probe answers nothing until `answer` is called"]
pub struct Probe<'a> {
    name: &'a str,
    version: &'a str,
    channel: bool,
    permissions: Vec<Permission>,
    verb: Option<Verb>,
    dogs: Vec<DogDependency>,
    shep: Option<String>,
    defaults: Option<String>,
    validate: bool,
}

impl<'a> Probe<'a> {
    /// A probe answering with `name` and `version`, asking for nothing.
    pub const fn new(name: &'a str, version: &'a str) -> Self {
        Self {
            name,
            version,
            channel: false,
            permissions: Vec::new(),
            verb: None,
            dogs: Vec::new(),
            shep: None,
            defaults: None,
            validate: false,
        }
    }

    /// Asks for the shepherd channel, which `shep adopt` records.
    ///
    /// shep then starts the dog with `channel` and `shutdown_with_message`
    /// set: `shep trigger` reaches it, and a stop arrives as a message with
    /// no signal behind it until `kill_timeout` runs out. `shep_channel::serve`
    /// hands the dog both.
    pub const fn ask_for_channel(mut self) -> Self {
        self.channel = true;
        self
    }

    /// Declares a permission the dog asks for, in the manifest only: the
    /// version flag has no place for it.
    pub fn permission(mut self, permission: Permission) -> Self {
        self.permissions.push(permission);
        self
    }

    /// Declares how `shep <name>` is described in `--help` and completion.
    pub fn verb(mut self, verb: Verb) -> Self {
        self.verb = Some(verb);
        self
    }

    /// Declares another dog this one needs, at the versions it accepts.
    pub fn depends_on_dog(mut self, dependency: DogDependency) -> Self {
        self.dogs.push(dependency);
        self
    }

    /// Declares the oldest shep this dog supports, as a version.
    pub fn depends_on_shep(mut self, minimum: impl Into<String>) -> Self {
        self.shep = Some(minimum.into());
        self
    }

    /// Declares the TOML text of the dog's own section, which `shep adopt`
    /// writes when the dog has none yet.
    pub fn defaults(mut self, toml: impl Into<String>) -> Self {
        self.defaults = Some(toml.into());
        self
    }

    /// Declares that the dog validates a proposed settings table before it
    /// is written. It answers over the shepherd channel, so it also needs
    /// [`Self::ask_for_channel`], and [`validate_answer`](super::validate_answer)
    /// is the call that answers.
    pub const fn validate(mut self) -> Self {
        self.validate = true;
        self
    }

    /// Answers the manifest flag, the version flag, or the schema flag with
    /// [`config_schema::<T>`](config_schema), and returns when this run is
    /// not a probe.
    ///
    /// # Exits
    ///
    /// Ends the process with [`process::exit`](std::process::exit), status 0, before `main` opens.
    #[cfg(feature = "schema")]
    pub fn answer<T: DogConfig + schemars::JsonSchema>(self) {
        self.answering(|| Some(config_schema::<T>().into()));
    }

    /// [`Self::answer`], for a dog that also publishes a schema for its
    /// per-sheep table, as [`probe_with_sheep`] does.
    ///
    /// # Exits
    ///
    /// Ends the process with [`process::exit`](std::process::exit), status
    /// 0, before `main` opens anything.
    #[cfg(feature = "schema")]
    pub fn answer_with_sheep<T, S>(self)
    where
        T: DogConfig + schemars::JsonSchema,
        S: DogConfig + schemars::JsonSchema,
    {
        self.answering(|| Some(config_schema_with_sheep::<T, S>().into()));
    }

    /// Answers the manifest flag without `settings` and the version flag,
    /// and returns when this run is not a probe. With the `schema` feature
    /// off, the schema flag exits without printing.
    ///
    /// # Exits
    ///
    /// Ends the process with [`process::exit`](std::process::exit), status 0, before `main` opens.
    #[cfg(not(feature = "schema"))]
    pub fn answer<T: DogConfig>(self) {
        self.answering(|| None);
    }

    /// [`Self::answer`], since with the `schema` feature off there is no
    /// schema to answer with either way.
    ///
    /// # Exits
    ///
    /// Ends the process with [`process::exit`](std::process::exit), status
    /// 0, before `main` opens anything.
    #[cfg(not(feature = "schema"))]
    pub fn answer_with_sheep<T: DogConfig, S: DogConfig>(self) {
        self.answer::<T>();
    }

    /// The body every answer shares: the schema is rendered by `schema`
    /// only when a flag asks for it.
    fn answering(self, schema: impl FnOnce() -> Option<serde_json::Value>) {
        if let Some(text) = self.reply(first_argument().as_deref(), schema) {
            answer(&text);
        }
    }

    /// What `flag` is answered with, whole, or `None` when it is not a
    /// probe.
    ///
    /// The schema flag answers nothing when `schema` returns `None`, which
    /// shep reads as a dog with no schema.
    fn reply(
        &self,
        flag: Option<&str>,
        schema: impl FnOnce() -> Option<serde_json::Value>,
    ) -> Option<String> {
        match flag? {
            VERSION_FLAG => Some(self.version_answer()),
            SCHEMA_FLAG => Some(schema().map(|schema| rendered(&schema)).unwrap_or_default()),
            MANIFEST_FLAG => Some(rendered(&self.manifest(schema()))),
            _ => None,
        }
    }

    /// The manifest, with `settings` the schema the schema flag answers.
    fn manifest(&self, settings: Option<serde_json::Value>) -> Manifest {
        let mut manifest = Manifest::new(self.name, self.version, crate::PROTOCOL_VERSION);
        manifest.channel = self.channel;
        manifest.settings = settings;
        manifest.defaults.clone_from(&self.defaults);
        manifest.permissions.clone_from(&self.permissions);
        manifest.verb.clone_from(&self.verb);
        manifest.depends.dogs.clone_from(&self.dogs);
        manifest.depends.shep.clone_from(&self.shep);
        manifest.validate = self.validate;
        manifest
    }

    /// The `--version` answer, whole, ending in a newline.
    ///
    /// Split out so the format can be tested against
    /// [`shep_core::dogs::parse_version_answer`], the code that reads it.
    fn version_answer(&self) -> String {
        let mut text = format!(
            "{} {}\n{SHEP_PROTOCOL_KEY}: {}\n",
            self.name,
            self.version,
            crate::PROTOCOL_VERSION
        );
        if self.channel {
            text.push_str(&format!("{SHEP_CHANNEL_KEY}: true\n"));
        }
        text
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

/// `value` as the schema and manifest flags print it: pretty JSON and a
/// newline.
fn rendered(value: &impl serde::Serialize) -> String {
    // A schemars `Schema` is a `serde_json::Value` already and a `Manifest`
    // holds only strings, numbers and values, so serializing cannot meet a
    // type serde_json has no representation for.
    let json = serde_json::to_string_pretty(value).expect("these always serialize to JSON");
    format!("{json}\n")
}

#[cfg(all(test, feature = "schema"))]
mod manifest_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// The two ends of the grammar: what a dog prints and what shep reads.
    /// Pinned as a round trip rather than as a string, because the format is
    /// only ever interesting to the parser.
    #[test]
    fn the_version_answer_parses_with_the_shepherds_own_parser() {
        let answer = Probe::new("shep-otel", "0.1.3").version_answer();
        let parsed = shep_core::dogs::parse_version_answer(&answer)
            .expect("the answer shep's own parser cannot read is the bug this pins");

        assert_eq!(parsed.version, "0.1.3");
        assert_eq!(parsed.protocol, Some(crate::PROTOCOL_VERSION));
        assert!(
            !parsed.channel,
            "a dog that asks for nothing asks for nothing"
        );
    }

    #[test]
    fn a_channel_ask_reaches_the_shepherds_own_parser() {
        let answer = Probe::new("shep-otel", "0.1.3")
            .ask_for_channel()
            .version_answer();
        let parsed = shep_core::dogs::parse_version_answer(&answer).unwrap();

        assert!(parsed.channel, "{answer}");
        assert_eq!(parsed.protocol, Some(crate::PROTOCOL_VERSION));
    }
}
