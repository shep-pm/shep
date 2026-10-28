//! The channel samples the metrics dog keeps between scrapes.
//!
//! [`SampleStore`] holds the last sample per sender, name and label set, and
//! [`SampleStore::snapshot`] turns what is still live into [`SampleFamily`]
//! rows. Text is rendered from that data form and never from the store.
//!
//! A sender's samples go when its process stops or restarts, and so does a
//! sender the flock no longer reports as online, or reports as an instance
//! younger than the samples it is holding. That is what keeps a dead process
//! from reporting a value it stopped updating.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Mutex;
use std::time::Duration;

use shep_core::protocol::{ChildMessage, MetricType, ProcessInfo};
use shep_core::status::ProcStatus;
use tokio::time::Instant;

use super::grammar::{MAX_SERIES_PER_SENDER, REFUSED_TOTAL, Refusal, RefusalReason, refusal_for};

/// How much younger than its own samples a sender may be before a scrape
/// treats them as another instance's. One second covers the gap between a
/// spawn and the first sample a new instance sends, and nothing wider: a new
/// instance always started after the old one's last sample, so its uptime is
/// below that sample's age.
const STALE_MARGIN: Duration = Duration::from_secs(1);

/// One metric sample as it arrived on a sender's shepherd channel.
pub(crate) struct ChannelMetric {
    name: String,
    value: f64,
    kind: Option<MetricType>,
    /// Sorted, which is the order the wire's `BTreeMap` already gives them.
    labels: Vec<(String, String)>,
    help: Option<String>,
}

impl ChannelMetric {
    /// The sample `message` carries, or `None` for a channel message that is
    /// not one.
    pub(crate) fn from_child(message: ChildMessage) -> Option<Self> {
        match message {
            ChildMessage::Metric {
                name,
                value,
                metric_type,
                labels,
                help,
            } => Some(Self {
                name,
                value,
                kind: metric_type,
                labels: labels.into_iter().collect(),
                help,
            }),
            ChildMessage::Ready
            | ChildMessage::ActionReply { .. }
            | ChildMessage::LambLabel { .. }
            | ChildMessage::Note { .. } => None,
        }
    }
}

/// One metric name, with every retained sample that carries it.
///
/// The data form the exposition renders and the exporter will encode: one
/// `# HELP`/`# TYPE` block per name, and one point per sender and label set
/// beneath it.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleFamily {
    /// The metric name.
    pub name: String,
    /// Help text, taken from the first retained sample under this name that
    /// carried any. `None` renders no `# HELP` line.
    pub help: Option<String>,
    /// The kind every retained sample under this name agrees on, since a
    /// sample whose kind disagrees is refused at ingestion.
    pub kind: MetricType,
    /// One point per sender and label set.
    pub points: Vec<SamplePoint>,
}

/// One series inside a [`SampleFamily`].
#[derive(Debug, Clone, PartialEq)]
pub struct SamplePoint {
    /// The sender's own labels first, the way the built-in series carry
    /// theirs, then the sample's own labels in sorted order.
    pub labels: Vec<(String, String)>,
    /// The value the sender last sent for this series.
    pub value: f64,
}

/// The samples the metrics dog keeps between scrapes.
///
/// Shared as `Arc<SampleStore>` between the feed that fills it and the two
/// paths that read it. Every method takes the lock for its own pass and
/// releases it before returning, so no lock is ever held across an `await` and
/// a slow scraper cannot hold one open.
pub struct SampleStore {
    state: Mutex<StoreState>,
}

impl Default for SampleStore {
    fn default() -> Self {
        Self::new()
    }
}

// Hand-written: a derived `Debug` would print every retained label value and
// help string, which are app-supplied text. `shep_channel`'s own
// `MetricOptions` redacts the same two (IR-41).
impl fmt::Debug for SampleStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock().unwrap();
        f.debug_struct("SampleStore")
            .field("senders", &state.senders.len())
            .field("series", &state.series_held())
            .field("refused", &state.refused.values().sum::<u64>())
            .finish()
    }
}

