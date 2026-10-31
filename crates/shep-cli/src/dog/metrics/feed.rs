//! The metrics dog's feed: the bus subscription that fills [`SampleStore`].
//!
//! The dog used to touch its client only when something scraped it, which is
//! why nothing here was ever armed against the bus. Keeping samples across
//! scrapes needs the events that say when a sender's samples are stale, so
//! this task runs beside the scrape server for the dog's whole life.

use std::sync::Arc;

use shep_client::{EventStream, RECONNECT_MIN_DELAY, ReconnectingClient, RequestError};
use shep_core::protocol::{BusEvent, ProcessEventKind};

use super::samples::{ChannelMetric, SampleStore};
use crate::dog::SHEPHERD_RETURN_BUDGET;
use crate::exit::ExitCode;

/// The two topics this dog needs: the samples it keeps, and the lifecycle
/// events that say when they are stale.
const TOPICS: [&str; 2] = ["channel.metric", "process.*"];

/// Reads the bus into `store` until the dog is over.
///
/// Raced in [`super::run`]'s `select!` beside the scrape server and
/// [`ReconnectingClient::link_lost`]. It returns an [`ExitCode`] only for a
/// shepherd that refused this dog's subscription: a shepherd that never comes
/// back parks this future instead, because an arm that returned on that would
/// end `run` with a code of its own and hide `DaemonUnreachable`.
pub(super) async fn run(client: Arc<ReconnectingClient>, store: Arc<SampleStore>) -> ExitCode {
    let mut stream = match arm(&client).await {
        Ok(stream) => stream,
        Err(code) => return code,
    };
    loop {
        match stream.next().await {
            Some(Ok(event)) => apply(&store, event),
            // A local lag is a gap, and `SampleStore::snapshot` drops a sender
            // whose samples are older than the instance the flock reports.
            // That is the whole of what a gap needs here.
            Some(Err(_lagged)) => {}
            // A subscription belongs to one connection generation, so a
            // handover ends this stream and nothing re-arms it for free.
            None => match arm(&client).await {
                Ok(next) => stream = next,
                Err(code) => return code,
            },
        }
    }
}

/// Waits for a live connection and subscribes to [`TOPICS`] on it.
///
/// `Err` is the exit code a refused subscription costs, already printed.
async fn arm(client: &ReconnectingClient) -> Result<EventStream, ExitCode> {
    loop {
        if client
            .connected_within(SHEPHERD_RETURN_BUDGET)
            .await
            .is_err()
        {
            // No shepherd inside the budget. Park rather than return:
            // `run` races this against `link_lost`, which is the arm that
            // reports the outage and its code.
            core::future::pending::<()>().await;
        }
        match client.subscribe(TOPICS.map(str::to_string).to_vec()).await {
            Ok(stream) => return Ok(stream),
            // The generation this was issued on had already gone. The
            // supervisor reports a connection's death a moment after the
            // socket does. A bare retry would spin and starve it on a
            // current-thread runtime, so wait one rung of its ladder.
            Err(RequestError::Closed) => tokio::time::sleep(RECONNECT_MIN_DELAY).await,
            // The request reached a shepherd and did not succeed. Waiting
            // cannot change that, and a dog serving without its feed looks
            // healthy, so say so and exit on the refusal's own code.
            Err(err) => {
                eprintln!("shep dog metrics: the subscription request failed: {err}");
                return Err(ExitCode::from(&err));
            }
        }
    }
}

/// Applies one bus event to `store`.
fn apply(store: &SampleStore, event: BusEvent) {
    match event {
        BusEvent::Channel { id, message } => {
            let Some(metric) = ChannelMetric::from_child(message) else {
                return;
            };
            let Some(refusal) = store.upsert(id, metric) else {
                return;
            };
            if refusal.first {
                eprintln!("shep dog metrics: {refusal}");
            }
        }
        BusEvent::Process {
            event: kind, info, ..
        } if clears_samples(kind) => store.clear(info.id),
        _ => {}
    }
}

