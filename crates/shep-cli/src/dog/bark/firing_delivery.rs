use super::rules::Firing;
use super::sinks::Sink;
use shep_core::barks::{self, SinkOutcome};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::MissedTickBehavior;

/// Everything a delivery needs, so the five values that travel together
/// through [`reconcile`](crate::dog::bark::config_hot_reload::reconcile), [`spawn_firings`] and [`deliver_and_record`]
/// travel as one.
///
/// `Clone` is what [`spawn_firings`] hands each spawned task: three
/// [`Arc`] bumps and two copies, the same clones it used to make one by
/// one. A config reload rebinds `sinks`, `sink_timeout` and `max_bytes`
/// in place; `append_lock` and `barks_path` outlive every reload.
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

/// The poll timer for `period`.
///
/// `interval_at`, not `interval`: a plain `interval` fires its first tick
/// immediately, so the first poll would be attributable to the timer's
/// startup rather than to a drop or an elapsed interval.
///
/// One function, not two call sites: a reload that rebuilt the timer and
/// forgot `MissedTickBehavior::Delay` would leave a poll that ran long
/// firing a burst of catch-up ticks.
pub(super) fn poll_timer(period: Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    interval
}

/// Spawns one delivery task per firing, so [`run_loop`](crate::dog::bark::dog_lifecycle::run_loop)'s own `select!`
/// returns to reading the next event immediately rather than waiting on any
/// of them.
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
    let mut outcomes = Vec::with_capacity(firing.sinks.len());
    for name in &firing.sinks {
        let outcome = match delivery.sinks.get(name) {
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
        };
        outcomes.push(outcome);
    }
    bark.sinks = outcomes;

    let _guard = delivery.append_lock.lock().await;
    if let Err(err) = barks::append(&delivery.barks_path, &bark, delivery.max_bytes) {
        eprintln!("shep dog bark: could not record a fired bark: {err}");
    }
}

/// Wall-clock milliseconds since the Unix epoch.
///
/// [`Rules::on_event`](crate::dog::bark::rules::Rules::on_event) and [`Rules::on_poll`](crate::dog::bark::rules::Rules::on_poll) take a caller-supplied
/// timestamp so a test can fix it; this is the production caller.
pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
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

    /// Drives `deliver_and_record` directly rather than through
    /// `dog_lifecycle::run_loop`: the property belongs to that function, and the loop's
    /// event plumbing would need a second synchronization mechanism to
    /// know when a failed delivery finished.
    #[tokio::test]
    async fn a_bark_is_recorded_even_when_every_sink_refuses_it() {
        let (addr, _captured) = one_shot_sink(500, "refused").await;
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");

        let mut sinks = BTreeMap::new();
        sinks.insert("ops".to_owned(), json_sink(format!("http://{addr}/hook")));
        let firing = Firing {
            bark: Bark {
                at_ms: 1_000,
                rule: "gave_up".to_owned(),
                subject: "web".to_owned(),
                message: "web gave up: restart budget exhausted".to_owned(),
                sinks: Vec::new(),
            },
            sinks: vec!["ops".to_owned()],
        };

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
}
