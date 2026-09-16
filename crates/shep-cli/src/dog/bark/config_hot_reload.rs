use super::config::{BarkConfig, rules_for};
use super::firing_delivery::{Delivery, now_ms, spawn_firings};
use super::rules::Rules;
use core::future::Future;
use shep_client::RequestError;
use shep_core::protocol::ProcessInfo;

/// What bark reads the flock through, so the loop's poll is drivable
/// without a socket.
///
/// `Sync`, not just `Send`: [`run_loop`](crate::dog::bark::dog_lifecycle::run_loop)'s future holds `&F` across the
/// `.await` in [`reconcile`], so both its lag arm and its interval arm
/// poll the same source without moving it.
pub trait FlockSource: Send + Sync {
    /// The flock as it stands.
    ///
    /// # Errors
    /// Whatever the source failed with: in production, whatever
    /// `Request::ListFlock` failed with.
    fn flock(&self) -> impl Future<Output = Result<Vec<ProcessInfo>, RequestError>> + Send;
}

/// What bark re-asks its own `[bark]` section through, so a config change
/// is drivable without a socket.
///
/// `BusEvent::DogConfigChanged` says nothing about what changed, so the
/// frame is only a prompt: the answer is one `Request::DogConfig`.
///
/// `Sync` for the reason [`FlockSource`] is: [`run_loop`](crate::dog::bark::dog_lifecycle::run_loop)'s future holds
/// a `&C` across the `.await` in [`reloaded_config`].
pub trait ConfigSource: Send + Sync {
    /// This dog's `[bark]` section as it stands now, empty when the file
    /// has no such section.
    ///
    /// # Errors
    /// Whatever the source failed with: in production, whatever
    /// `Request::DogConfig` failed with.
    fn section(&self) -> impl Future<Output = Result<String, RequestError>> + Send;
}

/// Re-asks `source` for `[bark]` and rebuilds what a config change can
/// swap, or `None` when the answer cannot be used.
///
/// Every failure is reported and dropped, never propagated: a dog that
/// exited on a bad edit would stop alerting while config is being edited.
/// stderr gets the fact, never the section: `[bark]` carries webhook URLs
/// that are bearer credentials.
pub(super) async fn reloaded_config<C: ConfigSource>(source: &C) -> Option<(BarkConfig, Rules)> {
    let section = match source.section().await {
        Ok(section) => section,
        Err(err) => {
            eprintln!("shep dog bark: could not re-read [bark] from the shepherd: {err}");
            return None;
        }
    };
    // Empty means the section is gone. The default no-sink rule is
    // rejected, so bark keeps the current configuration.
    let config = if section.is_empty() {
        BarkConfig::default()
    } else {
        match toml::from_str::<BarkConfig>(&section) {
            Ok(config) => config,
            Err(_err) => {
                eprintln!("shep dog bark: [bark] in dogs.toml does not parse; see `shep dogs`");
                return None;
            }
        }
    };
    match rules_for(&config) {
        Ok(rules) => Some((config, rules)),
        Err(err) => {
            eprintln!("shep dog bark: keeping the running rules; the new ones are refused: {err}");
            None
        }
    }
}

