//! `shep dog bark`: the webhook-alert dog.
//!
//! [`run`] reads `[bark]`, subscribes, and hands over to [`run_loop`],
//! which reads the shepherd's bus and polls the flock. [`rules`] decides
//! which events become a [`rules::Firing`], and [`sinks`] posts each one.
//! [`source`] holds the traits the loop reads the shepherd through.
//!
//! The bus is a `tokio::sync::broadcast`, so a lagging subscriber has
//! events dropped rather than queued, and load is when an alert matters
//! most. A dropped frame triggers an immediate poll.

mod clock;
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
use shep_client::dogs::Stop;

use super::DogRuntime;
use crate::exit::ExitCode;

/// The topics bark subscribes to: `process.*`, its own config topic, and the
/// log topics its rules read, if any.
///
/// Never `log.*`: a `log_match` rule on `err` alone must not make the
/// shepherd forward every stdout line of every sheep.
fn subscription_topics(dog: &str, rules: &rules::Rules) -> Vec<String> {
    let mut topics = vec!["process.*".to_owned(), format!("config.dog.{dog}")];
    topics.extend(rules.log_topics());
    topics
}

/// Runs the bark dog until it is signalled.
///
/// Parses `[bark]`, builds [`rules::Rules`] (or
/// [`rules::Rules::default_rules`] when the operator configured none),
/// subscribes on `process.*` and `config.dog.bark`, and hands both to
/// [`run_loop`] alongside a [`ClientShepherd`](source::ClientShepherd). A
/// refused config or a rejected rule set are both [`ExitCode::InvalidConfig`].
pub async fn run(runtime: DogRuntime) -> ExitCode {
    let config = match runtime.config::<BarkConfig>() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("shep dog bark: {err}; see `shep dogs`");
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
    let dog = runtime.identity().section().to_owned();
    // Named once, because `ClientEvents` asks for the same list again on
    // every handover and a second literal could drift from this one.
    let topics = subscription_topics(&dog, &rules);
    let barks_path = runtime.paths().barks.clone();
    let (events, shepherd) = match source::subscribe(runtime.into_client(), dog, topics).await {
        Ok(subscribed) => subscribed,
        Err(err) => {
            eprintln!("shep dog bark: could not subscribe to the shepherd's bus: {err}");
            return ExitCode::from(&err);
        }
    };
    run_loop(
        events,
        Arc::clone(&shepherd),
        rules,
        &config,
        &barks_path,
        shepherd,
        Stop::on_stop_signals(),
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

    /// What bark's opening `Subscribe` asks for, given `section` as its
    /// `[bark]` config.
    ///
    /// A handover fixture rather than `serve_one_request`: that one closes
    /// after its single reply, so the `Subscribe` that follows a dog's
    /// `DogConfig` is never read off the wire. This one keeps the
    /// connection open and records every envelope.
    async fn subscribed_topics(section: &str) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let daemon = fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
        daemon.reply_to_dog_config(section);
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
        task.abort();
        topics
    }

    /// A real sink: bark refuses to run without one. Port 1 is never
    /// dialled; no bark fires in these tests.
    const SINK: &str = "[sinks.ops]\nkind = \"json\"\nurl = \"http://127.0.0.1:1/hook\"\n";

    #[tokio::test]
    async fn bark_subscribes_to_its_own_config_topic() {
        let topics = subscribed_topics(SINK).await;

        assert!(
            topics.iter().any(|topic| topic == "config.dog.bark"),
            "bark must ask for its own config topic: {topics:?}"
        );
        assert!(
            topics.iter().any(|topic| topic == "process.*"),
            "the lifecycle topics every rule reads must survive: {topics:?}"
        );
        assert_eq!(
            topics,
            ["process.*", "config.dog.bark"],
            "with no log rule, nothing else"
        );
    }

    #[tokio::test]
    async fn a_stderr_log_rule_subscribes_to_log_err_alone() {
        let section = format!(
            "{SINK}[[rules]]\non = \"log_match\"\npattern = \"FATAL\"\nstream = \"err\"\nsinks = [\"ops\"]\n"
        );
        let topics = subscribed_topics(&section).await;

        assert!(topics.iter().any(|topic| topic == "log.err"), "{topics:?}");
        assert!(!topics.iter().any(|topic| topic == "log.out"), "{topics:?}");
        assert!(!topics.iter().any(|topic| topic == "log.*"), "{topics:?}");
    }

    fn rules_from(toml_rules: &str) -> super::rules::Rules {
        let config: super::BarkConfig = toml::from_str(&format!("{SINK}{toml_rules}")).unwrap();
        super::rules_for(&config).unwrap()
    }

    #[test]
    fn the_topics_name_a_log_stream_only_while_a_rule_reads_it() {
        let rule = |stream: &str| {
            format!(
                "[[rules]]\non = \"log_match\"\npattern = \"x\"\nstream = \"{stream}\"\nsinks = [\"ops\"]\n"
            )
        };
        let base = ["process.*", "config.dog.bark"];
        assert_eq!(super::subscription_topics("bark", &rules_from("")), base);
        assert_eq!(
            super::subscription_topics("bark", &rules_from(&rule("err"))),
            [base[0], base[1], "log.err"]
        );
        assert_eq!(
            super::subscription_topics("bark", &rules_from(&rule("out"))),
            [base[0], base[1], "log.out"]
        );
        assert_eq!(
            super::subscription_topics("bark", &rules_from(&rule("both"))),
            [base[0], base[1], "log.out", "log.err"]
        );
    }
}
