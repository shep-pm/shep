//! The OTLP/JSON payload shape, as types: `ExportMetricsServiceRequest`
//! with its `resourceMetrics`, `scopeMetrics` and `metrics` nesting, and
//! the `gauge`/`sum` pair that is how OTLP spells a metric's type in JSON.
//!
//! Where the shape differs from plain serde: 64-bit integers are decimal
//! strings ([`Nanos`], [`IntStr`]), doubles are numbers, and
//! `aggregationTemporality` is the integer `2` rather than a name.
//!
//! Nothing here knows what a sheep is: [`super`] builds the series from a
//! `Reading`, and [`super::samples`] from what a sender sent on its channel.

use std::borrow::Cow;

use serde::Serialize;

/// `aggregationTemporality.CUMULATIVE`, the only OTLP enum this mapping
/// writes, and the one its JSON form carries as an integer.
const TEMPORALITY_CUMULATIVE: u8 = 2;

/// A 64-bit nanosecond timestamp, on the wire as a decimal string: OTLP's
/// JSON mapping spells `timeUnixNano` and `startTimeUnixNano` as strings
/// because a 64-bit count of nanos is not exact as a JSON number.
#[derive(Debug, Clone, Copy)]
pub(super) struct Nanos(u64);

impl Nanos {
    /// Wrap one nanosecond stamp.
    pub(super) const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The stamp as the number it wraps, for a mapping that has to do
    /// arithmetic on one. What it serializes as stays a decimal string.
    pub(super) const fn as_u64(self) -> u64 {
        self.0
    }
}

impl Serialize for Nanos {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

/// A 64-bit integer point value, on the wire as a decimal string, for the
/// same reason as [`Nanos`].
#[derive(Debug, Clone, Copy)]
struct IntStr(u64);

impl Serialize for IntStr {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

/// `resourceMetrics[]`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExportRequest<'a> {
    resource_metrics: Vec<ResourceMetrics<'a>>,
}

/// One `resourceMetrics` entry: what produced the telemetry, and the
/// instrumentation that produced it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResourceMetrics<'a> {
    resource: Resource<'a>,
    scope_metrics: Vec<ScopeMetrics<'a>>,
}

/// `resource.attributes`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Resource<'a> {
    attributes: Vec<KeyValue<'a>>,
}

/// `scopeMetrics[]`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScopeMetrics<'a> {
    scope: Scope<'a>,
    metrics: Vec<Metric<'a>>,
}

/// `scope{name,version}`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Scope<'a> {
    name: &'a str,
    version: &'a str,
}

/// `metrics[]`. Exactly one of `gauge` or `sum` is set, which is how OTLP
/// spells a metric's type in JSON.
///
/// `name` and `description` are `Cow` because a channel sample's are
/// app-supplied text the store holds as `String`, while every built-in
/// series names itself with a `&'static str`.
///
/// `unit` is left out. A Prometheus-side collector appends `_bytes` to a
/// name that already ends in it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Metric<'a> {
    name: Cow<'a, str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gauge: Option<Gauge<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sum: Option<Sum<'a>>,
}

/// `gauge{dataPoints}`: a value read at an instant.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Gauge<'a> {
    data_points: Vec<DataPoint<'a>>,
}

/// `sum{dataPoints,aggregationTemporality,isMonotonic}`: a count that only
/// ever grows, reset only when this dog restarts.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Sum<'a> {
    data_points: Vec<DataPoint<'a>>,
    aggregation_temporality: u8,
    is_monotonic: bool,
}

/// One `dataPoints[]` entry. Exactly one of `as_int` or `as_double` is set.
///
/// `startTimeUnixNano` is present on the cumulative sum and absent on every
/// gauge: OTLP gives a gauge no start, and a sum without one has nothing to
/// difference against.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DataPoint<'a> {
    attributes: Vec<KeyValue<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_time_unix_nano: Option<Nanos>,
    time_unix_nano: Nanos,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_int: Option<IntStr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_double: Option<f64>,
}

/// One attribute: `{key,value}`, every value a string, matching the
/// Prometheus label of the same name.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct KeyValue<'a> {
    key: &'a str,
    value: StringValue<'a>,
}

/// `value{stringValue}`.
///
/// A `Cow` because a label's value is usually a `&str` borrowed from the
/// reading, and sometimes a `String` this function built: a sheep's numeric
/// id, which no `Reading` field holds as text.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StringValue<'a> {
    string_value: Cow<'a, str>,
}

/// One `{key,value}` pair.
pub(super) fn attr<'a>(key: &'a str, value: impl Into<Cow<'a, str>>) -> KeyValue<'a> {
    KeyValue {
        key,
        value: StringValue {
            string_value: value.into(),
        },
    }
}

