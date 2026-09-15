//! What a dog is, and the process spec the daemon spawns it as.
//!
//! [`dog_app`] assembles the same [`ResolvedApp`] a Flockfile entry would,
//! and the supervisor supervises it as a sheep. Both [`DogSpec::source`]
//! kinds run at the daemon's own trust level. [`spawn_enabled_dogs`] is the
//! boot-time caller: it starts every configured dog and never fails the boot
//! over one that would not start.

use core::fmt;
use std::path::PathBuf;

use shep_core::config::{AppConfig, ResolvedApp, normalize};
use shep_core::paths::ShepPaths;
use shep_core::protocol::DogSource;

use crate::bus::Bus;
use crate::supervisor::SupervisorHandle;

use super::narrate::narrate;

/// One dog the daemon knows about: its name, and where its binary comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DogSpec {
    /// The dog's name: the `[<name>]` key and the entry's name.
    pub name: String,
    /// Where its binary comes from.
    pub source: DogSource,
}

/// Error assembling a dog's app config, or reading its section
///
/// `Debug` needs no redaction: a path, a normalizer complaint, or a TOML
/// parser message, never a value read out of a parsed `[<name>]` table. A
/// syntax error can quote a line of the section's own source, but only to
/// the peer that asked, which peer-cred auth already established owns the
/// file.
///
/// [`Self::NoBinary`] and [`Self::Io`] wrap their [`std::io::Error`] rather
/// than rendering it, which costs this enum `Clone`, `PartialEq` and `Eq`.
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
        }
    }
}

impl core::error::Error for DogError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::NoBinary(err) | Self::Io(err) => Some(err),
            Self::UnsupportedSource(_) | Self::Config(_) => None,
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
/// operator's binary with no arguments. The environment carries exactly
/// `SHEP_HOME` and `SHEP_DOG_NAME`, never a `[<name>]` value: a dog asks for
/// its section over the socket.
///
/// # Errors
/// - [`DogError::NoBinary`] if a built-in dog has no program to run.
/// - [`DogError::UnsupportedSource`] if the source is a kind this build does
///   not know how to spawn.
/// - [`DogError::Config`] if the assembled config failed `normalize`.
pub fn dog_app(spec: &DogSpec, paths: &ShepPaths) -> Result<ResolvedApp, DogError> {
    let (script, args) = match &spec.source {
        DogSource::BuiltIn => (
            builtin_program()?.display().to_string(),
            vec!["dog".to_string(), spec.name.clone()],
        ),
        // No arguments: an adopted dog is somebody else's binary, and an argv
        // shep invented for it is one more thing it has to agree with.
        DogSource::Adopted { path } => (path.clone(), Vec::new()),
        source => return Err(DogError::UnsupportedSource(format!("{source:?}"))),
    };

    let mut config = AppConfig::minimal(&spec.name, &script);
    config.args = args;
    config
        .env
        .insert("SHEP_HOME".to_string(), paths.home.display().to_string());
    // The `[<name>]` key this dog's section lives beneath, and so the `name`
    // it puts in `Request::DogConfig`. An adopted dog has no argv to read it
    // from, so this is its only channel.
    config
        .env
        .insert("SHEP_DOG_NAME".to_string(), spec.name.clone());
    normalize(config).map_err(|err| DogError::Config(err.to_string()))
}

/// Starts every dog in `specs`, warning and carrying on for each one that
/// will not start.
///
/// Never fails the boot: a dog that cannot be spawned is a monitoring gap, and
/// refusing to bring the flock up over it turns that gap into an outage.
/// [`SupervisorHandle::start_dog`] is idempotent by name, so an `Ok` reply
/// carrying no `dog` means a sheep already held the name and nothing started.
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
    use crate::testing::test_paths;

    /// `current_exe` cannot safely be made to return `" (deleted)"`, so this
    /// drives `crate::handover::resolve_target`, which `builtin_program`
    /// delegates to. Unix only, because `handover` is.
    #[cfg(unix)]
    #[test]
    fn a_deleted_inode_answer_from_current_exe_never_becomes_a_dogs_script() {
        let refusal = crate::handover::resolve_target(
            [None, Some(PathBuf::from("/opt/shep/shep (deleted)"))],
            None,
        )
        .unwrap_err();
        let err = DogError::NoBinary(refusal);
        assert_eq!(
            err.to_string(),
            "this binary's own path is unresolvable: no binary to exec: \
             /opt/shep/shep (deleted) (names a deleted inode, not a file)"
        );
    }

    /// Asserted over the assembled spec rather than the config, because
    /// `assemble` is where an env map would be merged. `SHEP_DOG_NAME` is no
    /// exception to the rule: it is the key a dog needs to ask for its section
    /// at all.
    #[test]
    fn a_dogs_child_environment_carries_shep_home_and_its_name_and_no_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::write(
            &paths.dogs_config,
            "[bark]\nwebhook = \"https://example.invalid/hook\"\n",
        )
        .unwrap();
        let spec = DogSpec {
            name: "bark".to_string(),
            source: DogSource::BuiltIn,
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

    #[test]
    fn a_built_in_dog_runs_this_binary_and_an_adopted_one_runs_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);

        let built_in = dog_app(
            &DogSpec {
                name: "metrics".to_string(),
                source: DogSource::BuiltIn,
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
                    path: "/usr/local/bin/shep-otel".to_string(),
                },
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
                    path: "/usr/local/bin/shep-otel".to_string(),
                },
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