/// What one sender retains: one sample per name and label set.
#[derive(Debug, Default)]
struct SenderEntry {
    series: BTreeMap<(String, Vec<(String, String)>), Series>,
}

/// One retained sample.
#[derive(Debug)]
struct Series {
    value: f64,
    kind: MetricType,
    help: Option<String>,
    /// When this retained value arrived.
    arrived: Instant,
}

impl SenderEntry {
    /// `since`: when the oldest sample this sender still holds arrived.
    ///
    /// Derived rather than stored: replacing a series moves which retained
    /// sample is the oldest, and a stored minimum could only drift older than
    /// the samples it is meant to describe.
    fn since(&self) -> Option<Instant> {
        self.series.values().map(|series| series.arrived).min()
    }
}

#[derive(Debug, Default)]
struct StoreState {
    senders: BTreeMap<u32, SenderEntry>,
    /// Every name currently retained, its kind, and how many series hold it,
    /// so a name is free for another kind once its last sample is gone.
    kinds: BTreeMap<String, (MetricType, u32)>,
    /// Counted at ingestion, once per refused message.
    refused: BTreeMap<RefusalReason, u64>,
    /// Which (sender, reason) pairs already have a log line.
    announced: BTreeSet<(u32, RefusalReason)>,
}

impl StoreState {
    fn series_held(&self) -> usize {
        self.senders.values().map(|entry| entry.series.len()).sum()
    }

    /// Counts one refusal and says whether it is the first of this reason
    /// from this sender.
    fn record_refusal(&mut self, sender: u32, name: &str, reason: RefusalReason) -> Refusal {
        *self.refused.entry(reason).or_insert(0) += 1;
        let first = self.announced.insert((sender, reason));
        Refusal::new(sender, name, reason, first)
    }

    /// Frees `sender`'s entry, and every name no other sender holds.
    fn drop_sender(&mut self, sender: u32) {
        let Some(entry) = self.senders.remove(&sender) else {
            return;
        };
        for (name, _) in entry.series.keys() {
            if let Some((_, held)) = self.kinds.get_mut(name) {
                *held = held.saturating_sub(1);
                if *held == 0 {
                    self.kinds.remove(name);
                }
            }
        }
        // A restarted sender that hits the same refusal again is worth
        // telling its author about again.
        self.announced.retain(|(other, _)| *other != sender);
    }
}

