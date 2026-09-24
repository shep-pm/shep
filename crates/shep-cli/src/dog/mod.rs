//! `shep dog <name>`: the hidden re-exec target a built-in dog runs as, and
//! [`DogRuntime`], the connection and configuration every dog needs.
//!
//! A dog inherits `$SHEP_HOME` and nothing else: no `[dog.<name>]` value
//! rides in the environment, since that is readable from the process
//! table and captured into crash dumps. [`DogRuntime::start`] instead
//! connects to the socket and asks for the section over
//! `Request::DogConfig`.
//!
//! [`run_dog`] parses the name into a [`BuiltInDog`], connects, and
//! dispatches: `"metrics"` to [`metrics::run`], `"bark"` to [`bark::run`].

pub mod bark;
pub mod metrics;
mod runtime;

pub use runtime::DogRuntime;
use runtime::exit_code_for;

use std::time::Duration;

use shep_core::paths::ShepPaths;

use crate::exit::ExitCode;

/// A dog this binary runs built-in.
///
/// Every `match` on it is exhaustive, so a new variant fails to compile
/// at each dispatch rather than reaching one that never heard of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuiltInDog {
    Metrics,
    Bark,
}

impl BuiltInDog {
    /// Every variant.
    const ALL: [Self; 2] = {
        // Exhaustive, so a new variant stops the build here, beside the list.
        match Self::Metrics {
            Self::Metrics | Self::Bark => {}
        }
        [Self::Metrics, Self::Bark]
    };

    /// The name `shep dog <name>` and `enabled_dogs` know it by.
    const fn name(self) -> &'static str {
        match self {
            Self::Metrics => "metrics",
            Self::Bark => "bark",
        }
    }

    /// The built-in dog called `name`, or [`None`] for any other name.
    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|dog| dog.name() == name)
    }
}

/// The dog names this binary can run built-in, [`BuiltInDog::name`] of
/// each variant.
///
/// `enabled_dogs` accepts any name at all, an adopted dog's own choice, but
/// a re-exec through `shep dog <name>` only ever reaches one of these.
/// [`run_dog`] refuses anything else before touching the socket.
pub(crate) const BUILT_IN_DOGS: [&str; BuiltInDog::ALL.len()] = {
    // A loop because `map` is not callable in a `const`.
    let mut names = [""; BuiltInDog::ALL.len()];
    let mut i = 0;
    while i < names.len() {
        names[i] = BuiltInDog::ALL[i].name();
        i += 1;
    }
    names
};

/// How long a dog waits for a shepherd to answer again before it gives up
/// and exits.
///
/// A shepherd that execs a successor has not gone away, and every dog is
/// meant to cross that without restarting. A shepherd that stopped has
/// gone away, and a dog that waits for it indefinitely is still running
/// when an unrelated shepherd binds that socket later, at which point it
/// attaches itself to that one beside that shepherd's own dog of the same
/// kind and doubles its alerts quietly.
///
/// Measured on this machine over ten `shep daemon reload` runs against a
/// three-sheep flock: the control socket turned away a full connect,
/// handshake and request for 38ms at the shortest, 254ms at the longest,
/// 80ms on average. A larger flock and a busier host both push that up.
///
/// [`DOG_SILENCE_BUDGET`](shep_daemon::dogs::DOG_SILENCE_BUDGET) is the
/// number to reuse rather than a second one to invent: it is how long the
/// shepherd lets a dog go quiet before acting on it, so a dog that waits
/// exactly that long cannot outlive the budget it is judged by. It is also
/// around twenty times the longest handover measured, which leaves room
/// for a far slower one. Five seconds of waiting is not the lingering this
/// guards against; that one is measured in however long it takes another
/// shepherd to come along.
const SHEPHERD_RETURN_BUDGET: Duration = shep_daemon::dogs::DOG_SILENCE_BUDGET;

/// The schema a built-in dog would print for the schema flag, without
/// spawning anything: a built-in dog is this binary, so the answer is one
/// call away rather than a subprocess and a timeout away.
///
/// [`None`] for a name that is not a built-in, which is how a caller tells
/// an adopted dog (spawn its recorded path and ask) from a built-in
/// (this).
pub(crate) fn builtin_schema(name: &str) -> Option<serde_json::Value> {
    use shep_client::dogs::config_schema;

    let schema = match BuiltInDog::from_name(name)? {
        BuiltInDog::Metrics => config_schema::<metrics::MetricsConfig>(),
        BuiltInDog::Bark => config_schema::<bark::BarkConfig>(),
    };
    serde_json::to_value(schema).ok()
}

/// Runs the named dog until it is signalled. `main`'s `Commands::Dog` arm.
///
/// An unknown name is refused before the socket is touched
/// ([`ExitCode::Usage`]), naming every built-in in the refusal.
///
/// A dog's own diagnostics go to stderr, plain text: the daemon's log pump
/// captures it into `$SHEP_HOME/logs/<name>-0-err.log` like any sheep's,
/// read with `shep bleats <name>`.
pub async fn run_dog(name: &str, paths: ShepPaths) -> ExitCode {
    let Some(dog) = BuiltInDog::from_name(name) else {
        let known: Vec<String> = BUILT_IN_DOGS
            .iter()
            .map(|known| format!("{known:?}"))
            .collect();
        eprintln!(
            "shep dog: unknown dog {name:?}; the built-in dogs are {}",
            known.join(", ")
        );
        return ExitCode::Usage;
    };
    let runtime = match DogRuntime::start(name, paths).await {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("shep dog {name}: {err}");
            return exit_code_for(&err);
        }
    };
    match dog {
        BuiltInDog::Metrics => metrics::run(runtime).await,
        BuiltInDog::Bark => bark::run(runtime).await,
    }
}

