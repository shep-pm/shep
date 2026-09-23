//! [`run_loop`], bark's own loop, and the passes it runs: [`reconcile`] on
//! a poll or a dropped frame, [`reloaded_config`] on a config change.

use core::future::Future;
use std::path::Path;
use std::time::Duration;

use shep_core::protocol::BusEvent;
use tokio::time::MissedTickBehavior;

use super::config::{BarkConfig, rules_for};
use super::delivery::{Delivery, spawn_firings};
use super::rules::Rules;
use super::source::{ConfigSource, EventSource, FlockSource, Resubscribe};
use crate::dog::runtime::parse_section;
use crate::exit::ExitCode;

/// Bark's loop: subscribe for speed, poll for correctness. Ends on
/// `SIGINT`/`SIGTERM` or when `events` does.
///
/// A dropped frame polls immediately, since the bus drops what a lagging
/// subscriber cannot keep up with. Firings are spawned, never awaited
/// inline, or a slow sink causes the drop this loop exists to catch.
/// Appends serialize behind an in-process [`tokio::sync::Mutex`]; the
/// cross-process race is `barks::append`'s own `flock(2)`.
///
/// A plain `fn`, not `async fn`: the returned future must borrow neither
/// `config` nor `barks_path`, so callers can spawn it.
pub fn run_loop<E: EventSource, F: FlockSource, C: ConfigSource>(
    events: E,
    flock: F,
    rules: Rules,
    config: &BarkConfig,
    barks_path: &Path,
    config_source: C,
) -> impl Future<Output = ExitCode> + Send + use<E, F, C> {
    let mut delivery = Delivery::new(config, barks_path);
    let mut poll_period = config.poll.as_duration();

    async move {
        let mut events = events;
        let mut rules = rules;

        let mut sigterm = match crate::shutdown::Terminate::install() {
            Ok(sigterm) => sigterm,
            Err(err) => {
                eprintln!("shep dog bark: could not install a shutdown handler: {err}");
                return ExitCode::Failure;
            }
        };

        let mut poll_interval = poll_timer(poll_period);

        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break ExitCode::Success,
                _ = sigterm.recv() => break ExitCode::Success,
                next = events.next() => {
                    match next {
                        // One connection generation ended. Usually the
                        // shepherd exec'd a successor, which is not the
                        // shepherd going away: `resubscribe` waits a
                        // bounded time for one to answer, and only a
                        // shepherd that never does ends this dog.
                        None => match events.resubscribe().await {
                            Ok(()) => {
                                // Reconcile whichever daemon answered.
                                // Frames sent with no subscription are
                                // gone, and the verdict decides nothing
                                // here: bark debounces on a sheep's name,
                                // never on an id the shepherd minted.
                                reconcile(&flock, &mut rules, &delivery).await;
                            }
                            Err(failed) => {
                                eprintln!("shep dog bark: {failed}");
                                break match &failed {
                                    Resubscribe::Lost(lost) => crate::dog::exit_for(lost),
                                    Resubscribe::Request(err) => ExitCode::from(err),
                                };
                            }
                        },
                        // Matched on the variant rather than on the dog's
                        // name: the subscription already narrows this to
                        // bark's own topic, `config.dog.<name>`.
                        Some(Ok(BusEvent::DogConfigChanged { .. })) => {
                            if let Some((next, next_rules)) = reloaded_config(&config_source).await {
                                // In place, never a restart: sinks and
                                // rules are pure data with no OS resource
                                // to rebind.
                                delivery.reconfigure(&next);
                                // Rebuilt, which resets each rule's
                                // per-subject debounce: carrying it over
                                // a renumbered rule set would key state on
                                // an index that moved.
                                rules = next_rules;
                                if next.poll.as_duration() != poll_period {
                                    poll_period = next.poll.as_duration();
                                    poll_interval = poll_timer(poll_period);
                                }
                                eprintln!("shep dog bark: reloaded [bark] from dogs.toml");
                            }
                        }
                        Some(Ok(event)) => {
                            let firings = rules.on_event(&event, now_ms());
                            spawn_firings(firings, &delivery);
                        }
                        Some(Err(_dropped)) => {
                            // The drop says nothing about what was lost;
                            // only the shepherd can.
                            reconcile(&flock, &mut rules, &delivery).await;
                        }
                    }
                }
                _ = poll_interval.tick() => {
                    reconcile(&flock, &mut rules, &delivery).await;
                }
            }
        }
    }
}