impl SampleStore {
    /// A store holding nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(StoreState::default()),
        }
    }

    /// Records `metric` from `sender`, replacing whatever that sender already
    /// holds under the same name and label set. The latest value wins.
    ///
    /// Help text is sticky: a sample that carries none keeps the text the same
    /// series had. A refusal is counted here, at ingestion, never per scrape.
    pub(crate) fn upsert(&self, sender: u32, metric: ChannelMetric) -> Option<Refusal> {
        let ChannelMetric {
            name,
            value,
            kind,
            labels,
            help,
        } = metric;
        let mut state = self.state.lock().unwrap();
        if let Some(reason) = refusal_for(&name, &labels, help.as_deref()) {
            return Some(state.record_refusal(sender, &name, reason));
        }
        let kind = kind.unwrap_or(MetricType::Gauge);
        if let Some((held, _)) = state.kinds.get(&name)
            && *held != kind
        {
            return Some(state.record_refusal(sender, &name, RefusalReason::TypeConflict));
        }
        let key = (name.clone(), labels);
        let fresh = !state
            .senders
            .get(&sender)
            .is_some_and(|entry| entry.series.contains_key(&key));
        if fresh
            && state
                .senders
                .get(&sender)
                .is_some_and(|entry| entry.series.len() >= MAX_SERIES_PER_SENDER)
        {
            return Some(state.record_refusal(sender, &name, RefusalReason::SeriesCap));
        }

        let entry = state.senders.entry(sender).or_default();
        let help = match (help, entry.series.get(&key)) {
            (None, Some(existing)) => existing.help.clone(),
            (help, _) => help,
        };
        entry.series.insert(
            key,
            Series {
                value,
                kind: kind.clone(),
                help,
                arrived: Instant::now(),
            },
        );
        if fresh {
            match state.kinds.get_mut(&name) {
                Some((_, held)) => *held += 1,
                None => {
                    state.kinds.insert(name, (kind, 1));
                }
            }
        }
        None
    }

    /// Frees everything `sender` holds.
    ///
    /// Called for the lifecycle events that mean the samples were sent by an
    /// instance that no longer exists.
    pub(crate) fn clear(&self, sender: u32) {
        self.state.lock().unwrap().drop_sender(sender);
    }

    /// Every retained sample, grouped into one [`SampleFamily`] per metric
    /// name, names in order.
    ///
    /// `flock` is the listing this scrape just took: it names the sender
    /// labels to render and decides what is still live. A sender absent from
    /// it, or whose uptime is more than [`STALE_MARGIN`] below the age of its
    /// oldest sample, is dropped rather than rendered.
    pub fn snapshot(&self, flock: &[ProcessInfo]) -> Vec<SampleFamily> {
        let mut state = self.state.lock().unwrap();
        let live: BTreeMap<u32, &ProcessInfo> = flock.iter().map(|info| (info.id, info)).collect();
        let now = Instant::now();
        let gone: Vec<u32> = state
            .senders
            .iter()
            .filter(|(sender, entry)| {
                let Some(info) = live.get(sender) else {
                    return true;
                };
                if info.status != ProcStatus::Online {
                    return true;
                }
                let Some(since) = entry.since() else {
                    return true;
                };
                now.saturating_duration_since(since)
                    > Duration::from_millis(info.uptime_ms).saturating_add(STALE_MARGIN)
            })
            .map(|(sender, _)| *sender)
            .collect();
        for sender in gone {
            state.drop_sender(sender);
        }

        let mut families: BTreeMap<String, SampleFamily> = BTreeMap::new();
        for (sender, entry) in &state.senders {
            // Unreachable after the sweep above, which drops every sender the
            // listing does not name.
            let Some(info) = live.get(sender) else {
                continue;
            };
            let owned = sender_labels(info);
            for ((name, labels), series) in &entry.series {
                let family = families
                    .entry(name.clone())
                    .or_insert_with(|| SampleFamily {
                        name: name.clone(),
                        help: series.help.clone(),
                        kind: series.kind.clone(),
                        points: Vec::new(),
                    });
                if family.help.is_none() {
                    family.help = series.help.clone();
                }
                let mut point = owned.clone();
                point.extend_from_slice(labels);
                family.points.push(SamplePoint {
                    labels: point,
                    value: series.value,
                });
            }
        }

        if !state.refused.is_empty() {
            let points = state
                .refused
                .iter()
                .map(|(reason, count)| SamplePoint {
                    labels: vec![("reason".to_string(), reason.label().to_string())],
                    value: *count as f64,
                })
                .collect();
            // No collision is possible: a sample named like this one is
            // refused as `reserved_name`.
            families.insert(
                REFUSED_TOTAL.to_string(),
                SampleFamily {
                    name: REFUSED_TOTAL.to_string(),
                    help: Some("Channel samples this dog refused, by reason.".to_string()),
                    kind: MetricType::Counter,
                    points,
                },
            );
        }
        families.into_values().collect()
    }
}