#[cfg(test)]
mod builtin_schema_tests {
    use super::*;

    /// The secret marker reaching the schema a pane reads is the one
    /// thing standing between a webhook bearer token and the screen.
    #[test]
    fn both_built_ins_answer_and_a_stranger_does_not() {
        assert!(builtin_schema("metrics").is_some());
        let bark = builtin_schema("bark").expect("bark is a built-in");
        assert_eq!(
            bark["properties"]["sinks"][shep_core::dogs::SECRET_KEY],
            true
        );
        assert!(builtin_schema("otel").is_none());
    }

    #[test]
    fn every_built_in_is_listed_by_name_and_parses_back() {
        assert_eq!(BUILT_IN_DOGS, BuiltInDog::ALL.map(BuiltInDog::name));
        for dog in BuiltInDog::ALL {
            assert_eq!(BuiltInDog::from_name(dog.name()), Some(dog));
        }
        assert_eq!(BuiltInDog::from_name("otel"), None);
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use shep_client::testing::{sample_ack, serve_one_request};
    use shep_core::protocol::{Request, Response};

    use super::*;

    /// fails if [`SHEPHERD_RETURN_BUDGET`] moves without the prose that
    /// names its value moving too.
    ///
    /// Three doc comments say "five seconds" in words: the constant's own,
    /// and the two `mod slow` headers that explain why those tests take
    /// that long. None of them can be checked by a reader, because the
    /// value arrives from `shep_daemon` rather than from the line above
    /// them, so a change there leaves all three quietly wrong while every
    /// doc-link still resolves.
    ///
    /// Pinning it here rather than deleting the numbers: a budget a reader
    /// has to go and look up is worse documentation, and this makes the
    /// concrete version safe to keep.
    #[test]
    fn the_budget_is_the_five_seconds_the_docs_promise() {
        assert_eq!(
            SHEPHERD_RETURN_BUDGET,
            Duration::from_secs(5),
            "docs/dogs.md, the web dogs page and three doc comments all say five \
             seconds; change them together or not at all"
        );
    }

    /// A [`ShepPaths`] rooted at `dir`, with `socket` pointed wherever the
    /// caller's fake daemon actually bound. Flat, not nested under `run/`,
    /// so a test never has to create that directory.
    pub(in crate::dog) fn test_paths(dir: &Path, socket: PathBuf) -> ShepPaths {
        let home = dir.to_path_buf();
        ShepPaths {
            daemon_config: home.join("shep.toml"),
            dogs_config: home.join("dogs.toml"),
            snapshot: home.join("flock.json"),
            logs: home.join("logs"),
            pids: home.join("pids"),
            run: home.join("run"),
            socket,
            barks: home.join("barks.jsonl"),
            kv: home.join("kv.json"),
            overrides: home.join("overrides.json"),
            secrets: home.join("secrets.json"),
            secrets_cache: home.join("secrets-cache.json"),
            home,
        }
    }

    /// No listener is bound at this path: a connection attempt would
    /// report `DaemonUnreachable`, not `Usage`, proving the name check
    /// runs first.
    #[tokio::test]
    async fn an_unknown_dog_name_is_usage_without_touching_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(
            dir.path(),
            shep_client::testing::control_address(dir.path()),
        );
        let code = run_dog("otel", paths).await;
        assert_eq!(code, ExitCode::Usage);
    }

    /// Proves dispatch reaches [`DogRuntime::start`], nothing about what
    /// `bark::run` does next: `serve_one_request`'s fake daemon closes the
    /// connection right after this one `DogConfig` reply.
    #[tokio::test]
    async fn run_dog_reaches_bark() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let response = Response::DogSection {
            toml: String::new().into(),
        };
        let handle = serve_one_request(&socket, sample_ack(), response).await;
        let paths = test_paths(dir.path(), socket);

        let task = tokio::spawn(run_dog("bark", paths));

        let envelope = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("run_dog must reach the wire")
            .unwrap();
        assert_eq!(
            envelope.body,
            Request::DogConfig {
                name: "bark".to_string()
            }
        );

        task.abort();
    }

    /// [`metrics::run`] blocks on a shutdown signal once it is up, so this
    /// spawns it, waits for the `DogConfig` request, then aborts rather
    /// than awaiting a return that never comes. The section answers
    /// `bind = "127.0.0.1:0"`, an OS-assigned port, never
    /// [`metrics::MetricsConfig::default`]'s fixed `9615`.
    #[tokio::test]
    async fn run_dog_reaches_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let response = Response::DogSection {
            toml: "bind = \"127.0.0.1:0\"\n".to_string().into(),
        };
        let handle = serve_one_request(&socket, sample_ack(), response).await;
        let paths = test_paths(dir.path(), socket);

        let task = tokio::spawn(run_dog("metrics", paths));

        let envelope = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("run_dog must reach the wire")
            .unwrap();
        assert_eq!(
            envelope.body,
            Request::DogConfig {
                name: "metrics".to_string()
            }
        );

        task.abort();
    }

    #[tokio::test]
    async fn run_dog_reports_daemon_unreachable_with_no_shepherd_running() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(
            dir.path(),
            shep_client::testing::control_address(dir.path()),
        );
        let code = run_dog("metrics", paths).await;
        assert_eq!(code, ExitCode::DaemonUnreachable);
    }
}
