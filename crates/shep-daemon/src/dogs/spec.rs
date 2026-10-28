//! What a dog is, and the process spec the daemon spawns it as.
//!
//! [`dog_app`] assembles the same [`ResolvedApp`] a Flockfile entry would,
//! and the supervisor supervises it as a sheep. Both [`DogSpec::source`]
//! kinds run at the daemon's own trust level. [`spawn_enabled_dogs`] is the
//! boot-time caller: it starts every configured dog and never fails the boot
//! over one that would not start.

use core::fmt;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::PathBuf;

use shep_core::config::template::RenderError;
use shep_core::config::{AppConfig, ResolvedApp, normalize};
use shep_core::paths::ShepPaths;
use shep_core::protocol::DogSource;
use shep_core::protocol::ProcessInfo;
use shep_core::status::ProcStatus;

use crate::bus::Bus;
use crate::supervisor::SupervisorHandle;

use super::narrate::narrate;
use super::secrets::StoreFault;

/// One dog the daemon knows about: its name, where its binary comes from,
/// and the environment the operator set for it.
///
/// `Debug` is manual and redacted (IR-41): `env` is the operator's own
/// values, `{{secret:...}}` references and literals alike, and a spec is
/// what a boot or an `EnableDog` logs a dog's name from.
#[derive(Clone, PartialEq, Eq)]
pub struct DogSpec {
    /// The dog's name: the `[<name>]` key and the entry's name.
    pub name: String,
    /// Where its binary comes from.
    pub source: DogSource,
    /// Environment the operator set for this dog, as `shep.toml` wrote it.
    ///
    /// A `{{secret:...}}` is still a reference here: [`crate::assemble::assemble`]
    /// resolves it at spawn, the way a sheep's `env` is. Empty for a
    /// built-in dog, which has no `[daemon.adopted_dogs]` entry to read one
    /// from.
    pub env: BTreeMap<String, String>,
}

impl fmt::Debug for DogSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "DogSpec {{ name: {:?}, source: {:?}, env: <{} keys> }}",
            self.name,
            self.source,
            self.env.len()
        )
    }
}

/// The sentence this shepherd answers a request to delete a built-in dog with.
///
/// A built-in dog is an argv branch of the shep binary, so nothing removes it:
/// `delete` has no registration to forget and no binary to lose. `disable`
/// is the verb that exists for it, and the refusal names it.
///
/// Public because `shep-cli` reports it for a shepherd that is not running.
#[must_use]
pub fn built_in_dog_refusal(name: &str) -> String {
    format!("{name} is built in: `shep disable {name}`")
}

/// Error assembling a dog's app config, or reading its section
///
/// `Debug` needs no redaction: a path, a normalizer complaint, or a TOML
/// parser message, never a value read out of a parsed `[<name>]` table. The
/// two secret variants hold a key path, a reference as written, an
/// environment name and a [`StoreFault`], so no field can hold a resolved
/// value (IR-41).
#[non_exhaustive]
#[derive(Debug)]
pub enum DogError {
    /// A built-in dog has no program it can be spawned with: either
    /// [`std::env::current_exe`] failed, or handover-target resolution refused
    /// every candidate.
    NoBinary(std::io::Error),
    /// The dog's binary comes from a source this build cannot spawn (carries
    /// the source as `Debug` renders it). [`DogSource`] is `#[non_exhaustive]`,
    /// so a name enabled by a newer shep can reach an older daemon.
    UnsupportedSource(String),
    /// The assembled config failed `normalize`, or the file read is not
    /// valid `shep.toml`, or the section it holds cannot be rendered back to
    /// TOML (carries the rejection message)
    Config(String),
    /// The file exists and could not be read
    Io(std::io::Error),
    /// A `{{secret:...}}` in the section cannot be resolved: it names no
    /// value, names a namespace no provider has pushed, or is outside the
    /// reference grammar
    Reference {
        /// Where the string sits in the section, as `sinks.local.url` or
        /// `rules[1].url`
        key_path: String,
        /// Which reference, and why. Holds the reference as written and the
        /// environment, never a value.
        source: RenderError,
    },
    /// The section names a reference and the secret store could not be read
    SecretStore {
        /// Where the first reference in the section sits
        key_path: String,
        /// What went wrong, without the store's own message
        fault: StoreFault,
    },
}