/// Whether `kind` means the samples this sender sent belong to an instance
/// that no longer exists.
///
/// The arm a kind this build has not been taught lands on frees the
/// samples. The guess is made on the side that costs less: freeing on a
/// kind that meant "still the same instance" loses one sample period,
/// keeping on one that meant "gone" leaks a series in every scrape.
fn clears_samples(kind: ProcessEventKind) -> bool {
    match kind {
        ProcessEventKind::Start
        | ProcessEventKind::Restart
        | ProcessEventKind::Exit
        | ProcessEventKind::Stop
        | ProcessEventKind::Delete
        | ProcessEventKind::Errored
        | ProcessEventKind::SandboxRefused => true,
        // Still the instance that is serving. `Online` follows a `Start`
        // this dog already cleared for, and `Reloaded` names the
        // replacement, not the drainee the shepherd already deregistered
        // and cleared. Clearing on it loses the new instance's numbers.
        ProcessEventKind::Online
        | ProcessEventKind::Reload
        | ProcessEventKind::Reloaded
        | ProcessEventKind::ReloadAbandoned => false,
        // Where `Unrecognized` and any variant added later land.
        _ => true,
    }
}
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shep_client::testing::{
        Handshake, control_address, fake_daemon_across_handovers, fake_reconnecting_client_on,
        sample_ack,
    };
    use shep_core::protocol::{
        ChildMessage, DogSource, HelloAck, PROTOCOL_VERSION, ProcessInfo, RpcError, RpcErrorCode,
    };
    use shep_core::status::ProcStatus;

    use super::super::Reading;
    use super::super::exposition;
    use super::super::grammar::REFUSED_TOTAL;
    use super::*;

    /// How long a test waits for the feed to react to an event it pushed.
    /// Real sockets, so real time, and generous against how fast a loopback
    /// frame and a `Mutex` pass take.
    const REACT_BUDGET: Duration = Duration::from_secs(5);

    fn sheep(id: u32, name: &str) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online)
            .uptime_ms(60_000)
            .fold(Some("backend".to_string()))
            .build()
    }

    fn metric(id: u32, name: &str, value: f64) -> BusEvent {
        BusEvent::Channel {
            id,
            message: ChildMessage::Metric {
                name: name.to_string(),
                value,
                metric_type: None,
                labels: Default::default(),
                help: None,
            },
        }
    }

    /// A note named the way a sample's grammar refuses: a dot and an epoch.
    fn note(id: u32) -> BusEvent {
        BusEvent::Channel {
            id,
            message: ChildMessage::Note {
                name: "lease.web.want.1760500000".to_string(),
                value: 1.0,
            },
        }
    }

    fn process_event(kind: ProcessEventKind, info: ProcessInfo) -> BusEvent {
        BusEvent::Process {
            event: kind,
            info,
            manually: false,
            at_ms: 0,
        }
    }

    /// The metric names `store` holds for `flock`, minus the dog's own
    /// [`REFUSED_TOTAL`].
    ///
    /// [`SampleStore::snapshot`] carries that counter beside the samples it
    /// retained, and every wait below is about the samples. Reading the two as
    /// one list is what made a test that had caused a refusal assert against an
    /// extra name.
    fn sample_names(store: &SampleStore, flock: &[ProcessInfo]) -> Vec<String> {
        store
            .snapshot(flock)
            .iter()
            .map(|family| family.name.clone())
            .filter(|name| name != REFUSED_TOTAL)
            .collect()
    }

    /// Waits until `store` holds at least `want` samples for `flock`, and
    /// returns their metric names, or `None` when that never happened inside
    /// `REACT_BUDGET`. Every caller asserts on a name that could not have been
    /// there before the push it just made, so the wait is for a transition and
    /// not for a state.
    async fn snapshot_reaching(
        store: &SampleStore,
        flock: &[ProcessInfo],
        want: usize,
    ) -> Option<Vec<String>> {
        let started = tokio::time::Instant::now();
        loop {
            let names = sample_names(store, flock);
            if names.len() >= want {
                return Some(names);
            }
            if started.elapsed() > REACT_BUDGET {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Waits until `store` holds no samples for `flock`, which every caller
    /// calls only after asserting it held some. The refusal counter is excluded
    /// for the reason [`sample_names`] gives: it belongs to the dog rather than
    /// to a sender, and no lifecycle event clears it.
    async fn snapshot_clears(store: &SampleStore, flock: &[ProcessInfo]) -> bool {
        let started = tokio::time::Instant::now();
        loop {
            if sample_names(store, flock).is_empty() {
                return true;
            }
            if started.elapsed() > REACT_BUDGET {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The exposition this dog would answer right now.
    fn scrape(store: &SampleStore, flock: &[ProcessInfo]) -> String {
        exposition::render(&Reading {
            flock: flock.to_vec(),
            daemon_version: "9.9.9".to_string(),
            daemon_pid: 12345,
            host: None,
            samples: store.snapshot(flock),
        })
    }

    fn spawn_feed(
        client: ReconnectingClient,
        store: Arc<SampleStore>,
    ) -> tokio::task::JoinHandle<ExitCode> {
        tokio::spawn(run(Arc::new(client), store))
    }

    /// Every kind this build has is listed on one side of the table.
    ///
    /// `ProcessEventKind` is `#[non_exhaustive]`, so no match in this crate can
    /// be exhaustive and no test here can enumerate what does not exist yet.
    /// [`a_kind_this_build_has_not_been_taught_frees_the_senders_samples`] is
    /// the half that survives a variant being added later.
    #[test]
    fn every_event_kind_is_listed_as_clear_or_keep() {
        for kind in [
            ProcessEventKind::Start,
            ProcessEventKind::Restart,
            ProcessEventKind::Exit,
            ProcessEventKind::Stop,
            ProcessEventKind::Delete,
            ProcessEventKind::Errored,
            ProcessEventKind::SandboxRefused,
        ] {
            assert!(
                clears_samples(kind),
                "{kind:?} must free the sender's samples"
            );
        }
        for kind in [
            ProcessEventKind::Online,
            ProcessEventKind::Reload,
            ProcessEventKind::Reloaded,
            ProcessEventKind::ReloadAbandoned,
        ] {
            assert!(
                !clears_samples(kind),
                "{kind:?} describes the instance that is still serving"
            );
        }
    }

    /// The arm a kind this build has not been taught lands on, read through
    /// the kind that arm already has to serve: [`ProcessEventKind::Unrecognized`],
    /// which only decoding produces.
    ///
    /// It frees rather than keeps. A sender sends its next sample, so freeing
    /// costs at most one sample period, while a kept one is a series every
    /// scrape renders and never updates.
    #[test]
    fn a_kind_this_build_has_not_been_taught_frees_the_senders_samples() {
        assert!(
            clears_samples(ProcessEventKind::Unrecognized),
            "an unknown kind kept a dead instance's samples in every scrape"
        );

        let store = SampleStore::new();
        let flock = [sheep(1, "web")];
        apply(&store, metric(1, "rps", 7.0));
        apply(
            &store,
            process_event(ProcessEventKind::Unrecognized, sheep(1, "web")),
        );
        assert!(sample_names(&store, &flock).is_empty());
    }

    /// A replay of what a completed swap puts on the bus, read from the
    /// shepherd's own test of it
    /// (`crates/shep-daemon/src/supervisor/tests/reload_bus.rs`): `Reload`
    /// names the drainee, the replacement starts on a fresh id, the drainee
    /// is `Delete`d, and `Reloaded` lands on the replacement.
    ///
    /// The replacement's own sample survives the swap; its `Delete` frees it.
    #[test]
    fn a_completed_swap_keeps_the_replacements_sample_and_frees_the_draineed() {
        let store = SampleStore::new();
        apply(
            &store,
            process_event(ProcessEventKind::Reload, sheep(0, "web")),
        );
        apply(
            &store,
            process_event(ProcessEventKind::Start, sheep(1, "web")),
        );
        apply(&store, metric(0, "queue_depth", 1.0));
        apply(&store, metric(1, "queue_depth", 3.0));
        apply(
            &store,
            process_event(ProcessEventKind::Delete, sheep(0, "web")),
        );
        apply(
            &store,
            process_event(ProcessEventKind::Reloaded, sheep(1, "web")),
        );

        assert_eq!(
            format!("{store:?}"),
            "SampleStore { senders: 1, series: 1, refused: 0 }",
            "the swap left the wrong instance's samples behind"
        );
        assert_eq!(
            sample_names(&store, &[sheep(1, "web")]),
            vec!["queue_depth".to_string()],
            "the instance now serving lost the sample it sent"
        );
    }

    #[test]
    fn a_channel_message_that_is_not_a_sample_changes_nothing() {
        let store = SampleStore::new();
        apply(
            &store,
            BusEvent::Channel {
                id: 1,
                message: ChildMessage::Ready,
            },
        );
        assert_eq!(
            format!("{store:?}"),
            "SampleStore { senders: 0, series: 0, refused: 0 }"
        );
    }

    /// Every pattern here is a literal or a trailing `*`, so a prefix test
    /// is the shepherd's glob for them.
    #[test]
    fn the_feed_never_subscribes_to_notes() {
        let topic = note(1).topic();
        assert_eq!(topic, "channel.note");
        for pattern in TOPICS {
            let reaches = match pattern.strip_suffix('*') {
                Some(prefix) => topic.starts_with(prefix),
                None => pattern == topic,
            };
            assert!(!reaches, "`{pattern}` would deliver `{topic}` to this dog");
        }
    }

    /// The fake shepherd delivers whatever is pushed, topic or not. So
    /// this is the second line: a note that did arrive changes nothing.
    #[tokio::test]
    async fn a_note_is_neither_served_nor_counted_as_refused() {
        let dir = tempfile::tempdir().unwrap();
        let socket = control_address(dir.path());
        let (client, daemon) = fake_reconnecting_client_on(&socket).await;
        let store = Arc::new(SampleStore::new());
        let feed = spawn_feed(client, Arc::clone(&store));
        let flock = [sheep(1, "web")];

        daemon.push(note(1)).await;
        // Events reach one subscriber in order, so the sample proves the
        // note was already applied.
        daemon.push(metric(1, "rps", 7.0)).await;
        let names = snapshot_reaching(&store, &flock, 1)
            .await
            .expect("the feed never stored the sample after the note");
        assert_eq!(names, vec!["rps".to_string()]);

        assert_eq!(
            format!("{store:?}"),
            "SampleStore { senders: 1, series: 1, refused: 0 }"
        );
        let text = scrape(&store, &flock);
        assert!(!text.contains("lease"), "a note was served: {text}");
        assert!(!text.contains(REFUSED_TOTAL), "a note was refused: {text}");
        feed.abort();
    }

    /// The pure seam of [`snapshot_reaching`] and [`snapshot_clears`], so the
    /// shape CI caught here is caught without a socket too: a refusal puts the
    /// dog's own counter into `snapshot`, and a store that refused something
    /// can still reach "no samples held".
    #[test]
    fn a_refusal_leaves_one_sample_name_and_still_empties_the_store() {
        let store = SampleStore::new();
        let flock = [sheep(1, "web")];

        apply(&store, metric(1, "bad name", 7.0));
        apply(&store, metric(1, "rps", 7.0));
        assert_eq!(sample_names(&store, &flock), vec!["rps".to_string()]);
        assert_eq!(
            store
                .snapshot(&flock)
                .iter()
                .map(|family| family.name.as_str())
                .collect::<Vec<_>>(),
            vec!["rps", REFUSED_TOTAL],
            "the counter is the dog's own, not a sample"
        );

        apply(
            &store,
            process_event(ProcessEventKind::Stop, sheep(1, "web")),
        );
        assert!(
            sample_names(&store, &flock).is_empty(),
            "a store that refused something never reads as empty"
        );
    }

    #[tokio::test]
    async fn a_stopped_sheep_stops_reporting() {
        let dir = tempfile::tempdir().unwrap();
        let socket = control_address(dir.path());
        let (client, daemon) = fake_reconnecting_client_on(&socket).await;
        let store = Arc::new(SampleStore::new());
        let feed = spawn_feed(client, Arc::clone(&store));
        let flock = [sheep(1, "web")];

        daemon.push(metric(1, "rps", 7.0)).await;
        let names = snapshot_reaching(&store, &flock, 1)
            .await
            .expect("the feed never stored the sample");
        assert_eq!(names, vec!["rps".to_string()]);

        daemon
            .push(process_event(ProcessEventKind::Exit, sheep(1, "web")))
            .await;
        assert!(
            snapshot_clears(&store, &flock).await,
            "a stopped sheep is still reporting a value it stopped updating"
        );
        assert!(!feed.is_finished(), "the feed outlives one sheep's exit");
        feed.abort();
    }

    #[tokio::test]
    async fn a_restarted_sheep_starts_clean() {
        let dir = tempfile::tempdir().unwrap();
        let socket = control_address(dir.path());
        let (client, daemon) = fake_reconnecting_client_on(&socket).await;
        let store = Arc::new(SampleStore::new());
        let feed = spawn_feed(client, Arc::clone(&store));
        let flock = [sheep(1, "web")];

        daemon.push(metric(1, "rps", 7.0)).await;
        assert!(snapshot_reaching(&store, &flock, 1).await.is_some());

        daemon
            .push(process_event(ProcessEventKind::Restart, sheep(1, "web")))
            .await;
        assert!(
            snapshot_clears(&store, &flock).await,
            "a restart carried the old instance's numbers into the new one"
        );

        // And the new instance is not blocked: its own sample lands, and it
        // is the only thing the scrape renders.
        daemon.push(metric(1, "queue_depth", 3.0)).await;
        let names = snapshot_reaching(&store, &flock, 1)
            .await
            .expect("the new instance's own sample never landed");
        assert_eq!(names, vec!["queue_depth".to_string()]);
        feed.abort();
    }

    /// `Online`, `Reload` and `ReloadAbandoned` describe the instance that is
    /// still serving, so the samples it sent stay.
    #[tokio::test]
    async fn a_reload_that_never_replaced_the_sender_keeps_its_samples() {
        let dir = tempfile::tempdir().unwrap();
        let socket = control_address(dir.path());
        let (client, daemon) = fake_reconnecting_client_on(&socket).await;
        let store = Arc::new(SampleStore::new());
        let feed = spawn_feed(client, Arc::clone(&store));
        let flock = [sheep(1, "web")];

        daemon.push(metric(1, "rps", 7.0)).await;
        assert!(snapshot_reaching(&store, &flock, 1).await.is_some());

        for (i, kind) in [
            ProcessEventKind::Online,
            ProcessEventKind::Reload,
            ProcessEventKind::ReloadAbandoned,
        ]
        .into_iter()
        .enumerate()
        {
            daemon.push(process_event(kind, sheep(1, "web"))).await;
            // The metric below is the forcing mechanism: it proves the event
            // above went through the feed rather than being missed, and what
            // is asserted is that the first sample survived it.
            daemon
                .push(metric(1, &format!("after_{kind:?}"), 1.0))
                .await;
            let names = snapshot_reaching(&store, &flock, i + 2)
                .await
                .unwrap_or_else(|| {
                    panic!("{kind:?} did not reach the feed, or cleared the sender")
                });
            assert!(names.contains(&"rps".to_string()), "{names:?}");
        }
        feed.abort();
    }

    /// The store is the dog's own state, so a refusal has to be visible to the
    /// author whose metric vanished, and one refusal must not stop the feed.
    #[tokio::test]
    async fn a_refused_sample_is_counted_and_the_feed_keeps_running() {
        let dir = tempfile::tempdir().unwrap();
        let socket = control_address(dir.path());
        let (client, daemon) = fake_reconnecting_client_on(&socket).await;
        let store = Arc::new(SampleStore::new());
        let feed = spawn_feed(client, Arc::clone(&store));
        let flock = [sheep(1, "web")];

        daemon.push(metric(1, "bad name", 7.0)).await;
        // Events reach one subscriber in order, so a good sample queued after
        // the bad one proves the refusal was already processed.
        daemon.push(metric(1, "rps", 7.0)).await;
        let names = snapshot_reaching(&store, &flock, 1)
            .await
            .expect("one refusal stopped the feed");
        assert_eq!(
            names,
            vec!["rps".to_string()],
            "the refused sample was kept"
        );

        let text = scrape(&store, &flock);
        assert!(
            text.contains(r#"shep_channel_samples_refused_total{reason="invalid_name"} 1"#),
            "{text}"
        );
        feed.abort();
    }

    #[tokio::test]
    async fn the_feed_resubscribes_after_a_handover_and_keeps_serving() {
        let dir = tempfile::tempdir().unwrap();
        let socket = control_address(dir.path());
        let successor = HelloAck {
            daemon_version: "0.2.0".into(),
            protocol: PROTOCOL_VERSION,
            pid: 5150,
            min_supported: None,
        };
        let shepherds = fake_daemon_across_handovers(
            &socket,
            vec![
                Handshake::Accept(sample_ack()),
                Handshake::Accept(successor),
            ],
        );
        let client = ReconnectingClient::connect(&socket).await.unwrap();
        let store = Arc::new(SampleStore::new());
        let feed = tokio::spawn(run(Arc::new(client), Arc::clone(&store)));
        let flock = [sheep(1, "web")];

        shepherds.push(metric(1, "before", 1.0)).await;
        assert!(
            snapshot_reaching(&store, &flock, 1).await.is_some(),
            "the opening subscription never delivered a sample"
        );

        shepherds.cut().await;
        // Wait for the successor's connection to be accepted before pushing:
        // an event queued while the old generation is still being served dies
        // with it, which would test the gap rather than the recovery.
        let accepted = tokio::time::timeout(REACT_BUDGET, async {
            loop {
                if shepherds.accepted() >= 2 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(accepted.is_ok(), "the dog never reconnected to a successor");

        shepherds.push(metric(1, "after", 2.0)).await;
        let names = snapshot_reaching(&store, &flock, 2)
            .await
            .expect("the feed never re-armed its subscription after the handover");
        assert_eq!(
            names,
            vec!["after".to_string(), "before".to_string()],
            "the successor's sample never reached the store"
        );
        assert!(!feed.is_finished(), "a handover is not a reason to exit");
        feed.abort();
    }

    #[tokio::test]
    async fn a_refused_subscribe_exits_with_the_refusals_code() {
        let dir = tempfile::tempdir().unwrap();
        let socket = control_address(dir.path());
        let shepherds =
            fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
        let refusal = RpcError {
            code: RpcErrorCode::Unsupported,
            message: "this shepherd does not serve that topic".into(),
            daemon_version: None,
        };
        shepherds.refuse_next_subscribe(refusal.clone());
        let client = ReconnectingClient::connect(&socket).await.unwrap();
        let store = Arc::new(SampleStore::new());

        let code = tokio::time::timeout(REACT_BUDGET * 2, run(Arc::new(client), store))
            .await
            .expect("a refused subscription must end the feed, not park it");
        assert_eq!(
            code,
            ExitCode::from(&RequestError::Rpc(refusal)),
            "a dog serving without its feed looks healthy, so this has to exit"
        );
    }

    /// A dog's samples are stored under the dog's own id, and the flock
    /// decides the labels they render with.
    #[tokio::test]
    async fn a_dogs_sample_is_stored_under_its_own_id() {
        let dir = tempfile::tempdir().unwrap();
        let socket = control_address(dir.path());
        let (client, daemon) = fake_reconnecting_client_on(&socket).await;
        let store = Arc::new(SampleStore::new());
        let feed = spawn_feed(client, Arc::clone(&store));
        let mut bark = sheep(2, "bark");
        bark.dog = Some(DogSource::BuiltIn { grants: None });

        daemon.push(metric(2, "barks_sent", 4.0)).await;
        assert!(
            snapshot_reaching(&store, &[bark.clone()], 1)
                .await
                .is_some()
        );
        let families = store.snapshot(&[bark]);
        assert_eq!(
            families[0].points[0].labels,
            vec![
                ("dog".to_string(), "bark".to_string()),
                ("source".to_string(), "built-in".to_string()),
            ]
        );
        feed.abort();
    }
}
