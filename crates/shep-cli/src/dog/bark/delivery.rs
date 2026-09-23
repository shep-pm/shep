use super::config::BarkConfig;
use super::rules::Firing;
use super::sinks::Sink;
use futures_util::future::join_all;
use shep_core::barks::{self, SinkOutcome};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Everything a delivery needs, so the five values that travel together
/// through [`reconcile`](super::event_loop::reconcile), [`spawn_firings`] and
/// [`deliver_and_record`] travel as one.
///
/// `Clone` is what [`spawn_firings`] hands each spawned task: three
/// [`Arc`] bumps and two copies.
///
/// `Debug` is safe despite `sinks` holding webhook URLs, which are bearer
/// credentials: [`Sink`]'s own `Debug` redacts them.
#[derive(Debug, Clone)]
pub(super) struct Delivery {
    /// Every configured sink by name, for resolving a firing's own list.
    pub(super) sinks: Arc<BTreeMap<String, Sink>>,
    /// Serializes this process's appends to `barks_path`.
    pub(super) append_lock: Arc<Mutex<()>>,
    /// Where the bark trail is written.
    pub(super) barks_path: Arc<PathBuf>,
    /// How long one sink delivery may take.
    pub(super) sink_timeout: Duration,
    /// The trail's size ceiling.
    pub(super) max_bytes: u64,
}

impl Delivery {
    /// A delivery to `config`'s sinks, writing its trail to `barks_path`.
    pub(super) fn new(config: &BarkConfig, barks_path: &Path) -> Self {
        Self {
            sinks: Arc::new(config.sinks.clone()),
            append_lock: Arc::new(Mutex::new(())),
            barks_path: Arc::new(barks_path.to_path_buf()),
            sink_timeout: config.sink_timeout.as_duration(),
            max_bytes: config.history_bytes,
        }
    }

    /// Takes what a reloaded `config` changes, in place. The append lock
    /// and the trail's path outlive every reload.
    pub(super) fn reconfigure(&mut self, config: &BarkConfig) {
        self.sinks = Arc::new(config.sinks.clone());
        self.sink_timeout = config.sink_timeout.as_duration();
        self.max_bytes = config.history_bytes;
    }
}

/// Spawns one delivery task per firing, so
/// [`run_loop`](super::event_loop::run_loop)'s own `select!` returns to
/// reading the next event immediately rather than waiting on any of them.
pub(super) fn spawn_firings(firings: Vec<Firing>, delivery: &Delivery) {
    for firing in firings {
        let delivery = delivery.clone();
        tokio::spawn(async move {
            deliver_and_record(firing, &delivery).await;
        });
    }
}

/// Delivers `firing` to each of its named sinks, then writes the resulting
/// [`shep_core::barks::Bark`] to `barks_path`.
///
/// The sinks are driven together rather than in turn: a dead endpoint's
/// timeout runs beside the healthy sinks' deliveries instead of ahead of
/// them, so one unreachable webhook no longer holds up an alert the
/// others could have carried. Each delivery keeps its own
/// [`Delivery::sink_timeout`] and its own [`SinkOutcome`], and `join_all`
/// hands the outcomes back in `firing.sinks`' order, which is the order
/// the trail has always recorded them in.
///
/// After delivery, since a [`Firing`]'s [`shep_core::barks::Bark::sinks`]
/// is empty until each sink has been tried. Written even when every sink
/// refused it: the local trail is what an operator reads when the page
/// never arrived.
///
/// [`Delivery::append_lock`] covers only the [`barks::append`] call, a
/// read-modify-rename against one file that several of these run at once.
/// It does not replace `append`'s own cross-process `flock(2)`.
async fn deliver_and_record(firing: Firing, delivery: &Delivery) {
    let mut bark = firing.bark;
    let outcomes = join_all(firing.sinks.iter().map(|name| async {
        match delivery.sinks.get(name) {
            Some(sink) => match super::sinks::deliver(sink, &bark, delivery.sink_timeout).await {
                Ok(()) => SinkOutcome {
                    sink: name.clone(),
                    error: None,
                },
                Err(err) => SinkOutcome {
                    sink: name.clone(),
                    error: Some(err.to_string()),
                },
            },
            // Unreachable: `Rules::new` refuses a rule routing to a sink
            // `[dog.bark.sinks]` does not define. Recorded rather than
            // panicked on.
            None => SinkOutcome {
                sink: name.clone(),
                error: Some("sink not configured".to_owned()),
            },
        }
    }))
    .await;
    bark.sinks = outcomes;

    let _guard = delivery.append_lock.lock().await;
    if let Err(err) = barks::append(&delivery.barks_path, &bark, delivery.max_bytes) {
        eprintln!("shep dog bark: could not record a fired bark: {err}");
    }
}

#[cfg(test)]
mod tests {

    use std::collections::BTreeMap;

    use std::sync::Arc;
    use std::time::Duration;

    use shep_core::barks::{self};

    use tokio::sync::Mutex;

    use super::super::rules::Firing;

    use shep_core::barks::Bark;

    use super::*;

    use super::super::testing::*;
    use shep_core::values::UpDuration;

    /// A `gave_up` firing for `web`, routed to `sinks`.
    fn gave_up_firing(sinks: &[&str]) -> Firing {
        Firing {
            bark: Bark {
                at_ms: 1_000,
                rule: "gave_up".to_owned(),
                subject: "web".to_owned(),
                message: "web gave up: restart budget exhausted".to_owned(),
                sinks: Vec::new(),
            },
            sinks: sinks.iter().map(|&name| name.to_owned()).collect(),
        }
    }