/// One reconciliation pass: ask `flock` what the flock looks like now, run
/// it through `super::rules::on_poll`, and spawn a delivery for anything that
/// fires. Shared by [`run_loop`](crate::dog::bark::dog_lifecycle::run_loop)'s lag arm and interval arm, so the two
/// polls are one code path.
///
/// A failed poll is logged and dropped: the next bus event or interval
/// tick tries again.
pub(super) async fn reconcile<F: FlockSource>(flock: &F, rules: &mut Rules, delivery: &Delivery) {
    match flock.flock().await {
        Ok(snapshot) => {
            let firings = rules.on_poll(&snapshot, now_ms());
            spawn_firings(firings, delivery);
        }
        Err(err) => eprintln!("shep dog bark: reconciliation poll failed: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::config::BarkConfig;
    use super::super::dog_lifecycle::run_loop;

    use std::collections::BTreeMap;

    use std::time::Duration;

    use shep_client::LinkLost;
    use shep_core::barks::{self};

    use shep_core::values::UpDuration;

    use super::super::rules::Rules;

    use shep_core::protocol::ProcessEventKind;

    use tokio::sync::broadcast;

    use super::super::testing::*;

    /// Fails if the poll is only ever driven by its interval: `web`'s
    /// `errored` frame is genuinely dropped by a real broadcast channel,
    /// and the fixture's interval is 60s.
    ///
    /// A real clock, not `start_paused`: a deadline awaited through a
    /// `spawn_blocking` bridge cannot elapse under a paused one, so a
    /// regression hangs the suite instead of failing it.
    #[tokio::test]
    async fn a_dropped_frame_makes_bark_poll_and_catch_up() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(4);
        for i in 0..64 {
            tx.send(log_event(i)).unwrap();
        }
        tx.send(errored_event("web")).unwrap();

        // The drop is real, or this test proves nothing.
        assert!(
            matches!(rx.recv().await, Err(broadcast::error::RecvError::Lagged(n)) if n > 0),
            "the fixture must actually overflow the channel"
        );

        let (tx2, rx2) = tokio::sync::broadcast::channel(4);
        for i in 0..64 {
            tx2.send(log_event(i)).unwrap();
        }
        tx2.send(errored_event("web")).unwrap();

        let (addr, captured) = one_shot_sink(200, "").await;
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");
        let flock = ScriptedFlock::answering(vec![errored_info("web", 16)]);

        let loop_handle = tokio::spawn(run_loop(
            rx2,
            flock.clone(),
            gave_up_rules(),
            &config_with_sink(addr, &barks_path),
            &barks_path,
            // Never asked: no `config.dog.bark` frame is sent here.
            ScriptedConfig::answering(String::new()),
        ));

        let req = tokio::time::timeout(Duration::from_secs(5), captured)
            .await
            .expect("a dropped frame must produce a delivered bark")
            .unwrap();
        assert!(String::from_utf8_lossy(&req.body).contains("web"));

        // `captured` resolves when the sink server finishes writing its
        // response, concurrently with the delivery task's own tail
        // (outcome, append). A bounded poll covers that gap.
        let recorded = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let records = shep_core::barks::read(&barks_path).unwrap();
                if !records.is_empty() {
                    break records;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the delivered bark must be recorded promptly after delivery");
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].subject, "web");
        assert_eq!(recorded[0].sinks[0].error, None);

        assert_eq!(
            flock.calls(),
            1,
            "the poll ran because of the lag, not because an interval elapsed: \
                 the interval is 60s and this test is milliseconds old"
        );

        loop_handle.abort();
    }

    /// fails if a handover restarts the bark dog, which is what it used to
    /// do: the stream ends with the connection, and a loop that took that
    /// for its shepherd going away exited 0 once per `shep daemon reload`.
    /// The restart is not free. `restarts` is the one column an operator
    /// reads to judge a dog's health, and the dog drops every rule's
    /// per-subject debounce on the way out, so a sheep already alerted on
    /// can be alerted on again.
    #[tokio::test]
    async fn a_handover_re_subscribes_instead_of_ending_the_dog() {
        let (first_tx, first_rx) = tokio::sync::broadcast::channel(8);
        let (second_tx, second_rx) = tokio::sync::broadcast::channel(8);
        let (source, resubscribes) = HandoverSource::across(
            vec![first_rx, second_rx],
            LinkLost::Budget {
                waited: Duration::ZERO,
            },
        );

        let (addr, captured) = one_shot_sink(200, "").await;
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");
        // Empty on purpose. A re-subscribe reconciles, so a flock with
        // anything firing in it delivers a bark from that reconcile and
        // satisfies `captured` whether or not the second generation was
        // ever armed. The bark below has to come from the stream.
        let flock = ScriptedFlock::answering(Vec::new());

        let loop_handle = tokio::spawn(run_loop(
            source,
            flock.clone(),
            gave_up_rules(),
            &config_with_sink(addr, &barks_path),
            &barks_path,
            ScriptedConfig::answering(String::new()),
        ));

        // The handover: the first generation's sender goes, exactly as an
        // `execve` takes the accepted connection with it.
        drop(first_tx);

        // An event only the SECOND generation could have carried. A loop
        // that ended on the handover never sees this one.
        let delivered = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                // Sent repeatedly: the second generation is armed inside
                // the loop, and a broadcast sent before anyone subscribed
                // reaches nobody.
                let _ = second_tx.send(errored_event("web"));
                tokio::time::sleep(Duration::from_millis(20)).await;
                if loop_handle.is_finished() {
                    panic!("the loop ended on a handover instead of re-subscribing");
                }
            }
        });
        let captured = tokio::time::timeout(Duration::from_secs(5), captured);
        let request = tokio::select! {
            // Reached only when the timeout above expires, which is this
            // test failing rather than an impossible state: the sender
            // loop itself never returns.
            () = async { delivered.await.ok(); } => panic!(
                "no bark was delivered within 5s of the handover, so the \
                 second generation never reached the loop"
            ),
            request = captured => request.expect("a bark must be delivered after the handover"),
        };
        assert!(String::from_utf8_lossy(&request.unwrap().body).contains("web"));

        assert_eq!(
            resubscribes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the handover must have been answered by exactly one re-subscribe"
        );
        assert!(
            flock.calls() >= 1,
            "a re-subscribe must reconcile: frames sent while there was no \
                 subscription are gone, and only the shepherd knows the flock now"
        );
        assert!(
            !loop_handle.is_finished(),
            "the dog must still be running after a handover"
        );
        loop_handle.abort();
    }

    /// Fails if a slow sink stalls the loop: a bark dog that stops reading
    /// the bus while it waits drops the frames it exists to catch.
    ///
    /// The proof is an order, not a duration. The slow sink signals that
    /// it has accepted and parked, and the fast sink is reached after
    /// that; an inline-awaiting loop would still be parked. `sink_timeout`
    /// is ten minutes and both timeouts are failure guards, so their
    /// values change how long a broken loop takes to report, never what
    /// passes.
    #[tokio::test]
    async fn a_slow_sink_never_stalls_the_loop() {
        let (slow_addr, slow_connected) = slow_sink().await;
        let (fast_addr, fast_captured) = one_shot_sink(200, "").await;
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");

        let mut sinks = BTreeMap::new();
        sinks.insert(
            "slow".to_owned(),
            json_sink(format!("http://{slow_addr}/hook")),
        );
        sinks.insert(
            "fast".to_owned(),
            json_sink(format!("http://{fast_addr}/hook")),
        );
        let rules = Rules::new(
            vec![
                super::super::rules::Rule {
                    when: super::super::rules::Trigger::GaveUp {},
                    sinks: vec!["slow".to_owned()],
                    debounce: UpDuration::from_millis(0),
                },
                super::super::rules::Rule {
                    when: super::super::rules::Trigger::Event {
                        kinds: vec!["online".to_owned()],
                    },
                    sinks: vec!["fast".to_owned()],
                    debounce: UpDuration::from_millis(0),
                },
            ],
            &sinks,
        )
        .unwrap();
        let config = BarkConfig {
            sinks,
            rules: Vec::new(),
            poll: UpDuration::from_millis(60_000),
            history_bytes: barks::DEFAULT_MAX_BYTES,
            sink_timeout: UpDuration::from_millis(600_000),
        };

        let (tx, rx) = tokio::sync::broadcast::channel(8);
        tx.send(errored_event("web")).unwrap();
        tx.send(process_event("api", ProcessEventKind::Online))
            .unwrap();

        let flock = ScriptedFlock::answering(Vec::new());
        let loop_handle = tokio::spawn(run_loop(
            rx,
            flock,
            rules,
            &config,
            &barks_path,
            ScriptedConfig::answering(String::new()),
        ));

        // The order is the assertion: the slow sink has a connection and
        // is parked on it, so the loop is provably mid-delivery.
        tokio::time::timeout(Duration::from_secs(30), slow_connected)
            .await
            .expect("the slow sink must be reached at all, or this test proves nothing")
            .unwrap();

        // Then the fast sink is reached anyway. A loop awaiting firings
        // inline would still be parked on the delivery above.
        let req = tokio::time::timeout(Duration::from_secs(30), fast_captured)
            .await
            .expect(
                "the fast sink must be reached while a slow sink is still in \
                     flight; a slow sink must not stall the loop",
            )
            .unwrap();
        assert_eq!(req.method, "POST");

        loop_handle.abort();
    }
}
