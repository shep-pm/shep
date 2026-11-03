//! A long action through a real shepherd: the callee answers the trigger
//! with an id and publishes the result later, and the caller helper hands
//! the caller the result carrying that id.
//!
//! Both dogs are the `long_action_dog` example, spawned by the shepherd
//! holding their own connections, so each is bound by the grants recorded
//! for it. The test only reads what each wrote under `$SHEP_HOME`.
//!
//! Real time throughout: the dogs are real processes on real sockets.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use shep_core::paths::ShepPaths;
use shep_core::permissions::Grants;
use shep_core::protocol::DogSource;
use shep_daemon::boot::{BootOptions, boot};
use shep_daemon::dogs::DogSpec;
use shep_daemon::rpc::RpcContext;
use shep_daemon::tokio_runner::TokioRunner;

/// Slack for a loaded runner, never an expected duration.
const WAIT: Duration = Duration::from_secs(30);

/// The example's binary, next to this test's own under `deps/`.
fn example() -> PathBuf {
    let test_binary = std::env::current_exe().expect("a running test knows its own path");
    let path = test_binary
        .parent()
        .and_then(Path::parent)
        .expect("a test binary lives at <profile>/deps/<name>")
        .join("examples")
        .join("long_action_dog");
    assert!(
        path.is_file(),
        "{} must exist: run `cargo build -p shep-client --examples`, or a plain \
         `cargo test -p shep-client`; `--test long_action` alone does not build it",
        path.display()
    );
    path
}

/// The callee: speaks the channel and declares the one kind it publishes.
fn callee() -> DogSpec {
    callee_named("callee")
}

fn callee_named(name: &str) -> DogSpec {
    spec(name, "callee", &[], true, true)
}

/// A caller holding `grants`.
fn caller(name: &str, grants: &[&str], params: &str) -> DogSpec {
    caller_of(name, "callee", grants, params)
}

/// A caller of every dog in `callees`, commas between names.
fn caller_of(name: &str, callees: &str, grants: &[&str], params: &str) -> DogSpec {
    let mut dog = spec(name, "caller", grants, false, false);
    dog.env
        .insert("LONG_ACTION_CALLEE".to_owned(), callees.to_owned());
    dog.env
        .insert("LONG_ACTION_PARAMS".to_owned(), params.to_owned());
    dog
}

fn spec(name: &str, role: &str, grants: &[&str], channel: bool, publishes: bool) -> DogSpec {
    DogSpec {
        name: name.to_owned(),
        source: DogSource::Adopted {
            path: example().display().to_string(),
            channel,
            grants: Some(Grants::parse(grants).expect("known permissions")),
            events: if publishes {
                vec![shep_client::dogs::DONE_KIND.to_owned()]
            } else {
                Vec::new()
            },
        },
        env: BTreeMap::from([("LONG_ACTION_ROLE".to_owned(), role.to_owned())]),
    }
}

/// A booted shepherd holding `dogs`, on a home of its own.
struct Shepherd {
    home: PathBuf,
    ctx: RpcContext,
    run: Option<tokio::task::JoinHandle<Result<(), shep_daemon::boot::BootError>>>,
    _dir: tempfile::TempDir,
}

impl Shepherd {
    async fn boot(dogs: Vec<DogSpec>) -> Self {
        // `$SHEP_HOME` is the tempdir root: `sun_path` is short on macOS.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let paths = ShepPaths::resolve(
            &|key| (key == "SHEP_HOME").then(|| home.display().to_string()),
            Path::new("/nonexistent"),
        );
        let daemon = tokio::time::timeout(
            WAIT,
            boot(
                TokioRunner::new(),
                paths,
                BootOptions {
                    dogs: dogs.clone(),
                    known_dogs: dogs,
                    ..BootOptions::default()
                },
            ),
        )
        .await
        .expect("boot must not hang")
        .expect("the shepherd must boot");
        let ctx = daemon.context();
        let run = Some(tokio::spawn(daemon.run()));
        Self {
            home,
            ctx,
            run,
            _dir: dir,
        }
    }

    /// Waits for `file` under the home and returns its text.
    async fn read(&self, file: &str) -> String {
        let path = self.home.join(file);
        tokio::time::timeout(WAIT, async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    return text;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{} never appeared", path.display()))
    }

    /// Lets the callers start, once the callee is answering triggers.
    async fn go(&self) {
        self.go_after(&["callee"]).await;
    }

    /// Lets the callers start, once each of `callees` is answering triggers.
    async fn go_after(&self, callees: &[&str]) {
        for callee in callees {
            self.read(&format!("{callee}.ready")).await;
        }
        std::fs::write(self.home.join("go"), "").unwrap();
    }

    async fn shutdown(mut self) {
        self.ctx.shutdown();
        let run = self.run.take().expect("taken once");
        tokio::time::timeout(WAIT, run)
            .await
            .expect("teardown must not hang")
            .unwrap()
            .unwrap();
    }
}

