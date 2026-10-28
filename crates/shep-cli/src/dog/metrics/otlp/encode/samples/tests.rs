//! The channel-sample half of the payload: what [`super::sample_metrics`]
//! makes of the [`SampleFamily`](crate::dog::metrics::samples::SampleFamily)
//! values a sheep or a dog sent on its channel, pinned as exact JSON.
//!
//! The fixtures come from [`super::tests`], the module that reads the built-in
//! series through them too.

use shep_core::protocol::{MetricType, ProcessInfo};
use shep_core::status::ProcStatus;

use crate::dog::metrics::Reading;
use crate::dog::metrics::grammar::REFUSED_TOTAL;
use crate::dog::metrics::otlp::ExportContext;

use super::super::tests::{
    DOG_START, NOW, SHEEP_START, START, attributes_of, context, counter_start, family,
    metrics_by_name, metrics_with, point, reading, sheep_up, with_samples,
};

/// A sample the sender reported as a gauge is an OTLP gauge, and its
/// labels arrive in the order the exposition renders them: the sender's
/// own first, the sample's own after.
#[test]
fn a_gauge_sample_exports_as_a_gauge_with_its_labels_in_order() {
    let metrics = metrics_by_name(&with_samples(vec![family(
        "queue_depth",
        MetricType::Gauge,
        Some("Jobs waiting."),
        vec![point(
            &[
                ("sheep", "web"),
                ("id", "3"),
                ("fold", "backend"),
                ("model", "qwen"),
            ],
            7.5,
        )],
    )]));
    let metric = &metrics["queue_depth"];
    assert!(
        metric.get("sum").is_none(),
        "a gauge sample is not a sum: {metric}"
    );
    assert!(
        metric.get("unit").is_none(),
        "the payload names no unit for a sample either: {metric}"
    );

    let points = metric["gauge"]["dataPoints"]
        .as_array()
        .expect("dataPoints");
    assert_eq!(points.len(), 1, "{metric}");
    assert_eq!(
        attributes_of(&points[0]),
        vec![
            ("sheep", "web"),
            ("id", "3"),
            ("fold", "backend"),
            ("model", "qwen"),
        ]
    );
    // A sample is an `f64` on the wire, so its point is an `asDouble`
    // even when the value happens to be a whole number.
    assert_eq!(points[0]["asDouble"], serde_json::Value::from(7.5f64));
    assert!(points[0].get("asInt").is_none(), "{}", points[0]);
    assert_eq!(
        points[0]["timeUnixNano"],
        serde_json::Value::String(NOW.to_string())
    );
    assert!(
        points[0].get("startTimeUnixNano").is_none(),
        "OTLP gives a gauge no start: {}",
        points[0]
    );
}

/// A sample the sender reported as a counter is a monotonic cumulative sum,
/// so a collector differences two exports instead of reading one instant. Its
/// `startTimeUnixNano` is the instant the sending process started, read from
/// the listing's `uptime_ms`, and so a different sender carries a different
/// start: OTLP reads a change of start as a reset, and a sender that restarts
/// must move it.
#[test]
fn a_counter_sample_exports_as_a_monotonic_cumulative_sum() {
    let metrics = metrics_by_name(&with_samples(vec![family(
        "requests_total",
        MetricType::Counter,
        Some("Requests served."),
        vec![
            point(&[("sheep", "web"), ("id", "3"), ("fold", "backend")], 4.0),
            point(&[("dog", "bark"), ("source", "built-in")], 2.0),
        ],
    )]));
    let metric = &metrics["requests_total"];
    assert!(
        metric.get("gauge").is_none(),
        "a counter sample is not a gauge: {metric}"
    );

    let sum = metric["sum"]
        .as_object()
        .expect("a counter sample is a sum");
    assert_eq!(
        sum["aggregationTemporality"],
        serde_json::Value::from(2u8),
        "{sum:?}"
    );
    assert_eq!(sum["isMonotonic"], serde_json::Value::Bool(true));

    let points = sum["dataPoints"].as_array().expect("dataPoints");
    assert_eq!(points.len(), 2, "{metric}");
    assert_eq!(
        attributes_of(&points[1]),
        vec![("dog", "bark"), ("source", "built-in")]
    );
    // One start per sender, from that sender's own uptime: the sheep has
    // been up 65 seconds and the dog 30, so their counters did not start
    // accumulating at the same instant.
    let starts: Vec<&str> = points
        .iter()
        .map(|point| {
            point["startTimeUnixNano"]
                .as_str()
                .expect("a cumulative sum carries a start")
        })
        .collect();
    assert_eq!(
        starts,
        vec![SHEEP_START.to_string(), DOG_START.to_string(),]
    );
    for point in points {
        assert_eq!(
            point["timeUnixNano"],
            serde_json::Value::String(NOW.to_string()),
            "{point}"
        );
        assert!(
            point["asDouble"].is_number(),
            "a sample is an f64 whatever its kind: {point}"
        );
        assert!(point.get("asInt").is_none(), "{point}");
    }
    assert_eq!(points[0]["asDouble"], serde_json::Value::from(4.0f64));
    assert_eq!(points[1]["asDouble"], serde_json::Value::from(2.0f64));
}

