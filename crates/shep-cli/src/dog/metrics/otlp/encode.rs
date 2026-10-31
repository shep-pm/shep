//! The OTLP/JSON mapping for a [`Reading`]: the same series
//! [`crate::dog::metrics::exposition::render`] serves, in the shape a
//! collector reads, the channel samples included.
//!
//! The two renderers share no code on purpose: `render` is allocation-tuned,
//! and a neutral intermediate model would spend allocations on every scrape.
//! `the_payload_names_every_series_the_exposition_names` is the drift guard.
//!
//! [`payload`] holds the OTLP/JSON shape, [`samples`] the channel-sample
//! mapping.

use shep_core::status::ProcStatus;

use crate::dog::metrics::Reading;
use crate::dog::metrics::exposition::{ALL_STATUSES, status_label};

mod payload;
mod samples;

use payload::{
    Nanos, attr, cumulative_point, cumulative_sum, double_point, export_request, gauge, gauge_point,
};
use samples::sample_metrics;

/// `service.name`, the resource attribute a collector groups every series
/// under.
const SERVICE_NAME: &str = "shep";

/// `scope.name`: the instrumentation that produced these points, which is
/// this dog, not the shepherd that answered the listing.
const SCOPE_NAME: &str = "shep.metrics";

/// What the encoder stamps its points with, passed in rather than read from
/// the clock, so an exact-payload test can pin the timestamps.
#[derive(Debug, Clone, Copy)]
pub struct ExportContext<'a> {
    /// Nanoseconds since the Unix epoch, stamped as every point's
    /// `timeUnixNano`.
    pub now_unix_nano: u64,
    /// Nanoseconds since the Unix epoch, when this exporter started: the
    /// built-in cumulative sums' `startTimeUnixNano`, and the refusal
    /// counter's, so a collector can difference two exports and get the
    /// restarts inside the interval. A channel counter carries its sender's
    /// own start instead. See [`sample_metrics`].
    pub exporter_start_unix_nano: u64,
    /// `host.name`, when the OS reports one. `None` leaves the attribute
    /// out rather than sending an empty one.
    pub host_name: Option<&'a str>,
}

/// Encodes `reading` as one OTLP/JSON `ExportMetricsServiceRequest` body.
///
/// Timestamps and `host.name` come from `context`. A non-finite value is
/// dropped rather than encoded, since OTLP has no number for it.
///
/// # Panics
/// Never: every value here is a string, `u64`, finite `f64` or `bool`.
#[track_caller]
pub fn encode(reading: &Reading, context: &ExportContext<'_>) -> String {
    let now = Nanos::new(context.now_unix_nano);
    let start = Nanos::new(context.exporter_start_unix_nano);

    let mut cpu = Vec::new();
    let mut memory = Vec::new();
    let mut restarts = Vec::new();
    let mut uptime = Vec::new();
    let mut status = Vec::new();
    let mut dog_up = Vec::new();

    for info in &reading.flock {
        if let Some(source) = &info.dog {
            // The same rule `render` follows: a parked dog exports nothing,
            // so disabling one never reads as it going down.
            if info.disabled == Some(true) {
                continue;
            }
            let attributes = vec![
                attr("dog", info.name.as_str()),
                attr("source", <&'static str>::from(source)),
            ];
            dog_up.push(gauge_point(
                attributes,
                now,
                u64::from(info.status == ProcStatus::Online),
            ));
            continue;
        }

        let id = info.id.to_string();
        let fold = info.fold.as_deref().unwrap_or("");
        // A closure rather than one shared `Vec`, because every series
        // below needs its own copy to move into its own point.
        let sheep = || {
            vec![
                attr("sheep", info.name.as_str()),
                attr("id", id.clone()),
                attr("fold", fold),
            ]
        };

        // `None` covers a sheep that is not running and one the shepherd has
        // not sampled yet, and OTLP has no unknown: a point with no value
        // would read as zero, which `render` is careful never to say.
        if let Some(cpu_percent) = info.cpu_percent.filter(|percent| percent.is_finite()) {
            cpu.push(double_point(sheep(), now, f64::from(cpu_percent)));
        }
        if let Some(memory_bytes) = info.memory_bytes {
            memory.push(gauge_point(sheep(), now, memory_bytes));
        }
        restarts.push(cumulative_point(
            sheep(),
            start,
            now,
            u64::from(info.restarts),
        ));
        uptime.push(gauge_point(sheep(), now, info.uptime_ms / 1000));

        for candidate in ALL_STATUSES {
            let mut attributes = sheep();
            attributes.push(attr("status", status_label(candidate)));
            status.push(gauge_point(
                attributes,
                now,
                u64::from(info.status == candidate),
            ));
        }
    }

    let daemon_up = gauge_point(
        vec![attr("version", reading.daemon_version.as_str())],
        now,
        1,
    );
    let daemon_pid = gauge_point(Vec::new(), now, u64::from(reading.daemon_pid));

    let mut metrics = Vec::new();
    for group in [
        gauge(
            "shep_sheep_cpu_percent",
            Some("Tree CPU as a percentage of one core, over the last sampling window."),
            cpu,
        ),
        gauge(
            "shep_sheep_memory_bytes",
            Some("Tree resident set size in bytes."),
            memory,
        ),
        cumulative_sum(
            "shep_sheep_restart_total",
            Some("Restart count since registration or the last `shep reset`."),
            restarts,
        ),
        gauge(
            "shep_sheep_uptime_seconds",
            Some("Seconds since this sheep's last successful start."),
            uptime,
        ),
        gauge(
            "shep_sheep_status",
            Some("1 for the status this sheep is currently in, 0 for every other status."),
            status,
        ),
        gauge(
            "shep_dog_up",
            Some("1 when this dog is online, 0 otherwise."),
            dog_up,
        ),
        gauge(
            "shep_daemon_up",
            Some("Always 1: the scrape reached the shepherd."),
            vec![daemon_up],
        ),
        gauge(
            "shep_daemon_pid",
            Some("The shepherd's own pid, so a restart is visible as a step change."),
            vec![daemon_pid],
        ),
    ] {
        metrics.extend(group);
    }

    if let Some(host) = &reading.host {
        for group in [
            gauge(
                "shep_host_memory_total_bytes",
                Some("Total physical memory on the host."),
                vec![gauge_point(Vec::new(), now, host.memory_total_bytes)],
            ),
            gauge(
                "shep_host_memory_used_bytes",
                Some("Memory in use on the host, as the platform reports it."),
                vec![gauge_point(Vec::new(), now, host.memory_used_bytes)],
            ),
            gauge(
                "shep_host_processes",
                Some("Number of processes running on the host, the flock included."),
                vec![gauge_point(
                    Vec::new(),
                    now,
                    u64::try_from(host.processes).unwrap_or(u64::MAX),
                )],
            ),
            gauge(
                "shep_host_uptime_seconds",
                Some("Seconds since the host booted."),
                vec![gauge_point(Vec::new(), now, host.uptime_seconds)],
            ),
        ] {
            metrics.extend(group);
        }
    }

    metrics.extend(sample_metrics(&reading.samples, &reading.flock, start, now));

    let mut resource = vec![
        attr("service.name", SERVICE_NAME),
        attr("service.version", reading.daemon_version.as_str()),
    ];
    if let Some(host_name) = context.host_name {
        resource.push(attr("host.name", host_name));
    }

    let body = export_request(resource, SCOPE_NAME, &reading.daemon_version, metrics);
    serde_json::to_string(&body)
        .expect("every value here is a string, an integer, a finite double or a bool")
}

#[cfg(test)]
mod tests;