impl fmt::Display for DogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoBinary(err) => write!(f, "this binary's own path is unresolvable: {err}"),
            Self::UnsupportedSource(source) => {
                write!(f, "no way to spawn a dog from source {source}")
            }
            Self::Config(msg) => write!(f, "dog configuration is unusable: {msg}"),
            Self::Io(err) => write!(f, "dog configuration could not be read: {err}"),
            Self::Reference { key_path, source } => write!(f, "`{key_path}`: {source}"),
            Self::SecretStore { key_path, fault } => write!(
                f,
                "`{key_path}`: the secret store could not be read ({fault})"
            ),
        }
    }
}

impl core::error::Error for DogError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::NoBinary(err) | Self::Io(err) => Some(err),
            Self::Reference { source, .. } => Some(source),
            Self::UnsupportedSource(_) | Self::Config(_) | Self::SecretStore { .. } => None,
        }
    }
}

/// The program a built-in dog is spawned as: this binary's own resolved path.
///
/// Through `handover::exec_target` rather than [`std::env::current_exe`],
/// which on Linux answers `"<path> (deleted)"` once a package manager has
/// replaced the running binary, and that cannot be exec'd. A respawned dog
/// can therefore run newer code than the shepherd currently has loaded, if
/// the binary was replaced before this shepherd reloaded or handed over.
#[cfg(unix)]
fn builtin_program() -> Result<PathBuf, DogError> {
    crate::handover::exec_target().map_err(DogError::NoBinary)
}

/// The program a built-in dog is spawned as, on a platform with no handover.
///
/// Windows refuses to replace a running executable, so no unlinked inode can
/// exist for `current_exe` to name.
#[cfg(windows)]
fn builtin_program() -> Result<PathBuf, DogError> {
    std::env::current_exe().map_err(DogError::NoBinary)
}

/// The app config the daemon spawns `spec` from.
///
/// A built-in dog is `<this binary> dog <name>`; an adopted one is the
/// operator's binary with no arguments. The environment carries the
/// operator's `env`, then `SHEP_HOME` and `SHEP_DOG_NAME` over it: those two
/// are what a dog needs to ask for its own section at all. A `[<name>]`
/// value never travels here, and an adopted dog that asked for the shepherd
/// channel gets the channel's own variables too.
///
/// # Errors
/// - [`DogError::NoBinary`] if a built-in dog has no program to run.
/// - [`DogError::UnsupportedSource`] if the source is a kind this build does
///   not know how to spawn. [`DogError::Config`] if the config failed `normalize`.
pub fn dog_app(spec: &DogSpec, paths: &ShepPaths) -> Result<ResolvedApp, DogError> {
    let (script, args, channel) = match &spec.source {
        DogSource::BuiltIn { .. } => (
            builtin_program()?.display().to_string(),
            vec!["dog".to_string(), spec.name.clone()],
            false,
        ),
        // No arguments: an adopted dog is somebody else's binary, and an argv
        // shep invented for it is one more thing it has to agree with.
        DogSource::Adopted { path, channel, .. } => (path.clone(), Vec::new(), *channel),
        source => return Err(DogError::UnsupportedSource(format!("{source:?}"))),
    };

    let mut config = AppConfig::minimal(&spec.name, &script);
    config.args = args;
    config.channel = channel;
    config.shutdown_with_message = channel;
    // The operator's half first, so the two names the shepherd injects land
    // on top of anything `env` tried to claim. `normalize` validates each
    // value's `{{...}}` grammar here; the store lookup happens at spawn.
    config.env.extend(spec.env.clone());
    config
        .env
        .insert("SHEP_HOME".to_string(), paths.home.display().to_string());
    // The `[<name>]` key this dog's section lives beneath, and so the `name`
    // it puts in `Request::DogConfig`. An adopted dog has no argv to read it
    // from, so this is its only channel.
    config
        .env
        .insert(shep_core::dogs::DOG_NAME_VAR.to_string(), spec.name.clone());
    normalize(config).map_err(|err| DogError::Config(err.to_string()))
}