/// The defect a per-exporter start cannot express: OTLP reads a change of
/// `startTimeUnixNano` as the counter having reset, and a sheep that
/// restarts resets its own counters. The value dropping from 1000 to 3 with
/// the start unmoved is a cumulative sum that goes backwards, and a
/// collector that differences two exports gets a negative delta out of it.
#[test]
fn a_senders_restart_moves_the_start_of_its_counter() {
    let long_lived = ProcessInfo::builder(3, "web", ProcStatus::Online)
        .uptime_ms(600_000)
        .fold(Some("backend".to_string()))
        .build();
    let before = Reading {
        flock: vec![long_lived],
        samples: vec![family(
            "requests_total",
            MetricType::Counter,
            None,
            vec![point(
                &[("sheep", "web"), ("id", "3"), ("fold", "backend")],
                1000.0,
            )],
        )],
        ..reading()
    };

    // The same sheep name, a new instance 10 seconds later, up for 2
    // seconds, its counter back at 3.
    let fresh = ProcessInfo::builder(4, "web", ProcStatus::Online)
        .uptime_ms(2_000)
        .fold(Some("backend".to_string()))
        .build();
    let after = Reading {
        flock: vec![fresh],
        samples: vec![family(
            "requests_total",
            MetricType::Counter,
            None,
            vec![point(
                &[("sheep", "web"), ("id", "4"), ("fold", "backend")],
                3.0,
            )],
        )],
        ..reading()
    };
    let later = ExportContext {
        now_unix_nano: NOW + 10_000_000_000,
        ..context()
    };

    let (first, second) = (
        counter_start(&metrics_with(&before, &context())["requests_total"]),
        counter_start(&metrics_with(&after, &later)["requests_total"]),
    );
    assert_eq!(
        first,
        NOW - 600_000_000_000,
        "the first export starts at the 600 seconds the instance had been up"
    );
    assert_eq!(
        second,
        NOW + 10_000_000_000 - 2_000_000_000,
        "the second starts at the 2 seconds the new instance has been up"
    );
    assert!(
        second > first,
        "a restart has to move the start, or the drop from 1000 to 3 reads as \
         nothing: first {first}, second {second}"
    );
}

/// The other half of the same rule: for one instance the start must not
/// move, because a start that changes between two exports of a counter that
/// never reset reads to a collector as a reset too.
///
/// `uptime_ms` is millisecond-truncated and measured by the shepherd, so
/// the derived instant carries noise. The start is floored to a second so
/// that noise cannot move it.
#[test]
fn one_instances_counter_start_does_not_move_between_exports() {
    let samples = || {
        vec![family(
            "requests_total",
            MetricType::Counter,
            None,
            vec![point(
                &[("sheep", "web"), ("id", "3"), ("fold", "backend")],
                4.0,
            )],
        )]
    };

    // One instance, read at three moments: at start+65s, at start+80s, and
    // again at start+80s with the listing's uptime one millisecond short of
    // the clock the export stamps, which is the ordinary case.
    let later = ExportContext {
        now_unix_nano: NOW + 15_000_000_000,
        ..context()
    };
    let exports = [
        (sheep_up(65_000), context()),
        (sheep_up(80_000), later),
        (sheep_up(79_999), later),
    ];
    let starts: Vec<u64> = exports
        .iter()
        .map(|(sheep, export)| {
            let reading = Reading {
                flock: vec![sheep.clone()],
                samples: samples(),
                ..reading()
            };
            counter_start(&metrics_with(&reading, export)["requests_total"])
        })
        .collect();

    assert_eq!(
        starts, [SHEEP_START; 3],
        "one instance, one start, whatever the millisecond of noise between \
         the listing and the export"
    );
}

/// The floor that buys that stability, pinned: an instance that started
/// mid-second is stamped at the second below, not at an instant that moves
/// with the export that reads it.
#[test]
fn a_counter_start_is_floored_to_a_whole_second() {
    let metrics = metrics_by_name(&Reading {
        flock: vec![sheep_up(65_432)],
        samples: vec![family(
            "requests_total",
            MetricType::Counter,
            None,
            vec![point(
                &[("sheep", "web"), ("id", "3"), ("fold", "backend")],
                4.0,
            )],
        )],
        ..reading()
    });
    assert_eq!(
        counter_start(&metrics["requests_total"]),
        NOW - 66_000_000_000,
        "65.432 seconds of uptime started at the second below"
    );
}