impl Drop for Shepherd {
    fn drop(&mut self) {
        if self.run.is_some() {
            self.ctx.shutdown();
        }
    }
}

const BOTH: &[&str] = &["dogs.trigger:callee", "events:dog.callee"];

#[tokio::test(flavor = "multi_thread")]
async fn a_caller_triggers_a_long_action_and_hears_the_result_carrying_its_id() {
    let shepherd = Shepherd::boot(vec![callee(), caller("caller", BOTH, "50:one")]).await;
    shepherd.go().await;

    let outcome = shepherd.read("caller.out").await;

    let mut parts = outcome.splitn(3, ' ');
    assert_eq!(parts.next(), Some("ok"), "{outcome}");
    let id = parts.next().unwrap();
    assert!(!id.is_empty(), "{outcome}");
    assert_eq!(parts.next(), Some(r#"{"built":"one"}"#), "{outcome}");
    shepherd.shutdown().await;
}

/// The callee publishes before its reply leaves it, so a caller that
/// subscribed after its trigger would never hear this result.
#[tokio::test(flavor = "multi_thread")]
async fn a_result_published_before_the_trigger_is_answered_still_reaches_the_caller() {
    let shepherd = Shepherd::boot(vec![callee(), caller("caller", BOTH, "early:0:raced")]).await;
    shepherd.go().await;

    let outcome = shepherd.read("caller.out").await;

    assert!(outcome.starts_with("ok "), "{outcome}");
    assert!(outcome.ends_with(r#"{"built":"raced"}"#), "{outcome}");
    shepherd.shutdown().await;
}

/// `slow` is triggered first and finishes last, so each caller sees the
/// other's result before its own.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_long_actions_each_deliver_their_own_result_to_their_own_caller() {
    let shepherd = Shepherd::boot(vec![
        callee(),
        caller("caller-slow", BOTH, "600:slow"),
        caller("caller-fast", BOTH, "0:fast"),
    ])
    .await;
    shepherd.go().await;

    let slow = shepherd.read("caller-slow.out").await;
    let fast = shepherd.read("caller-fast.out").await;

    assert!(slow.ends_with(r#"{"built":"slow"}"#), "{slow}");
    assert!(fast.ends_with(r#"{"built":"fast"}"#), "{fast}");
    let id = |line: &str| line.split(' ').nth(1).unwrap().to_owned();
    assert_ne!(id(&slow), id(&fast));
    shepherd.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_caller_without_the_events_grant_is_told_which_grant_it_lacks() {
    let shepherd = Shepherd::boot(vec![
        callee(),
        caller("caller", &["dogs.trigger:callee"], "0:never"),
    ])
    .await;
    shepherd.go().await;

    let outcome = shepherd.read("caller.out").await;

    assert!(outcome.starts_with("err "), "{outcome}");
    assert!(outcome.contains("`events:dog.callee`"), "{outcome}");
    shepherd.shutdown().await;
}

/// One connection holds one topic filter, so a second wait must add its
/// callee's topic to the first's rather than replace it.
#[tokio::test(flavor = "multi_thread")]
async fn one_caller_waiting_on_two_callees_at_once_hears_both() {
    let grants = [
        "dogs.trigger:callee",
        "events:dog.callee",
        "dogs.trigger:other",
        "events:dog.other",
    ];
    let shepherd = Shepherd::boot(vec![
        callee(),
        callee_named("other"),
        caller_of("caller", "callee,other", &grants, "600:both"),
    ])
    .await;
    shepherd.go_after(&["callee", "other"]).await;

    let outcome = shepherd.read("caller.out").await;

    let lines: Vec<&str> = outcome.lines().collect();
    assert_eq!(lines.len(), 2, "{outcome}");
    for line in &lines {
        assert!(line.starts_with("ok "), "{outcome}");
        assert!(line.ends_with(r#"{"built":"both"}"#), "{outcome}");
    }
    shepherd.shutdown().await;
}