    /// Drives `deliver_and_record` directly rather than through
    /// `run_loop`: the property belongs to that function, and the loop's
    /// event plumbing would need a second synchronization mechanism to
    /// know when a failed delivery finished.
    #[tokio::test]
    async fn a_bark_is_recorded_even_when_every_sink_refuses_it() {
        let (addr, _captured) = one_shot_sink(500, "refused").await;
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");

        let mut sinks = BTreeMap::new();
        sinks.insert("ops".to_owned(), json_sink(format!("http://{addr}/hook")));
        let firing = gave_up_firing(&["ops"]);

        let delivery = Delivery {
            sinks: Arc::new(sinks),
            append_lock: Arc::new(Mutex::new(())),
            barks_path: Arc::new(barks_path.clone()),
            sink_timeout: Duration::from_secs(5),
            max_bytes: barks::DEFAULT_MAX_BYTES,
        };
        // Wider than `sink_timeout` by a margin a refusal from localhost
        // never needs, so only a regression into a hang reaches it.
        tokio::time::timeout(
            Duration::from_secs(30),
            deliver_and_record(firing, &delivery),
        )
        .await
        .expect("the delivery outlived its own sink timeout");

        let recorded = shep_core::barks::read(&barks_path).unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "a refused delivery must still be recorded"
        );
        assert_eq!(recorded[0].subject, "web");
        assert!(
            recorded[0].sinks[0].error.is_some(),
            "the 500 must be recorded as a failed delivery, not silently dropped"
        );
    }

    /// A dead endpoint's timeout must run beside the healthy sinks, not
    /// ahead of them. Asserted as an order rather than a duration, the
    /// way `slow_sink`'s own doc suggests: the live sink's request has to
    /// arrive while the dead one is still hanging, and in turn it is not
    /// contacted until that timeout has already passed.
    #[tokio::test]
    async fn a_dead_sink_does_not_delay_the_live_ones_delivery() {
        let (dead_addr, dead_connected) = slow_sink().await;
        let (live_addr, live_request) = one_shot_sink(200, "ok").await;
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");

        let mut sinks = BTreeMap::new();
        sinks.insert(
            "dead".to_owned(),
            json_sink(format!("http://{dead_addr}/hook")),
        );
        sinks.insert(
            "live".to_owned(),
            json_sink(format!("http://{live_addr}/hook")),
        );
        let firing = gave_up_firing(&["dead", "live"]);

        let delivery = Delivery {
            sinks: Arc::new(sinks),
            append_lock: Arc::new(Mutex::new(())),
            barks_path: Arc::new(barks_path.clone()),
            // Wider than the wait below by a margin no localhost round
            // trip closes: in turn, the live sink is not contacted until
            // this has already elapsed.
            sink_timeout: Duration::from_millis(500),
            max_bytes: barks::DEFAULT_MAX_BYTES,
        };
        let recorded = tokio::spawn(async move { deliver_and_record(firing, &delivery).await });

        tokio::time::timeout(Duration::from_secs(5), dead_connected)
            .await
            .expect("the dead sink must be contacted at all")
            .expect("the dead sink's accept signal must arrive");
        tokio::time::timeout(Duration::from_millis(150), live_request)
            .await
            .expect(
                "the live sink must hear about the alert while the dead one still hangs; \
                 driven in turn it would have waited out the dead sink's timeout first",
            )
            .expect("the live sink must have captured its request");

        // The delivery then finishes on the dead sink's own timeout, with
        // both outcomes in the firing's order.
        tokio::time::timeout(Duration::from_secs(5), recorded)
            .await
            .expect("the delivery must end on the dead sink's own timeout, not outlive it")
            .expect("the delivery task must not panic");
        let barks = shep_core::barks::read(&barks_path).unwrap();
        assert_eq!(barks.len(), 1);
        let outcomes = &barks[0].sinks;
        assert_eq!(outcomes.len(), 2, "both sinks must be recorded");
        assert_eq!(
            outcomes[0].sink, "dead",
            "the trail keeps the firing's order"
        );
        assert!(
            outcomes[0].error.is_some(),
            "the dead sink's timeout is its outcome, not a dropped one"
        );
        assert_eq!(outcomes[1].sink, "live");
        assert!(
            outcomes[1].error.is_none(),
            "the live sink must have been delivered to: {:?}",
            outcomes[1]
        );
    }

    #[test]
    fn a_reconfigure_takes_the_new_settings_and_keeps_the_trail() {
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");
        let before = config_with_sink("127.0.0.1:1".parse().unwrap());
        let mut delivery = Delivery::new(&before, &barks_path);
        let lock = Arc::clone(&delivery.append_lock);

        let mut after = config_with_sink("127.0.0.1:2".parse().unwrap());
        after.sink_timeout = UpDuration::from_millis(1_234);
        after.history_bytes = 4_096;
        delivery.reconfigure(&after);

        assert_eq!(*delivery.sinks, after.sinks);
        assert_eq!(delivery.sink_timeout, Duration::from_millis(1_234));
        assert_eq!(delivery.max_bytes, 4_096);
        assert!(
            Arc::ptr_eq(&delivery.append_lock, &lock),
            "a second lock would let two appends race"
        );
        assert_eq!(*delivery.barks_path, barks_path);
    }
}