/// Registers every dog in `known` and `enabled` at rest, so the flock lists
/// it whether or not it runs.
///
/// A dog outside `enabled` is registered `Stopped` and disabled; one in it is
/// registered `Stopped` all the same, for [`spawn_enabled_dogs`] to start. A
/// dog already registered under its name keeps its row and its mark, except
/// for the ones `stop_unenabled_dogs` takes back.
pub async fn register_dogs_at_rest(
    known: &[DogSpec],
    enabled: &[DogSpec],
    paths: &ShepPaths,
    supervisor: &SupervisorHandle,
) {
    let mut seen = BTreeSet::new();
    let mut dogs = Vec::new();
    for spec in enabled.iter().chain(known) {
        if !seen.insert(spec.name.as_str()) {
            continue;
        }
        match dog_app(spec, paths) {
            Ok(app) => dogs.push((app, spec.source.clone())),
            Err(err) => tracing::warn!(dog = %spec.name, %err, "a dog was not listed"),
        }
    }
    let disabled: BTreeSet<String> = known
        .iter()
        .map(|spec| spec.name.clone())
        .filter(|name| !enabled.iter().any(|spec| spec.name == *name))
        .collect();
    if let Err(err) = supervisor.register_dogs_at_rest(dogs, disabled).await {
        tracing::warn!(%err, "the dogs were not listed");
        return;
    }
    // The flock, and not what this call handed the actor: a dog whose own
    // config failed to assemble never reached the actor at all, and a carried
    // one is running there regardless.
    let flock = match supervisor.list_checked().await {
        Ok(flock) => flock,
        Err(err) => {
            tracing::warn!(%err, "the flock could not be read to check its dogs against");
            return;
        }
    };
    stop_unenabled_dogs(&flock, enabled, supervisor).await;
}

/// Stops and marks every dog row in `flock` that `enabled` does not name,
/// except one already stopped and marked.
///
/// The case is a handover successor's: a predecessor ran the dog and carried
/// it across the `execve`, and this boot read a file that has since dropped
/// the name. It goes down through the operator's own door, which marks the
/// app as it stops it, so `shep enable <name>` is the way back.
async fn stop_unenabled_dogs(
    flock: &[ProcessInfo],
    enabled: &[DogSpec],
    supervisor: &SupervisorHandle,
) {
    for info in flock {
        let named = enabled.iter().any(|spec| spec.name == info.name);
        let parked = info.disabled == Some(true) && info.status == ProcStatus::Stopped;
        if info.dog.is_none() || named || parked {
            continue;
        }
        if let Err(err) = supervisor.disable_dog(info.name.clone()).await {
            tracing::warn!(
                dog = %info.name,
                %err,
                "a dog this shepherd's config does not enable was left running"
            );
        }
    }
}

