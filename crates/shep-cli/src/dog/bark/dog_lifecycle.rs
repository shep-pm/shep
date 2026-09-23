use super::config::BarkConfig;
use super::config_hot_reload::{ConfigSource, FlockSource, reconcile, reloaded_config};
use super::firing_delivery::{Delivery, now_ms, poll_timer, spawn_firings};
use super::rules::Rules;
use crate::exit::ExitCode;
use core::fmt;
use core::future::Future;
use shep_client::{LinkLost, RequestError};
use shep_core::protocol::BusEvent;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

/// One source of bus events: a frame, or a notice that frames were lost.
///
/// A trait rather than a concrete `EventStream`, so a test can drive this
/// loop from a real `tokio::sync::broadcast::Receiver` with a small
/// capacity and make the bus genuinely drop events.
pub trait EventSource: Send {
    /// The next event; `Err(count)` when the source dropped `count` frames
    /// before this one; `None` when it ends.
    fn next(&mut self) -> impl Future<Output = Option<Result<BusEvent, u64>>> + Send;

    /// Arms a fresh source against whatever shepherd is answering now,
    /// waiting a bounded time for one to be.
    ///
    /// A subscription belongs to one connection, so a handover ends it:
    /// the shepherd execs a successor on purpose and every dog is meant to
    /// cross that without restarting. What [`run_loop`] calls when
    /// [`Self::next`] returns `None`.
    ///
    /// # Errors
    /// [`Resubscribe`], which the dog exits on either way. The two arms
    /// exit differently, because a shepherd that never answered and one
    /// that answered and refused send an operator to different places.
    fn resubscribe(&mut self) -> impl Future<Output = Result<(), Resubscribe>> + Send;
}

/// Why bark could not arm a fresh subscription.
///
/// Two outcomes rather than one, so a re-subscribe reports what the first
/// subscription would have. [`run`](super::run) exits
/// `ExitCode::from(&err)` when the shepherd refuses the opening
/// `Subscribe`; without this a refusal after a handover would exit
/// `DaemonUnreachable` instead, naming a shepherd that is running and
/// answering.
#[derive(Debug)]
#[must_use = "which of the two it was decides how the dog exits"]
pub enum Resubscribe {
    /// No shepherd answered inside the dog's budget, or one refused this
    /// dog's protocol version at the handshake.
    Lost(LinkLost),
    /// The `Subscribe` did not succeed and waiting cannot change that.
    /// Carried whole, since `RequestError` already decides an exit code and
    /// flattening it would lose that.
    ///
    /// All four of `RequestError`'s non-`Closed` conditions arrive here,
    /// and they are not one fault:
    ///
    /// - `Rpc` is a shepherd answering and saying no.
    /// - `Wire` and `Undecodable` are the transport failing, so pointing an
    ///   operator at the shepherd's configuration sends them to the wrong
    ///   place.
    /// - `Timeout` is nobody answering in time, which means the request may
    ///   never have reached a shepherd at all.
    ///
    /// Named for the request rather than for a refusal, matching
    /// [`DogRunError::Request`](crate::dog::runtime::DogRunError::Request),
    /// because only one of the four is a refusal.
    ///
    /// `Timeout` cannot arrive here today: `ClientEvents::resubscribe`
    /// bounds each attempt by what is left of `SHEPHERD_RETURN_BUDGET`,
    /// five seconds, and `Client::subscribe` carries seven, so the outer
    /// bound always fires first and reports a spent budget instead. That is
    /// a consequence of the two numbers rather than a guarantee, so the
    /// variant documents the condition rather than relying on it.
    Request(RequestError),
}

impl fmt::Display for Resubscribe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lost(lost) => lost.fmt(f),
            Self::Request(err) => write!(f, "the subscription request failed: {err}"),
        }
    }
}

impl core::error::Error for Resubscribe {
    /// Both arms wrap an error rather than describing one, so a structured
    /// logger walking the chain reaches the connection or RPC failure
    /// underneath instead of stopping here.
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Lost(lost) => Some(lost),
            Self::Request(err) => Some(err),
        }
    }
}

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
    let sinks = Arc::new(config.sinks.clone());
    let sink_timeout = config.sink_timeout.as_duration();
    let max_bytes = config.history_bytes;
    let mut poll_period = config.poll.as_duration();
    let barks_path = Arc::new(barks_path.to_path_buf());

    async move {
        let mut events = events;
        let mut rules = rules;
        let mut delivery = Delivery {
            sinks,
            append_lock: Arc::new(Mutex::new(())),
            barks_path,
            sink_timeout,
            max_bytes,
        };

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
                                    Resubscribe::Lost(lost) => super::super::exit_for(lost),
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
                                delivery.sinks = Arc::new(next.sinks.clone());
                                delivery.sink_timeout = next.sink_timeout.as_duration();
                                delivery.max_bytes = next.history_bytes;
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

#[cfg(test)]
mod tests {

    use std::time::Duration;

    use shep_client::LinkLost;

    use shep_core::protocol::BusEvent;

    use crate::exit::ExitCode;

    use shep_core::protocol::RpcErrorCode;

    use super::*;
    use tokio::sync::broadcast;

    use super::super::testing::*;

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
}
