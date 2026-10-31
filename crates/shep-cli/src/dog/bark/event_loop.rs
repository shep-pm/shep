//! [`run_loop`], bark's own loop, and the passes it runs: [`reconcile`] on
//! a poll or a dropped frame, [`reloaded_config`] on a config change.

use core::future::Future;
use std::path::Path;
use std::time::Duration;

use shep_client::dogs::{Stop, parse_section};
use shep_core::protocol::BusEvent;
use tokio::time::{Instant, MissedTickBehavior};

use super::clock::Clock;
use super::config::{BarkConfig, rules_for};
use super::delivery::{Delivery, spawn_firings};
use super::rules::Rules;
use super::source::{ConfigSource, EventSource, FlockSource, Resubscribe};
use crate::exit::ExitCode;

/// The least time between a poll and the next one a dropped frame asks for.
///
/// A poll awaits a `ListFlock` round trip inline, so frames queue behind it
/// and a busy bus can lap the subscriber again at once. Lags inside the gap
/// share one deferred poll.
const LAG_POLL_MIN_GAP: Duration = Duration::from_secs(1);

/// When bark last polled, and whether a dropped frame is waiting on the
/// next one.
#[derive(Debug, Default)]
struct LagPacing {
    last_poll: Option<Instant>,
    lag_pending: bool,
}

impl LagPacing {
    /// Records a poll, which answers every lag before it.
    fn polled(&mut self) {
        self.last_poll = Some(Instant::now());
        self.lag_pending = false;
    }

    /// Whether a lag now polls at once. When it does not, the lag is held
    /// for [`Self::deferred_poll_at`].
    fn lag_polls_now(&mut self) -> bool {
        match self.last_poll {
            Some(last) if last.elapsed() < LAG_POLL_MIN_GAP => {
                self.lag_pending = true;
                false
            }
            _ => true,
        }
    }

    /// When the held lag's poll is due, or `None` with no lag held.
    fn deferred_poll_at(&self) -> Option<Instant> {
        self.last_poll
            .filter(|_| self.lag_pending)
            .map(|last| last + LAG_POLL_MIN_GAP)
    }
}

/// Bark's loop: subscribe for speed, poll for correctness. Ends on
/// `stop`, or when `events` ends and its re-subscribe fails.
///
/// A dropped frame polls immediately unless the last poll was under
/// [`LAG_POLL_MIN_GAP`] ago. Firings are spawned, never awaited inline, or
/// a slow sink causes the drop this loop exists to catch. A plain `fn`, not
/// `async fn`: the returned future borrows neither `config` nor `barks_path`.
pub fn run_loop<E: EventSource, F: FlockSource, C: ConfigSource>(
    events: E,
    flock: F,
    rules: Rules,
    config: &BarkConfig,
    barks_path: &Path,
    config_source: C,
    stop: Stop,
) -> impl Future<Output = ExitCode> + Send + use<E, F, C> {
    let mut delivery = Delivery::new(config, barks_path);
    let mut poll_period = config.poll.as_duration();

    async move {
        let mut events = events;
        let mut rules = rules;
        let mut stop = stop;
        let mut clock = Clock::system();

        let mut poll_interval = poll_timer(poll_period);
        let mut pacing = LagPacing::default();

        // Log lines name a sheep by id, so the names are read once up
        // front. Not a reconcile: that would fire `gave_up` for a sheep
        // already errored earlier than it fires without a log rule.
        if !rules.log_topics().is_empty() {
            match flock.flock().await {
                Ok(snapshot) => rules.learn(&snapshot),
                Err(err) => eprintln!("shep dog bark: could not read the flock's names: {err}"),
            }
        }

        loop {
            tokio::select! {
                () = stop.wait() => break ExitCode::Success,
                next = events.next() => {
                    match next {
                        // One connection generation ended. Usually the
                        // shepherd exec'd a successor, which is not the
                        // shepherd going away: only a shepherd that never
                        // answers again ends this dog.
                        None => match events.resubscribe().await {
                            Ok(()) => {
                                // Reconcile whichever daemon answered.
                                // Frames sent with no subscription are
                                // gone. The verdict decides nothing here:
                                // bark debounces on a sheep's name, not an id.
                                reconcile(&flock, &mut rules, &mut clock, &delivery).await;
                                pacing.polled();
                            }
                            Err(failed) => break exit_for(&failed),
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
                                let mut next_rules = next_rules;
                                next_rules.take_names_from(&mut rules);
                                next_rules.take_sandbox_refusals_from(&mut rules);
                                rules = next_rules;
                                if next.poll.as_duration() != poll_period {
                                    poll_period = next.poll.as_duration();
                                    poll_interval = poll_timer(poll_period);
                                }
                                eprintln!("shep dog bark: reloaded [bark] from dogs.toml");
                                // The reload may have added or dropped the
                                // rules that read logs, and the shepherd
                                // forwards a line only to a subscriber that
                                // asked for its topic.
                                let wanted = wanted_topics(&events, &rules);
                                if wanted != events.topics() {
                                    if let Err(failed) = events.set_topics(wanted).await {
                                        break exit_for(&failed);
                                    }
                                    // Frames unread on the old stream are gone.
                                    reconcile(&flock, &mut rules, &mut clock, &delivery).await;
                                    pacing.polled();
                                }
                            }
                        }
                        Some(Ok(event)) => {
                            let firings = rules.on_event(&event, clock.now_ms());
                            spawn_firings(firings, &delivery);
                        }
                        Some(Err(_dropped)) => {
                            // The drop says nothing about what was lost;
                            // only the shepherd can.
                            if pacing.lag_polls_now() {
                                reconcile(&flock, &mut rules, &mut clock, &delivery).await;
                                pacing.polled();
                            }
                        }
                    }
                }
                () = deferred_poll(pacing.deferred_poll_at()) => {
                    reconcile(&flock, &mut rules, &mut clock, &delivery).await;
                    pacing.polled();
                }
                _ = poll_interval.tick() => {
                    reconcile(&flock, &mut rules, &mut clock, &delivery).await;
                    pacing.polled();
                }
            }
        }
    }
}