/// Starts every dog in `specs`, warning and carrying on for each one that
/// will not start.
///
/// Never fails the boot: a dog that cannot be spawned is a monitoring gap,
/// and refusing to bring the flock up over it turns that gap into an outage.
/// [`SupervisorHandle::start_dog`] is idempotent by name for a dog that runs,
/// so an `Ok` reply carrying no `dog` means a sheep already held the name.
pub async fn spawn_enabled_dogs(
    specs: &[DogSpec],
    paths: &ShepPaths,
    supervisor: &SupervisorHandle,
    events: &Bus,
) {
    for spec in specs {
        let app = match dog_app(spec, paths) {
            Ok(app) => app,
            Err(err) => {
                tracing::warn!(dog = %spec.name, %err, "a dog did not start");
                continue;
            }
        };
        // Read before `start_dog` takes the app: this is the one place that
        // knows which file the spawn resolved to.
        let script = app.config().script.clone();
        match supervisor.start_dog(app, spec.source.clone()).await {
            Ok(info) if info.dog.is_none() => tracing::warn!(
                dog = %spec.name,
                "a sheep is already registered under this name; the dog did not start"
            ),
            Ok(info) => {
                // `start_dog` is idempotent by name, so this reply may be a
                // dog that was already running: the wording is about the
                // binary this shepherd resolved, not about a spawn.
                narrate(
                    events,
                    &info,
                    &format!("shep has this dog enabled, running the binary at {script}"),
                )
                .await;
            }
            Err(err) => tracing::warn!(dog = %spec.name, %err, "a dog did not start"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::SpawnSpec;
    use crate::testing::test_paths;
    use shep_core::secrets::SecretView;

    /// `current_exe` cannot safely be made to return `" (deleted)"`, so this
    /// pins what `builtin_program` does with the answer it gets: it is the
    /// shared resolution's, which is never a deleted inode and always a file.
    #[cfg(unix)]
    #[test]
    fn a_built_in_dogs_program_is_the_resolved_exec_target_and_never_a_deleted_inode() {
        let program = builtin_program().expect("the test binary resolves");

        assert_eq!(program, crate::handover::exec_target().unwrap());
        assert!(program.is_file(), "{}", program.display());
        assert!(
            !program.to_string_lossy().ends_with(" (deleted)"),
            "{}",
            program.display()
        );
    }

    #[test]
    fn an_unresolvable_binary_says_so_and_keeps_the_refusal_as_its_source() {
        use core::error::Error as _;

        let refusal = std::io::Error::new(std::io::ErrorKind::NotFound, "no binary to exec");
        let err = DogError::NoBinary(refusal);

        assert_eq!(
            err.to_string(),
            "this binary's own path is unresolvable: no binary to exec"
        );
        assert_eq!(err.source().unwrap().to_string(), "no binary to exec");
    }

    /// Asserted over the assembled spec rather than the config, because
    /// `assemble` is where an env map would be merged. `SHEP_DOG_NAME` is no
    /// exception to the rule: it is the key a dog needs to ask for its section
    /// at all.
    #[test]
    fn a_dogs_child_environment_carries_shep_home_and_its_name_and_no_section() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::write(
            &paths.dogs_config,
            "[bark]\nwebhook = \"https://example.invalid/hook\"\n",
        )
        .unwrap();
        let spec = DogSpec {
            name: "bark".to_string(),
            source: DogSource::BuiltIn { grants: None },
            env: BTreeMap::new(),
        };
        let app = dog_app(&spec, &paths).unwrap();
        // A dog's config is built by `dog_app`, never operator-templated,
        // so an empty view resolves everything it holds.
        let assembled = crate::assemble::assemble(
            &app,
            0,
            &paths,
            None,
            &shep_core::secrets::SecretView::empty("production".to_string()),
        )
        .expect("a dog's own config carries no template to refuse");
        assert_eq!(
            assembled.env.get("SHEP_HOME"),
            Some(&paths.home.display().to_string())
        );
        assert_eq!(
            assembled.env.get("SHEP_DOG_NAME"),
            Some(&"bark".to_string()),
            "a dog is told the name its own section lives under"
        );
        assert!(
            !assembled
                .env
                .values()
                .any(|v| v.contains("example.invalid")),
            "a dog's configuration never travels in its environment: {:?}",
            assembled.env
        );
    }

    /// A dog that runs other programs, or a library that reads its key from
    /// the environment, gets it from `[daemon.adopted_dogs]` the way a sheep
    /// gets it from its own `env`.
    #[test]
    fn an_adopted_dogs_env_reaches_its_child_environment() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        let spec = adopted(
            "kelpie",
            BTreeMap::from([
                ("PADDOCK_KEY".to_string(), "lease-key".to_string()),
                ("RUST_LOG".to_string(), "warn".to_string()),
            ]),
        );
        let assembled = spawn_env(&spec, &paths, &SecretView::empty("production".to_string()));

        assert_eq!(
            assembled.env.get("PADDOCK_KEY"),
            Some(&"lease-key".to_string()),
            "the operator's key reaches the dog that asked for one"
        );
        assert_eq!(assembled.env.get("RUST_LOG"), Some(&"warn".to_string()));
        assert_eq!(
            assembled.env.get("SHEP_HOME"),
            Some(&paths.home.display().to_string())
        );
        assert_eq!(
            assembled.env.get("SHEP_DOG_NAME"),
            Some(&"kelpie".to_string())
        );
    }

    /// The store lookup happens at spawn, exactly as it does for a sheep's
    /// `env`, so the reference is what sits in `shep.toml` and the value is
    /// what the child reads.
    #[test]
    fn an_adopted_dogs_env_secret_arrives_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        let spec = adopted(
            "kelpie",
            BTreeMap::from([(
                "PADDOCK_KEY".to_string(),
                "{{secret:PADDOCK_KEY}}".to_string(),
            )]),
        );
        let view = SecretView::new(
            "production".to_string(),
            BTreeMap::from([(
                "PADDOCK_KEY".to_string(),
                BTreeMap::from([("production".to_string(), "gateway-key".to_string())]),
            )]),
            shep_core::secrets::ProviderCache::default(),
        );
        let assembled = spawn_env(&spec, &paths, &view);

        assert_eq!(
            assembled.env.get("PADDOCK_KEY"),
            Some(&"gateway-key".to_string())
        );
        assert!(
            !assembled.env.values().any(|v| v.contains("{{secret:")),
            "no reference reaches the child once the store has answered: {:?}",
            assembled.env
        );
    }

    /// A reference the store cannot answer costs the spawn, not the boot: the
    /// dog comes up `Errored` and the restart policy brings it back once the
    /// key is pushed.
    #[test]
    fn an_adopted_dogs_env_secret_the_store_cannot_answer_refuses_the_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        let spec = adopted(
            "kelpie",
            BTreeMap::from([("PADDOCK_KEY".to_string(), "{{secret:ABSENT}}".to_string())]),
        );
        let app = dog_app(&spec, &paths).expect("the reference grammar is valid");
        let err = crate::assemble::assemble(
            &app,
            0,
            &paths,
            None,
            &SecretView::empty("production".to_string()),
        )
        .expect_err("no value for ABSENT");
        assert!(
            matches!(
                err,
                crate::assemble::AssembleError::Template { ref field, .. } if field == "PADDOCK_KEY"
            ),
            "{err:?}"
        );
    }

    /// The two names a dog needs to ask for its own section at all are the
    /// shepherd's, whatever `env` claims.
    #[test]
    fn a_dogs_env_cannot_replace_shep_home_or_the_name_it_was_registered_under() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        let spec = adopted(
            "kelpie",
            BTreeMap::from([
                ("SHEP_HOME".to_string(), "/home/ada/elsewhere".to_string()),
                ("SHEP_DOG_NAME".to_string(), "someone-else".to_string()),
            ]),
        );
        let assembled = spawn_env(&spec, &paths, &SecretView::empty("production".to_string()));

        assert_eq!(
            assembled.env.get("SHEP_HOME"),
            Some(&paths.home.display().to_string()),
            "the shepherd's own home is not the operator's to rename"
        );
        assert_eq!(
            assembled.env.get("SHEP_DOG_NAME"),
            Some(&"kelpie".to_string())
        );
    }

    /// A dog's `env` is checked against the same reserved names a sheep's is.
    #[test]
    fn a_dogs_env_cannot_claim_a_name_the_shepherd_injects_into_every_child() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        let spec = adopted(
            "kelpie",
            BTreeMap::from([("SHEP_NAME".to_string(), "x".to_string())]),
        );

        let err = dog_app(&spec, &paths).expect_err("SHEP_NAME is reserved");
        assert!(matches!(err, DogError::Config(_)), "{err:?}");
        assert!(err.to_string().contains("SHEP_NAME"), "{err}");
    }

    /// IR-41. A spec is what a boot logs a dog's name from, and `env` is the
    /// operator's own values. Exact string, so a `#[derive(Debug)]` on
    /// [`DogSpec`] fails here.
    #[test]
    fn a_dogs_spec_debug_prints_its_name_and_source_and_no_env_value() {
        let spec = adopted(
            "kelpie",
            BTreeMap::from([("PADDOCK_KEY".to_string(), "gateway-key".to_string())]),
        );

        assert_eq!(
            format!("{spec:?}"),
            "DogSpec { name: \"kelpie\", source: Adopted { path: \"/usr/local/bin/shep-kelpie\", \
             channel: false, grants: None }, env: <1 keys> }"
        );
    }

    /// One dog as `shep.toml` would hand it to the shepherd: adopted, with
    /// `env` set.
    fn adopted(name: &str, env: BTreeMap<String, String>) -> DogSpec {
        DogSpec {
            name: name.to_string(),
            source: DogSource::Adopted {
                grants: None,
                path: "/usr/local/bin/shep-kelpie".to_string(),
                channel: false,
            },
            env,
        }
    }

    /// The environment `dog_app`'s config actually spawns with, which is where
    /// an `env` map is merged and a reference is resolved.
    fn spawn_env(spec: &DogSpec, paths: &ShepPaths, secrets: &SecretView) -> SpawnSpec {
        let app = dog_app(spec, paths).expect("the fixture's config assembles");
        crate::assemble::assemble(&app, 0, paths, None, secrets)
            .expect("the fixture's spawn assembles")
    }

    #[test]
    fn a_built_in_dog_runs_this_binary_and_an_adopted_one_runs_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);

        let built_in = dog_app(
            &DogSpec {
                name: "metrics".to_string(),
                source: DogSource::BuiltIn { grants: None },
                env: BTreeMap::new(),
            },
            &paths,
        )
        .unwrap();
        assert_eq!(
            built_in.config().script,
            std::env::current_exe().unwrap().display().to_string()
        );
        assert_eq!(built_in.config().args, vec!["dog", "metrics"]);

        let adopted = dog_app(
            &DogSpec {
                name: "otel".to_string(),
                source: DogSource::Adopted {
                    grants: None,
                    path: "/usr/local/bin/shep-otel".to_string(),
                    channel: false,
                },
                env: BTreeMap::new(),
            },
            &paths,
        )
        .unwrap();
        assert_eq!(adopted.config().script, "/usr/local/bin/shep-otel");
        assert!(adopted.config().args.is_empty());
        assert_eq!(
            adopted.config().name,
            "otel",
            "the NAME is the config key, never the filename"
        );
    }

    /// A dog's log also holds shep's narration, which `narrate` always
    /// stamps, so the dog's own lines must carry the stamp too.
    #[test]
    fn a_dogs_own_log_lines_are_stamped() {
        let dir = tempfile::tempdir().unwrap();
        let spec = DogSpec {
            name: "metrics".to_string(),
            source: DogSource::BuiltIn { grants: None },
            env: BTreeMap::new(),
        };

        assert!(
            dog_app(&spec, &test_paths(&dir))
                .unwrap()
                .config()
                .log_timestamps
        );
    }

    #[test]
    fn only_an_adopted_dog_that_asked_gets_the_channel_and_a_stop_by_message() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        let adopted = |channel| DogSource::Adopted {
            grants: None,
            path: "/usr/local/bin/shep-otel".to_string(),
            channel,
        };

        for (source, expected) in [
            (adopted(true), true),
            (adopted(false), false),
            (DogSource::BuiltIn { grants: None }, false),
        ] {
            let spec = DogSpec {
                name: "otel".to_string(),
                source: source.clone(),
                env: BTreeMap::new(),
            };
            let app = dog_app(&spec, &paths).unwrap();
            let config = app.config();
            assert_eq!(config.channel, expected, "{source:?}");
            assert_eq!(config.shutdown_with_message, expected, "{source:?}");
        }
    }

    /// An adopted dog is given no argv, so the environment is its only
    /// channel, and a mismatch looks exactly like a dog with no configuration.
    /// The name is the one the operator chose, not the binary's file stem.
    #[test]
    fn an_adopted_dog_is_told_the_name_it_was_registered_under() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);

        let adopted = dog_app(
            &DogSpec {
                name: "telemetry".to_string(),
                source: DogSource::Adopted {
                    grants: None,
                    path: "/usr/local/bin/shep-otel".to_string(),
                    channel: false,
                },
                env: BTreeMap::new(),
            },
            &paths,
        )
        .unwrap();

        assert!(
            adopted.config().args.is_empty(),
            "the name arrives without shep inventing an argv for a foreign binary"
        );
        assert_eq!(
            adopted.config().env.get("SHEP_DOG_NAME"),
            Some(&"telemetry".to_string())
        );
    }
}