/// The labels a sender's own series carry, so a sample from `info` lines up
/// with the built-in series for the same process.
///
/// Shared with the exporter, which reads a point back to its sender by these
/// and so has to build them the same way this module does.
pub(crate) fn sender_labels(info: &ProcessInfo) -> Vec<(String, String)> {
    match &info.dog {
        Some(source) => vec![
            ("dog".to_string(), info.name.clone()),
            (
                "source".to_string(),
                <&'static str>::from(source).to_string(),
            ),
        ],
        None => vec![
            ("sheep".to_string(), info.name.clone()),
            ("id".to_string(), info.id.to_string()),
            ("fold".to_string(), info.fold.clone().unwrap_or_default()),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dog::metrics::HostReading;
    use crate::dog::metrics::exposition;
    use crate::dog::metrics::grammar;
    use shep_core::protocol::DogSource;

    /// An online sheep the snapshot can label, with an uptime long enough that
    /// no sample below reads as stale.
    fn sheep(id: u32, name: &str) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online)
            .uptime_ms(60_000)
            .fold(Some("backend".to_string()))
            .build()
    }

    /// An online dog, which the snapshot labels `dog=` and `source=`.
    fn dog(id: u32, name: &str) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online)
            .uptime_ms(60_000)
            .dog(Some(DogSource::BuiltIn { grants: None }))
            .build()
    }

    /// A gauge sample with no labels and no help text.
    fn metric(name: &str, value: f64) -> ChannelMetric {
        ChannelMetric {
            name: name.to_string(),
            value,
            kind: None,
            labels: Vec::new(),
            help: None,
        }
    }

    fn typed(name: &str, value: f64, kind: MetricType) -> ChannelMetric {
        ChannelMetric {
            kind: Some(kind),
            ..metric(name, value)
        }
    }

    fn labelled(name: &str, value: f64, labels: &[(&str, &str)]) -> ChannelMetric {
        ChannelMetric {
            labels: labels
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            ..metric(name, value)
        }
    }

    /// A full `Reading` carrying `store`'s snapshot over `flock`, so every
    /// assertion below reads the real exposition rather than the store.
    fn reading_with(store: &SampleStore, flock: &[ProcessInfo]) -> super::super::Reading {
        super::super::Reading {
            flock: flock.to_vec(),
            daemon_version: "9.9.9".to_string(),
            daemon_pid: 12345,
            host: Some(HostReading {
                memory_total_bytes: 16_000_000_000,
                memory_used_bytes: 8_000_000_000,
                processes: 200,
                uptime_seconds: 3600,
            }),
            samples: store.snapshot(flock),
        }
    }

    /// Refuses `sample` from `sender`, then asserts both halves of what a
    /// refusal does: the counter names the reason, and `needle`, what the
    /// sample would have rendered as, never reaches the scrape.
    fn assert_refused(
        store: &SampleStore,
        sender: u32,
        sample: ChannelMetric,
        reason: RefusalReason,
        needle: &str,
    ) {
        let refusal = store
            .upsert(sender, sample)
            .unwrap_or_else(|| panic!("{needle:?} must be refused"));
        assert_eq!(refusal.reason, reason);
        assert_eq!(refusal.sender, sender);
        let text = exposition::render(&reading_with(store, &[sheep(1, "web")]));
        assert!(
            text.contains(&format!(
                "{}{{reason=\"{}\"}} ",
                grammar::REFUSED_TOTAL,
                reason.label()
            )),
            "the refusal is not counted in the scrape: {text}"
        );
        assert!(
            !text.contains(needle),
            "the refused sample reached the scrape: {text}"
        );
    }

    #[test]
    fn a_metric_with_no_type_is_served_as_a_gauge() {
        let store = SampleStore::new();
        store.upsert(1, metric("rps", 12.0));
        let text = exposition::render(&reading_with(&store, &[sheep(1, "web")]));
        assert!(text.contains("# TYPE rps gauge"), "{text}");
        assert!(
            text.contains(r#"rps{sheep="web",id="1",fold="backend"} 12"#),
            "{text}"
        );
    }

    #[test]
    fn the_last_sample_per_sender_name_and_label_set_wins() {
        let store = SampleStore::new();
        store.upsert(1, metric("rps", 1.0));
        store.upsert(1, metric("rps", 7.5));
        let families = store.snapshot(&[sheep(1, "web")]);
        assert_eq!(families.len(), 1);
        assert_eq!(families[0].points.len(), 1, "one series, replaced in place");
        assert_eq!(families[0].points[0].value, 7.5);
    }

    #[test]
    fn two_label_sets_under_one_name_are_two_series() {
        let store = SampleStore::new();
        store.upsert(1, labelled("queue_depth", 1.0, &[("model", "qwen")]));
        store.upsert(1, labelled("queue_depth", 2.0, &[("model", "llama")]));

        let text = exposition::render(&reading_with(&store, &[sheep(1, "web")]));
        assert!(
            text.contains(r#"queue_depth{sheep="web",id="1",fold="backend",model="qwen"} 1"#),
            "{text}"
        );
        assert!(
            text.contains(r#"queue_depth{sheep="web",id="1",fold="backend",model="llama"} 2"#),
            "{text}"
        );
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("# TYPE queue_depth"))
                .count(),
            1,
            "two series of one name share one TYPE line: {text}"
        );
    }

    #[test]
    fn a_sender_is_labelled_the_way_its_own_series_are() {
        let store = SampleStore::new();
        store.upsert(1, metric("rps", 1.0));
        store.upsert(2, metric("rps", 3.0));
        let text = exposition::render(&reading_with(&store, &[sheep(1, "web"), dog(2, "bark")]));
        assert!(
            text.contains(r#"rps{sheep="web",id="1",fold="backend"} 1"#),
            "a sheep's sample carries the sheep, id and fold its own series carry: {text}"
        );
        assert!(
            text.contains(r#"rps{dog="bark",source="built-in"} 3"#),
            "a dog's sample carries the dog and source its own series carry: {text}"
        );
    }

    #[test]
    fn a_sample_named_like_a_shep_series_is_refused() {
        let store = SampleStore::new();
        assert_refused(
            &store,
            1,
            metric("shep_mine", 1.0),
            RefusalReason::ReservedName,
            "shep_mine",
        );
        assert_refused(
            &store,
            1,
            metric("shep_theirs", 1.0),
            RefusalReason::ReservedName,
            "shep_theirs",
        );
        // Counted once per refused message, and a scrape never adds to it.
        let text = exposition::render(&reading_with(&store, &[sheep(1, "web")]));
        assert!(
            text.contains(&format!(
                "{}{{reason=\"reserved_name\"}} 2",
                grammar::REFUSED_TOTAL
            )),
            "{text}"
        );
        assert_eq!(
            text,
            exposition::render(&reading_with(&store, &[sheep(1, "web")])),
            "a second scrape must not count anything again"
        );
    }

    #[test]
    fn a_sample_with_an_invalid_name_is_refused() {
        let store = SampleStore::new();
        for name in ["9lives", "r ps", "r-ps", "\u{e9}lan", "queue-depth"] {
            assert_refused(
                &store,
                1,
                metric(name, 1.0),
                RefusalReason::InvalidName,
                name,
            );
        }
        let too_long = "n".repeat(grammar::MAX_NAME_BYTES + 1);
        assert_refused(
            &store,
            1,
            metric(&too_long, 1.0),
            RefusalReason::InvalidName,
            &too_long,
        );

        // An empty name is refused rather than rendered as a nameless series,
        // which is the one refusal with no text of its own to look for.
        let empty = SampleStore::new();
        let refusal = empty
            .upsert(1, metric("", 1.0))
            .expect("an empty name must be refused");
        assert_eq!(refusal.reason, RefusalReason::InvalidName);
        let text = exposition::render(&reading_with(&empty, &[sheep(1, "web")]));
        assert!(
            text.contains(&format!(
                "{}{{reason=\"invalid_name\"}} 1",
                grammar::REFUSED_TOTAL
            )),
            "{text}"
        );
        assert_eq!(
            text.lines()
                .filter(|line| !line.starts_with('#') && !line.starts_with("shep_"))
                .count(),
            0,
            "an empty name rendered a nameless series: {text}"
        );
    }

    #[test]
    fn a_sample_with_an_invalid_label_name_is_refused() {
        let store = SampleStore::new();
        for key in ["9key", "a key", "a-key", "", "__internal"] {
            assert_refused(
                &store,
                1,
                labelled("queue_depth", 1.0, &[(key, "v")]),
                RefusalReason::InvalidLabel,
                "queue_depth",
            );
        }
        assert_refused(
            &store,
            1,
            labelled(
                "queue_depth",
                1.0,
                &[("k".repeat(grammar::MAX_LABEL_NAME_BYTES + 1).as_str(), "v")],
            ),
            RefusalReason::InvalidLabel,
            "queue_depth",
        );
    }

    #[test]
    fn a_sample_using_a_sender_label_is_refused() {
        let store = SampleStore::new();
        for key in grammar::SENDER_LABELS {
            assert_refused(
                &store,
                1,
                labelled("queue_depth", 1.0, &[(key, "web")]),
                RefusalReason::ReservedLabel,
                "queue_depth",
            );
        }
    }

    #[test]
    fn a_sample_over_one_of_the_caps_is_refused() {
        let store = SampleStore::new();
        let keys: Vec<String> = (0..=grammar::MAX_LABELS).map(|i| format!("k{i}")).collect();
        let too_many: Vec<(&str, &str)> = keys.iter().map(|key| (key.as_str(), "v")).collect();
        assert_refused(
            &store,
            1,
            labelled("queue_depth", 1.0, &too_many),
            RefusalReason::TooLong,
            "queue_depth",
        );
        assert_refused(
            &store,
            1,
            labelled(
                "queue_depth",
                1.0,
                &[(
                    "model",
                    "v".repeat(grammar::MAX_LABEL_VALUE_BYTES + 1).as_str(),
                )],
            ),
            RefusalReason::TooLong,
            "queue_depth",
        );
        let mut long_help = metric("queue_depth", 1.0);
        long_help.help = Some("h".repeat(grammar::MAX_HELP_BYTES + 1));
        assert_refused(&store, 1, long_help, RefusalReason::TooLong, "queue_depth");
    }

    #[test]
    fn a_new_series_from_a_sender_at_the_cap_is_refused_and_an_old_one_is_kept() {
        let store = SampleStore::new();
        for i in 0..grammar::MAX_SERIES_PER_SENDER {
            let slot = i.to_string();
            assert_eq!(
                store.upsert(1, labelled("queue_depth", 1.0, &[("slot", &slot)])),
                None,
                "series {i} must be accepted"
            );
        }
        assert_eq!(
            store.upsert(1, labelled("queue_depth", 2.0, &[("slot", "0")])),
            None,
            "a sample for a series already held is not a new one"
        );
        assert_refused(
            &store,
            1,
            labelled("queue_depth", 3.0, &[("slot", "spare")]),
            RefusalReason::SeriesCap,
            r#"slot="spare""#,
        );

        let text = exposition::render(&reading_with(&store, &[sheep(1, "web")]));
        assert!(
            text.contains(r#"queue_depth{sheep="web",id="1",fold="backend",slot="0"} 2"#),
            "the stable series must survive the cap: {text}"
        );
        // The cap is per sender: a second sender is not blocked by the first.
        assert_eq!(
            store.upsert(2, labelled("queue_depth", 4.0, &[("slot", "spare")])),
            None
        );
    }

    #[test]
    fn a_sample_whose_kind_disagrees_with_the_name_is_refused() {
        let store = SampleStore::new();
        assert_eq!(
            store.upsert(1, typed("requests", 1.0, MetricType::Gauge)),
            None
        );
        assert_refused(
            &store,
            1,
            typed("requests", 2.0, MetricType::Counter),
            RefusalReason::TypeConflict,
            "# TYPE requests counter",
        );

        // Absent means gauge, so it agrees with the gauge already held and
        // replaces it rather than conflicting.
        assert_eq!(store.upsert(1, metric("requests", 3.0)), None);
        let families = store.snapshot(&[sheep(1, "web")]);
        assert_eq!(families[0].kind, MetricType::Gauge);
        assert_eq!(families[0].points[0].value, 3.0);
    }

    #[test]
    fn a_refusal_is_announced_once_per_sender_and_reason() {
        let store = SampleStore::new();
        assert!(
            store.upsert(1, metric("shep_own", 1.0)).unwrap().first,
            "the first refusal of a reason from a sender is the one worth a line"
        );
        assert!(
            !store.upsert(1, metric("shep_own", 2.0)).unwrap().first,
            "one line per sender and reason, not per message"
        );
        assert!(
            store.upsert(2, metric("shep_own", 1.0)).unwrap().first,
            "a different author has to be told too"
        );
        assert!(
            store.upsert(1, metric("bad name", 1.0)).unwrap().first,
            "a different reason is a different line"
        );
    }

    /// A paused clock, because the thing under test is how old a sample is.
    /// Nothing here touches a socket, which is what a paused clock is safe
    /// against.
    #[tokio::test(start_paused = true)]
    async fn a_sample_older_than_the_current_instance_is_dropped_at_scrape() {
        let store = SampleStore::new();
        store.upsert(1, metric("rps", 9.0));
        store.upsert(2, metric("rps", 8.0));

        // No event was delivered for either sender: this is the gap the
        // scrape-time guard exists to cover.
        tokio::time::sleep(Duration::from_secs(30)).await;

        // Sender 1 is an instance younger than the sample it holds, so that
        // sample is another instance's. Sender 2 has been up longer than its
        // sample has been held, which is the live case.
        let young = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .uptime_ms(1_000)
            .build();
        let long_lived = ProcessInfo::builder(2, "api", ProcStatus::Online)
            .uptime_ms(120_000)
            .build();
        let families = store.snapshot(&[young, long_lived]);
        assert_eq!(families.len(), 1, "{families:?}");
        assert_eq!(families[0].points.len(), 1);
        assert_eq!(
            families[0].points[0].labels[0],
            ("sheep".to_string(), "api".to_string())
        );

        // Dropping is not a one-scrape read: the stale entry is freed.
        assert_eq!(
            format!("{store:?}"),
            "SampleStore { senders: 1, series: 1, refused: 0 }"
        );
    }

    #[test]
    fn a_sender_missing_from_the_flock_is_dropped_and_its_entry_freed() {
        let store = SampleStore::new();
        store.upsert(1, metric("rps", 9.0));
        store.upsert(3, metric("rps", 8.0));

        // Only sender 1 is in the listing.
        let text = exposition::render(&reading_with(&store, &[sheep(1, "web")]));
        assert!(
            text.contains(r#"rps{sheep="web",id="1",fold="backend"} 9"#),
            "{text}"
        );
        assert_eq!(
            text.lines().filter(|line| line.starts_with("rps{")).count(),
            1,
            "the sender the listing does not name renders nothing: {text}"
        );

        // Sender 3's entry is freed rather than parked for the next scrape.
        assert_eq!(
            format!("{store:?}"),
            "SampleStore { senders: 1, series: 1, refused: 0 }"
        );

        // A sender the listing reports as not online is dropped the same way.
        let mut parked = sheep(1, "web");
        parked.status = ProcStatus::WaitingRestart;
        assert!(store.snapshot(&[parked]).is_empty());
        assert_eq!(
            format!("{store:?}"),
            "SampleStore { senders: 0, series: 0, refused: 0 }"
        );
    }

    #[test]
    fn a_freed_name_is_free_for_the_other_kind() {
        let store = SampleStore::new();
        store.upsert(1, typed("requests", 1.0, MetricType::Gauge));
        store.clear(1);
        assert_eq!(
            store.upsert(2, typed("requests", 2.0, MetricType::Counter)),
            None,
            "nothing holds the name any more, so a counter may take it"
        );
    }

    #[test]
    fn help_text_is_sticky_across_a_sample_that_carries_none() {
        let store = SampleStore::new();
        let mut with_help = metric("queue_depth", 1.0);
        with_help.help = Some("Requests waiting for the lease.".to_string());
        store.upsert(1, with_help);
        store.upsert(1, metric("queue_depth", 2.0));

        let text = exposition::render(&reading_with(&store, &[sheep(1, "web")]));
        assert!(
            text.contains("# HELP queue_depth Requests waiting for the lease."),
            "{text}"
        );
    }

    /// IR-41: pins the exact string, and separately that no label value or
    /// help text appears in it.
    #[test]
    fn the_store_debug_counts_and_never_prints_sample_text() {
        let store = SampleStore::new();
        let mut secret = labelled("queue_depth", 1.0, &[("model", "SECRET-LABEL-VALUE")]);
        secret.help = Some("SECRET HELP TEXT".to_string());
        store.upsert(1, secret);
        store.upsert(1, metric("shep_own", 1.0));

        let rendered = format!("{store:?}");
        assert_eq!(
            rendered,
            "SampleStore { senders: 1, series: 1, refused: 1 }"
        );
        assert!(!rendered.contains("SECRET"), "{rendered}");
        assert!(!rendered.contains("queue_depth"), "{rendered}");
    }

    /// The snapshot is the data form, so the exporter #736 encodes the same
    /// grouping the scrape renders.
    #[test]
    fn the_snapshot_groups_two_senders_of_one_name_into_one_family() {
        let store = SampleStore::new();
        let mut helped = metric("requests", 1.0);
        helped.help = Some("Requests served.".to_string());
        store.upsert(1, helped);
        store.upsert(2, metric("requests", 2.0));

        let families = store.snapshot(&[sheep(1, "web"), dog(2, "bark")]);
        assert_eq!(families.len(), 1);
        let family = &families[0];
        assert_eq!(family.name, "requests");
        assert_eq!(family.kind, MetricType::Gauge);
        assert_eq!(family.help.as_deref(), Some("Requests served."));
        assert_eq!(family.points.len(), 2);
    }

    /// The counter the dog keeps of its own refusals is a family of its own,
    /// carried in the same list as the retained samples. A test that reads
    /// `snapshot` as "the samples" has to say so, which is what CI caught on
    /// the feed's name assertion.
    #[test]
    fn the_refusal_counter_rides_in_the_snapshot_beside_the_samples() {
        let store = SampleStore::new();
        store.upsert(1, metric("bad name", 7.0));
        store.upsert(1, metric("rps", 7.0));

        let families = store.snapshot(&[sheep(1, "web")]);
        let names: Vec<&str> = families.iter().map(|family| family.name.as_str()).collect();
        assert_eq!(names, vec!["rps", REFUSED_TOTAL]);

        let counter = families.iter().find(|f| f.name == REFUSED_TOTAL).unwrap();
        assert_eq!(counter.kind, MetricType::Counter);
        assert_eq!(counter.points.len(), 1);
        assert_eq!(
            counter.points[0].labels,
            vec![("reason".into(), "invalid_name".into())]
        );
        assert_eq!(counter.points[0].value, 1.0, "one refusal, counted once");
    }

    /// The counter belongs to the dog rather than to a sender, so it survives
    /// every clear and outlives an empty flock. "The snapshot is empty" is
    /// therefore not the test for "no samples are held".
    #[test]
    fn the_refusal_counter_outlives_every_sample_and_every_clear() {
        let store = SampleStore::new();
        store.upsert(1, metric("bad name", 7.0));
        store.upsert(1, metric("rps", 7.0));
        store.clear(1);

        let families = store.snapshot(&[sheep(1, "web")]);
        assert_eq!(families.len(), 1);
        assert_eq!(families[0].name, REFUSED_TOTAL);
        assert!(store.snapshot(&[]).iter().any(|f| f.name == REFUSED_TOTAL));
    }
}