/// The topics `rules` need on top of what `events` already holds: its
/// non-log topics, then the log ones the rules read.
///
/// Derived from the source rather than passed in, so the loop has one list
/// of base topics, the one the subscription was built with.
fn wanted_topics<E: EventSource>(events: &E, rules: &Rules) -> Vec<String> {
    let mut topics: Vec<String> = events
        .topics()
        .iter()
        .filter(|topic| !topic.starts_with("log."))
        .cloned()
        .collect();
    topics.extend(rules.log_topics());
    topics
}

/// Reports why bark cannot keep its subscription and the exit code that
/// ends the dog.
fn exit_for(failed: &Resubscribe) -> ExitCode {
    eprintln!("shep dog bark: {failed}");
    match failed {
        Resubscribe::Lost(lost) => ExitCode::from(lost),
        Resubscribe::Request(err) => ExitCode::from(err),
    }
}

/// One reconciliation pass: ask `flock` what the flock looks like now, run
/// it through [`Rules::on_poll`] stamped by `clock`, and spawn a delivery
/// for anything that fires. Shared by [`run_loop`]'s lag arm and interval
/// arm, so the two polls are one code path.
///
/// A failed poll is logged and dropped: the next bus event or interval
/// tick tries again.
pub(super) async fn reconcile<F: FlockSource>(
    flock: &F,
    rules: &mut Rules,
    clock: &mut Clock,
    delivery: &Delivery,
) {
    match flock.flock().await {
        Ok(snapshot) => {
            let firings = rules.on_poll(&snapshot, clock.now_ms());
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
    let config = match parse_section::<BarkConfig>("bark", &section) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("shep dog bark: {err}; see `shep dogs`");
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

/// Resolves at `due`, or never when there is no held lag.
async fn deferred_poll(due: Option<Instant>) {
    match due {
        Some(due) => tokio::time::sleep_until(due).await,
        None => core::future::pending().await,
    }
}

/// The poll timer for `period`.
///
/// `interval_at`, not `interval`: a plain `interval` fires its first tick
/// immediately, so the first poll would be attributable to the timer's
/// startup rather than to a drop or an elapsed interval.
fn poll_timer(period: Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    interval
}

#[cfg(test)]
mod log_match_tests;

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
            &config_with_sink(addr),
            &barks_path,
            // Never asked: no `config.dog.bark` frame is sent here.
            ScriptedConfig::answering(String::new()),
            Stop::never(),
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
            &config_with_sink(addr),
            &barks_path,
            ScriptedConfig::answering(String::new()),
            Stop::never(),
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
    /// that; an inline-awaiting loop would still be parked.
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
            Stop::never(),
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
        // satisfying `captured` without the third generation ever being
        // read. The only bark here has to come from the stream.
        let flock = ScriptedFlock::answering(Vec::new());

        let loop_handle = tokio::spawn(run_loop(
            source,
            flock.clone(),
            gave_up_rules(),
            &config_with_sink(addr),
            &barks_path,
            ScriptedConfig::answering(String::new()),
            Stop::never(),
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

    /// An [`EventSource`] whose every item is a lag notice, sent when the
    /// test says so.
    struct LagSource(tokio::sync::mpsc::UnboundedReceiver<u64>);

    impl EventSource for LagSource {
        async fn next(&mut self) -> Option<Result<BusEvent, u64>> {
            self.0.recv().await.map(Err)
        }

        fn topics(&self) -> &[String] {
            &[]
        }

        async fn set_topics(&mut self, _topics: Vec<String>) -> Result<(), Resubscribe> {
            Ok(())
        }

        async fn resubscribe(&mut self) -> Result<(), Resubscribe> {
            Err(Resubscribe::Lost(LinkLost::Budget {
                waited: Duration::ZERO,
            }))
        }
    }

    /// A loop over a [`LagSource`] polling every `poll`, with an empty flock
    /// so a poll fires nothing.
    ///
    /// The directory is returned to keep it alive for the loop's lifetime.
    fn lagging_loop(
        poll: Duration,
    ) -> (
        tokio::sync::mpsc::UnboundedSender<u64>,
        ScriptedFlock,
        tokio::task::JoinHandle<ExitCode>,
        tempfile::TempDir,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let flock = ScriptedFlock::answering(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_with_sink("127.0.0.1:1".parse().unwrap());
        config.poll = UpDuration::from_millis(poll.as_millis() as u64);
        let handle = tokio::spawn(run_loop(
            LagSource(rx),
            flock.clone(),
            gave_up_rules(),
            &config,
            &dir.path().join("barks.jsonl"),
            ScriptedConfig::answering(String::new()),
            Stop::never(),
        ));
        (tx, flock, handle, dir)
    }

    /// Moves the paused clock by `by`, then lets the loop run everything
    /// that became ready.
    async fn advance_and_settle(by: Duration) {
        tokio::time::advance(by).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    /// Fails if a busy bus can trigger a poll per lag notice: five notices
    /// inside one gap cost the immediate poll and one deferred to the end of
    /// the gap, no earlier.
    #[tokio::test(start_paused = true)]
    async fn a_burst_of_lags_costs_one_immediate_poll_and_one_deferred() {
        let (tx, flock, handle, _dir) = lagging_loop(Duration::from_secs(60));

        tx.send(1).unwrap();
        advance_and_settle(Duration::ZERO).await;
        assert_eq!(flock.calls(), 1, "the first lag after quiet polls at once");

        for _ in 0..4 {
            advance_and_settle(Duration::from_millis(200)).await;
            tx.send(1).unwrap();
        }
        advance_and_settle(Duration::from_millis(199)).await;
        assert_eq!(
            flock.calls(),
            1,
            "four lags inside the gap wait for its end"
        );

        advance_and_settle(Duration::from_millis(1)).await;
        assert_eq!(flock.calls(), 2, "the deferred poll runs when the gap ends");

        advance_and_settle(LAG_POLL_MIN_GAP * 5).await;
        assert_eq!(flock.calls(), 2, "the deferred poll is spent, not repeated");

        handle.abort();
    }

    /// Fails if a lag that arrives once the gap has passed is made to wait.
    #[tokio::test(start_paused = true)]
    async fn a_lag_after_the_gap_polls_immediately() {
        let (tx, flock, handle, _dir) = lagging_loop(Duration::from_secs(60));

        tx.send(1).unwrap();
        advance_and_settle(Duration::ZERO).await;
        assert_eq!(flock.calls(), 1);

        advance_and_settle(LAG_POLL_MIN_GAP).await;
        tx.send(1).unwrap();
        advance_and_settle(Duration::ZERO).await;
        assert_eq!(flock.calls(), 2, "a full gap since the last poll is enough");

        advance_and_settle(LAG_POLL_MIN_GAP * 5).await;
        assert_eq!(flock.calls(), 2, "nothing was deferred");

        handle.abort();
    }

    /// Fails if the lag floor holds back the interval: a tick inside the gap
    /// polls on schedule.
    #[tokio::test(start_paused = true)]
    async fn a_lag_does_not_postpone_the_interval_poll() {
        let (tx, flock, handle, _dir) = lagging_loop(Duration::from_secs(2));

        tx.send(1).unwrap();
        advance_and_settle(Duration::ZERO).await;
        advance_and_settle(Duration::from_millis(1_500)).await;
        tx.send(1).unwrap();
        advance_and_settle(Duration::ZERO).await;
        assert_eq!(flock.calls(), 2, "the second lag is past the gap");

        // The tick is due at 2s, half a gap after the last poll.
        advance_and_settle(Duration::from_millis(499)).await;
        assert_eq!(flock.calls(), 2);
        advance_and_settle(Duration::from_millis(1)).await;
        assert_eq!(flock.calls(), 3, "the tick polls at its own time");

        handle.abort();
    }

    /// Fails if a lag already answered by a later poll is polled for again.
    #[tokio::test(start_paused = true)]
    async fn an_interval_poll_answers_a_pending_lag() {
        let (tx, flock, handle, _dir) = lagging_loop(Duration::from_secs(2));

        tx.send(1).unwrap();
        advance_and_settle(Duration::ZERO).await;
        advance_and_settle(Duration::from_millis(1_500)).await;
        tx.send(1).unwrap();
        advance_and_settle(Duration::from_millis(250)).await;
        tx.send(1).unwrap();
        advance_and_settle(Duration::ZERO).await;
        assert_eq!(flock.calls(), 2, "the third lag is inside the gap");

        advance_and_settle(Duration::from_millis(250)).await;
        assert_eq!(flock.calls(), 3, "the tick polls");

        // Short of the next tick at 4s, past where a held lag would poll.
        advance_and_settle(LAG_POLL_MIN_GAP).await;
        assert_eq!(flock.calls(), 3, "the tick already covered the lag");

        handle.abort();
    }

    /// Runs the loop on `source` until `tx` is dropped and the dog exits on
    /// whatever its re-subscribe reports.
    async fn exit_code_once_the_stream_ends(
        source: HandoverSource,
        tx: broadcast::Sender<BusEvent>,
    ) -> ExitCode {
        let (addr, _captured) = one_shot_sink(200, "").await;
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");
        let loop_handle = tokio::spawn(run_loop(
            source,
            ScriptedFlock::answering(Vec::new()),
            gave_up_rules(),
            &config_with_sink(addr),
            &barks_path,
            ScriptedConfig::answering(String::new()),
            Stop::never(),
        ));
        drop(tx);
        tokio::time::timeout(Duration::from_secs(5), loop_handle)
            .await
            .expect("a dog whose stream ended for good must exit, not linger")
            .unwrap()
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

        let code = exit_code_once_the_stream_ends(source, tx).await;
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

        let code = exit_code_once_the_stream_ends(source, tx).await;
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

        let code = exit_code_once_the_stream_ends(source, tx).await;
        assert_eq!(code, ExitCode::ProtocolMismatch);
    }

    /// Fails if a `config.dog.bark` frame does not reach bark's sinks: the
    /// next firing is delivered to the sink the new section names, over a
    /// real socket, while the old sink's server is still up.
    ///
    /// The loop is the same loop throughout: bark's config is pure data
    /// with no OS resource attached, so it swaps in place. A real clock,
    /// not `start_paused`, or a broken swap hangs the suite.
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
            &config_with_sink(old_addr),
            &barks_path,
            source.clone(),
            Stop::never(),
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
            &config_with_sink(addr),
            &barks_path,
            source.clone(),
            Stop::never(),
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