/// One gauge point: an instant's value, with no start time.
pub(super) fn gauge_point<'a>(
    attributes: Vec<KeyValue<'a>>,
    now: Nanos,
    value: u64,
) -> DataPoint<'a> {
    DataPoint {
        attributes,
        start_time_unix_nano: None,
        time_unix_nano: now,
        as_int: Some(IntStr(value)),
        as_double: None,
    }
}

/// One gauge point carrying a rate.
pub(super) fn double_point<'a>(
    attributes: Vec<KeyValue<'a>>,
    now: Nanos,
    value: f64,
) -> DataPoint<'a> {
    DataPoint {
        attributes,
        start_time_unix_nano: None,
        time_unix_nano: now,
        as_int: None,
        as_double: Some(value),
    }
}

/// One cumulative monotonic sum point, started at `start`: the instant the
/// counter began accumulating, which is the caller's to know.
pub(super) fn cumulative_point<'a>(
    attributes: Vec<KeyValue<'a>>,
    start: Nanos,
    now: Nanos,
    value: u64,
) -> DataPoint<'a> {
    DataPoint {
        attributes,
        start_time_unix_nano: Some(start),
        time_unix_nano: now,
        as_int: Some(IntStr(value)),
        as_double: None,
    }
}

/// One cumulative monotonic sum point carrying a channel sample's `f64`.
pub(super) fn cumulative_double_point<'a>(
    attributes: Vec<KeyValue<'a>>,
    start: Nanos,
    now: Nanos,
    value: f64,
) -> DataPoint<'a> {
    DataPoint {
        attributes,
        start_time_unix_nano: Some(start),
        time_unix_nano: now,
        as_int: None,
        as_double: Some(value),
    }
}

/// One cumulative monotonic sum point carrying a count.
///
/// The value reached the store as an `f64`, because that is what the wire
/// carries, but a count was a `u64` before it was one, and `asInt` is what
/// the built-in counters use for the same reason.
pub(super) fn cumulative_count_point<'a>(
    attributes: Vec<KeyValue<'a>>,
    start: Nanos,
    now: Nanos,
    value: f64,
) -> DataPoint<'a> {
    DataPoint {
        attributes,
        start_time_unix_nano: Some(start),
        time_unix_nano: now,
        // A count this dog only ever grew from 0, so a negative or
        // over-large value cannot come from it; saturating is what keeps one
        // a big number rather than a wrapped small one.
        as_int: Some(IntStr(value.max(0.0) as u64)),
        as_double: None,
    }
}

/// A `metrics[]` gauge entry, or nothing at all when it has no points, which
/// is how `render` drops a group with no series.
///
/// `description` is `Option` because a channel sample carries help text only
/// if its sender wrote any, and `render` writes no `# HELP` line for a family
/// that has none. Every built-in series names its own.
pub(super) fn gauge<'a>(
    name: impl Into<Cow<'a, str>>,
    description: Option<impl Into<Cow<'a, str>>>,
    data_points: Vec<DataPoint<'a>>,
) -> Option<Metric<'a>> {
    (!data_points.is_empty()).then_some(Metric {
        name: name.into(),
        description: description.map(Into::into),
        gauge: Some(Gauge { data_points }),
        sum: None,
    })
}

/// A `metrics[]` cumulative monotonic sum entry, or nothing at all when it
/// has no points.
pub(super) fn cumulative_sum<'a>(
    name: impl Into<Cow<'a, str>>,
    description: Option<impl Into<Cow<'a, str>>>,
    data_points: Vec<DataPoint<'a>>,
) -> Option<Metric<'a>> {
    (!data_points.is_empty()).then_some(Metric {
        name: name.into(),
        description: description.map(Into::into),
        gauge: None,
        sum: Some(Sum {
            data_points,
            aggregation_temporality: TEMPORALITY_CUMULATIVE,
            is_monotonic: true,
        }),
    })
}

/// The one `resourceMetrics` entry this dog sends, filled in.
///
/// The envelope is built here rather than in the caller so its nesting stays
/// private to the shape it belongs to.
pub(super) fn export_request<'a>(
    resource_attributes: Vec<KeyValue<'a>>,
    scope_name: &'a str,
    scope_version: &'a str,
    metrics: Vec<Metric<'a>>,
) -> ExportRequest<'a> {
    ExportRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: Resource {
                attributes: resource_attributes,
            },
            scope_metrics: vec![ScopeMetrics {
                scope: Scope {
                    name: scope_name,
                    version: scope_version,
                },
                metrics,
            }],
        }],
    }
}
