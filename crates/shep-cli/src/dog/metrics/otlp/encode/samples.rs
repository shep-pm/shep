//! The channel samples a sheep or a dog sent on its shepherd channel, as
//! `metrics[]` entries: the same families
//! [`crate::dog::metrics::exposition::render`] appends to a scrape, in the
//! OTLP shape.
//!
//! The sender's kind decides `gauge` or `sum`, its help text is the
//! `description`, and its labels are attributes in the order the store built
//! them. Nothing here decides where in the payload the entries land; that is
//! [`super`].

use shep_core::protocol::{MetricType, ProcessInfo};

use crate::dog::metrics::grammar::REFUSED_TOTAL;
use crate::dog::metrics::samples::{SampleFamily, SamplePoint, sender_labels};

use super::payload::{
    KeyValue, Metric, Nanos, attr, cumulative_count_point, cumulative_double_point, cumulative_sum,
    double_point, gauge,
};

/// Nanoseconds in a second, the grid a sender's start is floored to. See
/// [`instance_start`].
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// One sender in the listing: the labels its points start with, and the
/// instant the instance that sent them started.
struct SenderStart {
    /// What [`sender_labels`] built for this sender. Every point the store
    /// hands over for it starts with exactly these.
    owned: Vec<(String, String)>,
    start: Nanos,
}

/// Every sender `flock` reports, with its instance start, so a point can be
/// read back to the process that sent it.
fn sender_starts(flock: &[ProcessInfo], now: Nanos) -> Vec<SenderStart> {
    flock
        .iter()
        .map(|info| SenderStart {
            owned: sender_labels(info),
            start: instance_start(info, now),
        })
        .collect()
}

/// The start of the sender `labels` belong to, or `None` for a point no
/// sender sent: the dog's own refusal counter, labelled by reason alone.
fn sender_start(labels: &[(String, String)], starts: &[SenderStart]) -> Option<Nanos> {
    starts
        .iter()
        .find(|sender| labels.starts_with(&sender.owned))
        .map(|sender| sender.start)
}

/// When `info`'s current instance started, floored to a whole second.
///
/// The listing carries `uptime_ms` and no absolute start, so the instant is
/// derived: this export's clock minus how long the process has been up. The
/// uptime is millisecond-truncated and measured by the shepherd, so the
/// derived instant carries noise a collector would read as a counter reset;
/// the floor puts that noise below a whole second.
fn instance_start(info: &ProcessInfo, now: Nanos) -> Nanos {
    let up_nano = info.uptime_ms.saturating_mul(1_000_000);
    Nanos::new(now.as_u64().saturating_sub(up_nano) / NANOS_PER_SECOND * NANOS_PER_SECOND)
}

/// One sample point's attributes: the sender's own labels, then the sample's
/// own, in the order the store built them.
fn sample_attributes<'a>(point: &'a SamplePoint) -> Vec<KeyValue<'a>> {
    point
        .labels
        .iter()
        .map(|(key, value)| attr(key, value.as_str()))
        .collect()
}

/// The samples OTLP can carry a number for.
///
/// The text exposition has its own spelling for a non-finite value (`NaN`,
/// `+Inf`) and OTLP/JSON has none, so the point is dropped rather than
/// encoded, the same rule a non-finite `cpu_percent` follows.
fn finite(points: &[SamplePoint]) -> impl Iterator<Item = &SamplePoint> {
    points.iter().filter(|point| point.value.is_finite())
}

/// One `metrics[]` entry per [`SampleFamily`], in the store's own name order,
/// the same order `render` appends them in.
///
/// A counter's `startTimeUnixNano` is the instant its sender's instance
/// started, read off `flock`, because OTLP reads a change of start as a
/// reset and a sender that restarts resets its own counters.
pub(super) fn sample_metrics<'a>(
    samples: &'a [SampleFamily],
    flock: &'a [ProcessInfo],
    exporter_start: Nanos,
    now: Nanos,
) -> Vec<Metric<'a>> {
    let starts = sender_starts(flock, now);
    samples
        .iter()
        .filter_map(|family| match family.kind {
            MetricType::Gauge => gauge(
                family.name.as_str(),
                family.help.as_deref(),
                finite(&family.points)
                    .map(|point| double_point(sample_attributes(point), now, point.value))
                    .collect(),
            ),
            MetricType::Counter => cumulative_sum(
                family.name.as_str(),
                family.help.as_deref(),
                finite(&family.points)
                    .map(|point| {
                        let attributes = sample_attributes(point);
                        let start = sender_start(&point.labels, &starts).unwrap_or(exporter_start);
                        // The dog's own refusal counter, named by the store
                        // rather than by a sender, and a count rather than a
                        // measurement. A sender cannot take this name: the
                        // grammar refuses it as reserved.
                        if family.name == REFUSED_TOTAL {
                            cumulative_count_point(attributes, start, now, point.value)
                        } else {
                            cumulative_double_point(attributes, start, now, point.value)
                        }
                    })
                    .collect(),
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests;