/// The dog's own refusal counter belongs to the dog rather than to any
/// sender, and the dog restarting restarts the exporter with it. It keeps
/// the exporter's start, which is also the stamp `shep_sheep_restart_total`
/// carries.
#[test]
fn the_refusal_counter_keeps_the_exporters_start() {
    let metrics = metrics_by_name(&with_samples(vec![
        family(
            "requests_total",
            MetricType::Counter,
            None,
            vec![point(
                &[("sheep", "web"), ("id", "3"), ("fold", "backend")],
                4.0,
            )],
        ),
        family(
            REFUSED_TOTAL,
            MetricType::Counter,
            None,
            vec![point(&[("reason", "invalid_name")], 1.0)],
        ),
    ]));
    assert_eq!(
        counter_start(&metrics[REFUSED_TOTAL]),
        START,
        "the dog's own counter starts when the dog did"
    );
    assert_eq!(
        counter_start(&metrics["requests_total"]),
        SHEEP_START,
        "and a sender's counter starts when its sender did"
    );
}

/// A sample's help text is the payload's `description`, and a family the
/// sender never gave any has no `description` at all: an empty one tells a
/// collector nothing, the way a family with no help writes no `# HELP`
/// line in the exposition.
#[test]
fn a_sample_help_becomes_the_description_and_no_help_omits_it() {
    let metrics = metrics_by_name(&with_samples(vec![
        family(
            "queue_depth",
            MetricType::Gauge,
            Some("Jobs waiting."),
            vec![point(&[("model", "qwen")], 1.0)],
        ),
        family(
            "in_flight",
            MetricType::Gauge,
            None,
            vec![point(&[("model", "qwen")], 1.0)],
        ),
        family(
            "requests_total",
            MetricType::Counter,
            None,
            vec![point(&[("model", "qwen")], 1.0)],
        ),
    ]));
    assert_eq!(
        metrics["queue_depth"]["description"],
        serde_json::Value::String("Jobs waiting.".to_owned())
    );
    for name in ["in_flight", "requests_total"] {
        assert!(
            metrics[name].get("description").is_none(),
            "{}: {}",
            name,
            metrics[name]
        );
    }
}

/// The counter the dog keeps for its own refusals is a count, not a
/// measurement, so it goes out as `asInt` on a monotonic cumulative sum:
/// the same spelling `shep_sheep_restart_total` uses for the same reason.
#[test]
fn the_refused_counter_is_exported_when_nonzero() {
    let metrics = metrics_by_name(&with_samples(vec![family(
        REFUSED_TOTAL,
        MetricType::Counter,
        Some("Channel samples this dog refused, by reason."),
        vec![
            point(&[("reason", "invalid_name")], 3.0),
            point(&[("reason", "type_conflict")], 1.0),
        ],
    )]));
    let metric = &metrics[REFUSED_TOTAL];
    let sum = metric["sum"]
        .as_object()
        .expect("a refused count is a monotonic sum");
    assert_eq!(sum["isMonotonic"], serde_json::Value::Bool(true));
    assert_eq!(
        sum["aggregationTemporality"],
        serde_json::Value::from(2u8),
        "{sum:?}"
    );

    let points = sum["dataPoints"].as_array().expect("dataPoints");
    assert_eq!(points.len(), 2, "{metric}");
    assert_eq!(attributes_of(&points[0]), vec![("reason", "invalid_name")]);
    assert_eq!(attributes_of(&points[1]), vec![("reason", "type_conflict")]);
    assert_eq!(
        points[0]["asInt"],
        serde_json::Value::String("3".to_owned())
    );
    assert_eq!(
        points[1]["asInt"],
        serde_json::Value::String("1".to_owned())
    );
    for point in points {
        assert!(point.get("asDouble").is_none(), "{point}");
        assert_eq!(
            point["startTimeUnixNano"],
            serde_json::Value::String(START.to_string()),
            "{point}"
        );
    }
}

/// Nothing has been refused, so the store put no such family on the
/// reading, and the payload names nothing either: the exposition writes no
/// `# TYPE shep_channel_samples_refused_total` block either.
#[test]
fn the_refused_counter_is_absent_until_the_first_refusal() {
    let metrics = metrics_by_name(&with_samples(vec![family(
        "queue_depth",
        MetricType::Gauge,
        None,
        vec![point(&[("model", "qwen")], 1.0)],
    )]));
    assert!(
        metrics.get(REFUSED_TOTAL).is_none(),
        "{:?}",
        metrics.keys().collect::<Vec<_>>()
    );
}
