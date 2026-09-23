//! `shep dog bark`: the webhook-alert dog.
//!
//! [`run`] reads `[bark]`, subscribes, and hands over to [`run_loop`],
//! which reads the shepherd's bus and polls the flock. [`rules`] decides
//! which events and poll snapshots become a [`rules::Firing`], and
//! [`sinks`] renders and posts each one. [`source`] holds the traits the
//! loop reads the shepherd through, and their implementations over a real
//! client.
//!
//! The bus is a `tokio::sync::broadcast`, so a lagging subscriber has
//! events dropped rather than queued, and load is when an alert matters
//! most. A dropped frame triggers an immediate poll, and [`rules::Rules`]'s
//! per-subject debounce is what lets an `Errored` seen by both routes fire
//! once.

mod config;
mod delivery;
mod event_loop;
pub mod rules;
pub mod sinks;
mod source;
#[cfg(test)]
mod testing;

pub use config::BarkConfig;

use std::sync::Arc;

use config::rules_for;
use event_loop::run_loop;

use super::DogRuntime;
use crate::exit::ExitCode;

/// Runs the bark dog until it is signalled.
///
/// Parses `[dog.bark]`, builds [`rules::Rules`] (or
/// [`rules::Rules::default_rules`] when the operator configured
/// none), subscribes to the shepherd's bus on `process.*` and
/// `config.dog.bark`, and hands both to [`run_loop`] alongside a
/// [`ClientShepherd`](source::ClientShepherd) wrapping this same connection.
///
/// A refused config or a rule set `Rules::new` rejects are both
/// [`ExitCode::InvalidConfig`].
pub async fn run(runtime: DogRuntime) -> ExitCode {
    let config = match runtime.config::<BarkConfig>() {
        Ok(config) => config,
        Err(_err) => {
            // The fact, not the value: a `[bark]` section can carry a
            // webhook URL with a bearer token in its path.
            eprintln!("shep dog bark: [bark] in dogs.toml does not parse; see `shep dogs`");
            return ExitCode::InvalidConfig;
        }
    };
    let rules = match rules_for(&config) {
        Ok(rules) => rules,
        Err(err) => {
            eprintln!("shep dog bark: {err}");
            return ExitCode::InvalidConfig;
        }
    };
    // Subscribes to this dog's own `config.dog.<name>` topic, not
    // `config.*`, which would hand it every other dog's prompts too. `dog`
    // is reused below for `ClientShepherd`'s re-read request, so the two
    // cannot drift apart.
    let dog = runtime.name.clone();
    // Named once, because `ClientEvents` asks for the same list again on
    // every handover and a second literal could drift from this one.
    let topics = vec!["process.*".to_owned(), format!("config.dog.{dog}")];
    let (events, shepherd) = match source::subscribe(runtime.client, dog, topics).await {
        Ok(subscribed) => subscribed,
        Err(err) => {
            eprintln!("shep dog bark: could not subscribe to the shepherd's bus: {err}");
            return ExitCode::from(&err);
        }
    };
    let barks_path = runtime.paths.barks;
    run_loop(
        events,
        Arc::clone(&shepherd),
        rules,
        &config,
        &barks_path,
        shepherd,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shep_client::testing::{Handshake, fake_daemon_across_handovers, sample_ack};
    use shep_core::protocol::Request;

    use crate::dog::run_dog;
    use crate::dog::tests::test_paths;

    /// A handover fixture rather than `serve_one_request`: that one closes
    /// after its single reply, so the `Subscribe` that follows a dog's
    /// `DogConfig` is never read off the wire. This one keeps the
    /// connection open and records every envelope.
    #[tokio::test]
    async fn bark_subscribes_to_its_own_config_topic() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let daemon = fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
        // A real sink: bark refuses to run without one. Port 1 is never
        // dialled; no bark fires in this test.
        daemon.reply_to_dog_config(
            "[sinks.ops]\nkind = \"json\"\nurl = \"http://127.0.0.1:1/hook\"\n",
        );
        let paths = test_paths(dir.path(), socket);

        let task = tokio::spawn(run_dog("bark", paths));

        // Polled rather than slept on, and bounded: the fixture records
        // envelopes as it reads them, so the test yields until the
        // subscribe arrives and fails with its own message if it never
        // does.
        let topics =
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let subscribed = daemon.envelopes().into_iter().find_map(|(_, envelope)| {
                        match envelope.body {
                            Request::Subscribe { topics } => Some(topics),
                            _ => None,
                        }
                    });
                    if let Some(topics) = subscribed {
                        break topics;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("bark must subscribe once its config parses");

        assert!(
            topics.iter().any(|topic| topic == "config.dog.bark"),
            "bark must ask for its own config topic: {topics:?}"
        );
        assert!(
            topics.iter().any(|topic| topic == "process.*"),
            "the lifecycle topics every rule reads must survive: {topics:?}"
        );

        task.abort();
    }
}