/// One reconciliation pass: ask `flock` what the flock looks like now, run
/// it through [`Rules::on_poll`], and spawn a delivery for anything that
/// fires. Shared by [`run_loop`]'s lag arm and interval arm, so the two
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
    let config = match parse_section::<BarkConfig>(&section) {
        Ok(config) => config,
        Err(_err) => {
            eprintln!("shep dog bark: [bark] in dogs.toml does not parse; see `shep dogs`");
            return None;
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

/// The poll timer for `period`.
///
/// `interval_at`, not `interval`: a plain `interval` fires its first tick
/// immediately, so the first poll would be attributable to the timer's
/// startup rather than to a drop or an elapsed interval.
///
/// One function, not two call sites: a reload that rebuilt the timer and
/// forgot `MissedTickBehavior::Delay` would leave a poll that ran long
/// firing a burst of catch-up ticks.
fn poll_timer(period: Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    interval
}

/// Wall-clock milliseconds since the Unix epoch.
///
/// [`Rules::on_event`] and [`Rules::on_poll`] take a caller-supplied
/// timestamp so a test can fix it; this is the production caller.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use shep_client::LinkLost;
    use shep_core::barks;
    use shep_core::protocol::{ProcessEventKind, RpcErrorCode};
    use shep_core::values::UpDuration;
    use tokio::sync::broadcast;

    use super::*;
    use crate::dog::bark::testing::*;

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

    /// fails if a dog survives one handover and not the next.
    ///
    /// The single-handover test proves the arm runs once. A reload is not a
    /// one-off, and a rolling restart is several in a row, so the property
    /// that matters is that the second and third cost no more than the
    /// first. A `resubscribe` that double-advanced its queue or left the
    /// previous generation in place would pass with one handover.
    #[tokio::test]
    async fn a_dog_rides_out_three_shepherds_in_a_row() {
        let (first_tx, first_rx) = tokio::sync::broadcast::channel(8);
        let (second_tx, second_rx) = tokio::sync::broadcast::channel(8);
        let (third_tx, third_rx) = tokio::sync::broadcast::channel(8);
        let (source, resubscribes) = HandoverSource::across(
            vec![first_rx, second_rx, third_rx],
            LinkLost::Budget {
                waited: Duration::ZERO,
            },
        );

        let (addr, captured) = one_shot_sink(200, "").await;
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");
        // Empty on purpose. A re-subscribe reconciles, and a flock with
        // anything firing in it would deliver a bark from that reconcile,
        // which satisfies `captured` without the third generation ever
        // being read. The only bark this test can produce has to come from
        // the stream.
        let flock = ScriptedFlock::answering(Vec::new());

        let loop_handle = tokio::spawn(run_loop(
            source,
            flock.clone(),
            gave_up_rules(),
            &config_with_sink(addr, &barks_path),
            &barks_path,
            ScriptedConfig::answering(String::new()),
        ));

        // Two handovers back to back, each ending a generation the dog is
        // already on.
        drop(first_tx);
        let two = tokio::time::timeout(Duration::from_secs(5), async {
            while resubscribes.load(std::sync::atomic::Ordering::SeqCst) < 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            drop(second_tx);
            while resubscribes.load(std::sync::atomic::Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(two.is_ok(), "both handovers must be answered within 5s");

        // An event only the THIRD generation could carry.
        let delivered = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let _ = third_tx.send(errored_event("web"));
                tokio::time::sleep(Duration::from_millis(20)).await;
                assert!(
                    !loop_handle.is_finished(),
                    "the dog ended on one of the two handovers"
                );
            }
        });
        let captured = tokio::time::timeout(Duration::from_secs(5), captured);
        let request = tokio::select! {
            () = async { delivered.await.ok(); } => panic!(
                "no bark was delivered on the third generation within 5s"
            ),
            request = captured => request.expect("a bark must be delivered after two handovers"),
        };
        assert!(String::from_utf8_lossy(&request.unwrap().body).contains("web"));

        assert_eq!(
            resubscribes.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "two handovers, two re-subscribes, no more"
        );
        loop_handle.abort();
    }

    /// fails if a dog whose shepherd is gone for good lingers. A lingering
    /// dog attaches itself to whatever shepherd next binds that socket,
    /// beside that shepherd's own dog of the same kind, and doubles its
    /// alerts quietly.
    #[tokio::test]
    async fn a_shepherd_that_never_comes_back_ends_the_dog() {
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        let (source, resubscribes) = HandoverSource::across(
            vec![rx],
            LinkLost::Budget {
                waited: Duration::from_secs(5),
            },
        );

        let (addr, _captured) = one_shot_sink(200, "").await;
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");

        let loop_handle = tokio::spawn(run_loop(
            source,
            ScriptedFlock::answering(Vec::new()),
            gave_up_rules(),
            &config_with_sink(addr, &barks_path),
            &barks_path,
            ScriptedConfig::answering(String::new()),
        ));

        drop(tx);

        let code = tokio::time::timeout(Duration::from_secs(5), loop_handle)
            .await
            .expect("a dog whose shepherd is gone must exit, not linger")
            .unwrap();
        assert_eq!(
            code,
            ExitCode::DaemonUnreachable,
            "exiting 0 would read as a dog that finished its work"
        );
        assert_eq!(
            resubscribes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "it must have tried once before giving up"
        );
    }

    /// fails if a shepherd that answers and refuses the subscription is
    /// reported as a shepherd that never answered.
    ///
    /// `bark::run` exits `ExitCode::from(&err)` when the opening `Subscribe`
    /// is refused. A re-subscribe meeting the same refusal has to exit the
    /// same code, or the same fault reports differently depending on
    /// whether a handover happened to have run first.
    #[tokio::test]
    async fn a_refused_re_subscribe_exits_on_the_refusals_own_code() {
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        let (source, _resubscribes) = HandoverSource::refusing(vec![rx], RpcErrorCode::Unsupported);

        let (addr, _captured) = one_shot_sink(200, "").await;
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");

        let loop_handle = tokio::spawn(run_loop(
            source,
            ScriptedFlock::answering(Vec::new()),
            gave_up_rules(),
            &config_with_sink(addr, &barks_path),
            &barks_path,
            ScriptedConfig::answering(String::new()),
        ));

        drop(tx);

        let code = tokio::time::timeout(Duration::from_secs(5), loop_handle)
            .await
            .expect("a refused subscription must end the dog, not be waited out")
            .unwrap();
        assert_eq!(
            code,
            ExitCode::Unsupported,
            "exiting DaemonUnreachable would name a shepherd that answered"
        );
    }

    /// fails if a dog that cannot speak the shepherd's protocol exits as
    /// though nothing answered. The shepherd that refused is running and
    /// answering, and an operator told it was unreachable goes looking for
    /// the wrong thing.
    #[tokio::test]
    async fn a_refused_dog_exits_saying_it_was_refused() {
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        let (source, _resubscribes) = HandoverSource::across(
            vec![rx],
            LinkLost::Refused {
                daemon_version: Some("0.9.9".into()),
                message: "daemon speaks protocol 9, client speaks 8".into(),
            },
        );

        let (addr, _captured) = one_shot_sink(200, "").await;
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");

        let loop_handle = tokio::spawn(run_loop(
            source,
            ScriptedFlock::answering(Vec::new()),
            gave_up_rules(),
            &config_with_sink(addr, &barks_path),
            &barks_path,
            ScriptedConfig::answering(String::new()),
        ));

        drop(tx);

        let code = tokio::time::timeout(Duration::from_secs(5), loop_handle)
            .await
            .expect("a refused dog must exit rather than retry")
            .unwrap();
        assert_eq!(code, ExitCode::ProtocolMismatch);
    }

    /// Fails if a `config.dog.bark` frame does not reach bark's sinks: the
    /// next firing is delivered to the sink the new section names, over a
    /// real socket, while the old sink's server is still up.
    ///
    /// The loop is the same loop throughout, which is the other half of
    /// what this pins: bark's config is pure data with no OS resource
    /// attached, so it swaps in place instead of restarting.
    ///
    /// A real clock, not `start_paused`, or a broken swap hangs the suite
    /// rather than failing it.
    #[tokio::test]
    async fn a_config_change_swaps_barks_sinks_in_place() {
        let (old_addr, mut old_captured) = one_shot_sink(200, "").await;
        let (new_addr, new_captured) = one_shot_sink(200, "").await;
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");
        let source = ScriptedConfig::answering(format!(
            "[sinks.ops]\nkind = \"json\"\nurl = \"http://{new_addr}/hook\"\n"
        ));

        let (tx, rx) = broadcast::channel(16);
        let loop_handle = tokio::spawn(run_loop(
            rx,
            ScriptedFlock::answering(Vec::new()),
            gave_up_rules(),
            &config_with_sink(old_addr, &barks_path),
            &barks_path,
            source.clone(),
        ));

        // One receiver, one queue: the loop awaits the re-ask before it
        // takes the second event, so the delivery below is attributable
        // to the new section.
        tx.send(BusEvent::DogConfigChanged {
            dog: "bark".to_owned(),
        })
        .unwrap();
        tx.send(errored_event("web")).unwrap();

        let req = tokio::time::timeout(Duration::from_secs(5), new_captured)
            .await
            .expect("the bark must reach the sink the new section names")
            .unwrap();
        assert!(String::from_utf8_lossy(&req.body).contains("web"));

        assert_eq!(source.calls(), 1, "one frame, one re-ask");
        assert!(
            !loop_handle.is_finished(),
            "bark swaps its config in place; it does not exit to pick one up"
        );
        // `try_recv`, never an await: the old sink's server is still
        // parked in `accept`, so it holds its sender alive and an await
        // would never return. The delivery above has already happened.
        assert!(
            old_captured.try_recv().is_err(),
            "the sink the old section named must be left alone"
        );

        loop_handle.abort();
    }

    /// An emptied section parses to no sinks at all, which `Rules::new`
    /// refuses, so the running sinks must stay.
    #[tokio::test]
    async fn a_config_change_that_empties_the_section_keeps_the_running_sinks() {
        let (addr, captured) = one_shot_sink(200, "").await;
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");
        let source = ScriptedConfig::answering(String::new());

        let (tx, rx) = broadcast::channel(16);
        let loop_handle = tokio::spawn(run_loop(
            rx,
            ScriptedFlock::answering(Vec::new()),
            gave_up_rules(),
            &config_with_sink(addr, &barks_path),
            &barks_path,
            source.clone(),
        ));

        tx.send(BusEvent::DogConfigChanged {
            dog: "bark".to_owned(),
        })
        .unwrap();
        tx.send(errored_event("web")).unwrap();

        let req = tokio::time::timeout(Duration::from_secs(5), captured)
            .await
            .expect("the running sink must still get the bark")
            .unwrap();
        assert!(String::from_utf8_lossy(&req.body).contains("web"));
        assert_eq!(source.calls(), 1, "the frame must still be re-asked");

        loop_handle.abort();
    }
}
