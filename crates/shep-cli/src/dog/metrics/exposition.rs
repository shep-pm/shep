//! Renders a [`super::Reading`] as Prometheus text exposition.
//!
//! [`render`] is what `dog::metrics::handle_connection` calls to answer
//! `/metrics`.

use core::fmt::{self, Write as _};

use shep_core::protocol::MetricType;
use shep_core::status::ProcStatus;

use super::Reading;
use super::samples::SampleFamily;

/// Every [`ProcStatus`] value, in the order `shep_sheep_status` renders
/// them, so a scrape's series order is stable across calls, and so the OTLP
/// exporter writes the same six.
pub(super) const ALL_STATUSES: [ProcStatus; 6] = [
    ProcStatus::Starting,
    ProcStatus::Online,
    ProcStatus::Stopping,
    ProcStatus::Stopped,
    ProcStatus::Errored,
    ProcStatus::WaitingRestart,
];

/// One `# HELP`/`# TYPE` block and the series beneath it.
struct MetricGroup {
    name: &'static str,
    help: &'static str,
    kind: &'static str,
    series: Vec<String>,
}

impl MetricGroup {
    fn new(name: &'static str, help: &'static str, kind: &'static str) -> Self {
        Self {
            name,
            help,
            kind,
            series: Vec::new(),
        }
    }

    /// Appends one series line. `label_str` is the label list, braces
    /// included, or empty for a label-less metric: anything that `Display`s,
    /// so a caller with a shared prefix can hand over that prefix plus the
    /// one pair that differs rather than a rebuilt `String`.
    fn push(&mut self, label_str: impl fmt::Display, value: impl fmt::Display) {
        let mut line = String::new();
        let _ = writeln!(line, "{}{label_str} {value}", self.name);
        self.series.push(line);
    }

    /// Renders this group's `# HELP`/`# TYPE` pair and series, or nothing
    /// at all when it has no series.
    fn render_into(&self, out: &mut String) {
        if self.series.is_empty() {
            return;
        }
        let _ = writeln!(out, "# HELP {} {}", self.name, self.help);
        let _ = writeln!(out, "# TYPE {} {}", self.name, self.kind);
        for line in &self.series {
            out.push_str(line);
        }
    }
}

/// Escapes a label value per the Prometheus text exposition format:
/// backslash, double quote and newline are the only three that need it.
/// One pass over the characters, since sequential replaces would
/// double-escape the backslashes an earlier one introduced.
fn escape_label_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str(r"\\"),
            '"' => escaped.push_str(r#"\""#),
            '\n' => escaped.push_str(r"\n"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Escapes and formats `pairs` as an *open* label list: `{k1="v1",k2="v2"`
/// with no closing brace. A caller that appends one more pair and closes it
/// once then pays the shared prefix's escapes once instead of once per
/// appended pair, which is what [`render`]'s status loop does.
fn label_prefix(pairs: &[(&str, &str)]) -> String {
    let mut out = String::from("{");
    for (i, (key, value)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "{key}=\"{}\"", escape_label_value(value));
    }
    out
}

/// Formats a label list as `{k1="v1",k2="v2"}`, or an empty string for no
/// labels, so [`MetricGroup::push`] formats both cases the same way.
fn labels(pairs: &[(&str, &str)]) -> String {
    if pairs.is_empty() {
        return String::new();
    }
    format!("{}}}", label_prefix(pairs))
}

/// [`render`]'s status series: an open prefix already escaped once per
/// sheep, closed by the one `status` pair that differs between the six
/// candidates.
///
/// `Display` rather than a `String` so the six series reuse the one escaped
/// prefix instead of each cloning it, re-escaping it, or reformatting it.
struct StatusLabels<'a> {
    prefix: &'a str,
    status: &'static str,
}

impl fmt::Display for StatusLabels<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{},status=\"{}\"}}", self.prefix, self.status)
    }
}

/// `ProcStatus`'s label value, the same spelling [`ProcStatus`]'s own
/// `Display` and wire form use. A status this client predates decodes as
/// `Unrecognized` and reads `unrecognized`; [`ALL_STATUSES`] has no entry.
///
/// A `&'static str` so the status loop pays no `to_string` for a spelling
/// fixed at compile time.
pub(super) fn status_label(status: ProcStatus) -> &'static str {
    match status {
        ProcStatus::Starting => "starting",
        ProcStatus::Online => "online",
        ProcStatus::Stopping => "stopping",
        ProcStatus::Stopped => "stopped",
        ProcStatus::Errored => "errored",
        ProcStatus::WaitingRestart => "waiting-restart",
        // `Unrecognized`, and any status added after this build.
        _ => "unrecognized",
    }
}

/// The `# TYPE` spelling for a channel sample's kind, the same two words the
/// wire carries.
fn kind_label(kind: &MetricType) -> &'static str {
    match kind {
        MetricType::Gauge => "gauge",
        MetricType::Counter => "counter",
    }
}

/// Escapes help text for a `# HELP` line. Backslash and newline are the only
/// two that need it, and the newline is the one that would split the line.
fn escape_help(help: &str) -> String {
    let mut escaped = String::with_capacity(help.len());
    for ch in help.chars() {
        match ch {
            '\\' => escaped.push_str(r"\\"),
            '\n' => escaped.push_str(r"\n"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// A sample value in the text form's own spelling. The wire can carry a value
/// the format has no decimal spelling for, and `inf` is not one of the three
/// names a scraper accepts.
fn render_value(value: f64) -> String {
    if value.is_finite() {
        value.to_string()
    } else if value.is_nan() {
        "NaN".to_string()
    } else if value > 0.0 {
        "+Inf".to_string()
    } else {
        "-Inf".to_string()
    }
}

/// Renders one channel sample family: its `# HELP`/`# TYPE` pair, or the
/// `# TYPE` half alone when nothing carried help, then one series per point.
///
/// The name needs no escaping: [`super::grammar`] refused every name that is
/// not `[a-zA-Z_:][a-zA-Z0-9_:]*` before the store kept it.
fn render_family(family: &SampleFamily, out: &mut String) {
    if family.points.is_empty() {
        return;
    }
    if let Some(help) = &family.help {
        let _ = writeln!(out, "# HELP {} {}", family.name, escape_help(help));
    }
    let _ = writeln!(out, "# TYPE {} {}", family.name, kind_label(&family.kind));
    for point in &family.points {
        let pairs: Vec<(&str, &str)> = point
            .labels
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let _ = writeln!(
            out,
            "{}{} {}",
            family.name,
            labels(&pairs),
            render_value(point.value)
        );
    }
}

/// Renders `reading` as Prometheus text exposition, format version 0.0.4.
///
/// One `# HELP`/`# TYPE` pair per metric name, every series of a name
/// grouped beneath it, and a trailing newline.
///
/// Label values are escaped (`\\`, `"`, `\n`): a sheep's name is
/// operator-supplied and reaches this function verbatim.
#[must_use]
pub fn render(reading: &Reading) -> String {
    let mut cpu = MetricGroup::new(
        "shep_sheep_cpu_percent",
        "Tree CPU as a percentage of one core, over the last sampling window.",
        "gauge",
    );
    let mut memory = MetricGroup::new(
        "shep_sheep_memory_bytes",
        "Tree resident set size in bytes.",
        "gauge",
    );
    let mut restarts = MetricGroup::new(
        "shep_sheep_restart_total",
        "Restart count since registration.",
        "counter",
    );
    let mut uptime = MetricGroup::new(
        "shep_sheep_uptime_seconds",
        "Seconds since this sheep's last successful start.",
        "gauge",
    );
    let mut status = MetricGroup::new(
        "shep_sheep_status",
        "1 for the status this sheep is currently in, 0 for every other status.",
        "gauge",
    );
    let mut dog_up = MetricGroup::new(
        "shep_dog_up",
        "1 when this dog is online, 0 otherwise.",
        "gauge",
    );
    let mut daemon_up = MetricGroup::new(
        "shep_daemon_up",
        "Always 1: the scrape reached the shepherd.",
        "gauge",
    );
    let mut daemon_pid = MetricGroup::new(
        "shep_daemon_pid",
        "The shepherd's own pid, so a restart is visible as a step change.",
        "gauge",
    );
    let mut host_memory_total = MetricGroup::new(
        "shep_host_memory_total_bytes",
        "Total physical memory on the host.",
        "gauge",
    );
    let mut host_memory_used = MetricGroup::new(
        "shep_host_memory_used_bytes",
        "Memory in use on the host, as the platform reports it.",
        "gauge",
    );
    let mut host_processes = MetricGroup::new(
        "shep_host_processes",
        "Number of processes running on the host, the flock included.",
        "gauge",
    );
    let mut host_uptime = MetricGroup::new(
        "shep_host_uptime_seconds",
        "Seconds since the host booted.",
        "gauge",
    );

    for info in &reading.flock {
        if let Some(source) = &info.dog {
            // Parked on purpose: a series at 0 would read as a dog that
            // went down, and fire the alert written for that.
            if info.disabled == Some(true) {
                continue;
            }
            let pairs = [
                ("dog", info.name.as_str()),
                ("source", <&'static str>::from(source)),
            ];
            let up = i32::from(info.status == ProcStatus::Online);
            dog_up.push(labels(&pairs), up);
            continue;
        }

        let fold = info.fold.as_deref().unwrap_or("");
        let id_string = info.id.to_string();
        let sheep_pairs = [
            ("sheep", info.name.as_str()),
            ("id", id_string.as_str()),
            ("fold", fold),
        ];
        // The open form first, because the closed one is it plus a brace.
        // Only `status` differs between the six candidates below, so
        // building them separately would escape this sheep's name, id and
        // fold twice here and six times more per status, every scrape.
        let sheep_prefix = label_prefix(&sheep_pairs);
        let sheep_labels = format!("{sheep_prefix}}}");

        if let Some(cpu_percent) = info.cpu_percent {
            cpu.push(&sheep_labels, cpu_percent);
        }
        if let Some(memory_bytes) = info.memory_bytes {
            memory.push(&sheep_labels, memory_bytes);
        }
        restarts.push(&sheep_labels, info.restarts);
        uptime.push(&sheep_labels, info.uptime_ms / 1000);

        for candidate in ALL_STATUSES {
            let value = i32::from(info.status == candidate);
            status.push(
                StatusLabels {
                    prefix: &sheep_prefix,
                    status: status_label(candidate),
                },
                value,
            );
        }
    }

    daemon_up.push(labels(&[("version", reading.daemon_version.as_str())]), 1);
    daemon_pid.push("", reading.daemon_pid);

    if let Some(host) = &reading.host {
        host_memory_total.push("", host.memory_total_bytes);
        host_memory_used.push("", host.memory_used_bytes);
        host_processes.push("", host.processes);
        host_uptime.push("", host.uptime_seconds);
    }

    let mut out = String::new();
    for group in [
        &cpu,
        &memory,
        &restarts,
        &uptime,
        &status,
        &dog_up,
        &daemon_up,
        &daemon_pid,
        &host_memory_total,
        &host_memory_used,
        &host_processes,
        &host_uptime,
    ] {
        group.render_into(&mut out);
    }
    for family in &reading.samples {
        render_family(family, &mut out);
    }
    out
}

#[cfg(test)]
pub(super) mod tests {
    use shep_core::protocol::{DogSource, MetricType, ProcessInfo};
    use shep_core::status::ProcStatus;

    use super::super::Reading;
    use super::super::samples::{SampleFamily, SamplePoint};
    use super::render;
    use crate::host_sample::HostReading;

    /// The exposition of [`golden_reading`], byte for byte, as the code
    /// before the shared-prefix change rendered it. That change moves the
    /// sheep/id/fold escapes out of the status loop so they are paid once
    /// per sheep instead of once per candidate status: an allocation change
    /// and nothing else, so the wire bytes must not move. Regenerate it
    /// after an intentional format change with:
    ///
    /// ```text
    /// BLESS=1 cargo test -p shep --lib --all-features bless_exposition_golden -- --ignored
    /// ```
    const GOLDEN_EXPOSITION: &str = include_str!("testdata/exposition_golden.prom");

    /// A sheep fixture shared by every test below: id `3`, fold `backend`,
    /// online, with a CPU and memory sample. Both are fixed, since the
    /// assertions spell out `id="3"` and `fold="backend"`.
    fn sample_info(name: &str) -> ProcessInfo {
        ProcessInfo::builder(3, name, ProcStatus::Online)
            .pid(Some(4242))
            .restarts(2)
            .uptime_ms(65_000)
            .fold(Some("backend".to_string()))
            .cpu_percent(Some(1.5))
            .memory_bytes(Some(2048))
            .build()
    }

    /// A baseline [`Reading`] with an empty flock and a host sample. Tests
    /// override what they care about with `..reading()`.
    fn reading() -> Reading {
        Reading {
            flock: vec![],
            daemon_version: "9.9.9".to_string(),
            daemon_pid: 12345,
            samples: vec![],
            host: Some(HostReading {
                memory_total_bytes: 16_000_000_000,
                memory_used_bytes: 8_000_000_000,
                processes: 200,
                uptime_seconds: 3600,
            }),
        }
    }

    #[test]
    fn a_sheep_with_no_reading_contributes_no_series() {
        let mut info = sample_info("web");
        info.cpu_percent = None;
        info.memory_bytes = None;
        let text = render(&Reading {
            flock: vec![info],
            ..reading()
        });
        assert!(!text.contains("shep_sheep_cpu_percent{"));
        assert!(!text.contains("shep_sheep_memory_bytes{"));
        // The counters do not depend on a sample and must still be there.
        assert!(text.contains("shep_sheep_restart_total{"));
    }

    #[test]
    fn status_is_a_label_with_one_series_per_state() {
        let mut info = sample_info("web");
        info.status = ProcStatus::Errored;
        let text = render(&Reading {
            flock: vec![info],
            ..reading()
        });
        assert!(text.contains(
            r#"shep_sheep_status{sheep="web",id="3",fold="backend",status="errored"} 1"#
        ));
        assert!(text.contains(r#"status="online"} 0"#));
    }

    #[test]
    fn an_unrecognized_status_makes_no_series_and_does_not_panic() {
        let mut info = sample_info("web");
        info.status = ProcStatus::Unrecognized;
        let text = render(&Reading {
            flock: vec![info],
            ..reading()
        });
        assert!(!text.contains(r#"status="unrecognized""#));
        assert!(text.contains(r#"status="online"} 0"#));
        assert_eq!(
            super::status_label(ProcStatus::Unrecognized),
            "unrecognized"
        );
    }

    #[test]
    fn a_label_value_is_escaped_so_one_odd_name_cannot_corrupt_the_response() {
        let text = render(&Reading {
            flock: vec![sample_info(r#"we"b\x"#)],
            ..reading()
        });
        // Every line must parse as `name{labels} value`. Only quotes not
        // preceded by a backslash count: a plain quote count is invariant
        // to escaping, so it balances on a broken implementation too.
        for line in text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            let mut real_quotes = 0;
            let mut chars = line.chars();
            while let Some(ch) = chars.next() {
                if ch == '\\' {
                    chars.next(); // escaped char, not a delimiter
                } else if ch == '"' {
                    real_quotes += 1;
                }
            }
            assert_eq!(real_quotes % 2, 0, "unbalanced quotes: {line}");
        }
        assert!(text.contains(r#"sheep="we\"b\\x""#));
    }

    #[test]
    fn a_dog_that_is_down_reports_zero_rather_than_nothing() {
        let mut dead = sample_info("bark");
        dead.status = ProcStatus::Errored;
        dead.dog = Some(DogSource::BuiltIn { grants: None });
        let text = render(&Reading {
            flock: vec![dead],
            ..reading()
        });
        assert!(text.contains(r#"shep_dog_up{dog="bark",source="built-in"} 0"#));
        assert!(
            !text.contains(r#"shep_sheep_status{sheep="bark""#),
            "a dog is not reported as a sheep"
        );
    }

    /// fails if disabling a dog reads as it going down, which fires the
    /// "dog down" alert an operator wrote for a crash.
    #[test]
    fn a_disabled_dog_exports_no_series_at_all() {
        let mut parked = sample_info("bark");
        parked.status = ProcStatus::Stopped;
        parked.dog = Some(DogSource::BuiltIn { grants: None });
        parked.disabled = Some(true);
        let mut running = sample_info("metrics");
        running.dog = Some(DogSource::BuiltIn { grants: None });
        let text = render(&Reading {
            flock: vec![parked, running],
            ..reading()
        });
        assert!(!text.contains(r#"dog="bark""#), "{text}");
        assert!(
            text.contains(r#"shep_dog_up{dog="metrics",source="built-in"} 1"#),
            "{text}"
        );
    }

    /// `Adopted` is the only source an operator can produce, and the label
    /// is what a dashboard filters on. Renaming it silently repoints every
    /// query built against it.
    #[test]
    fn an_adopted_dog_renders_the_adopted_source_label() {
        let mut dog = sample_info("shep-discord");
        dog.dog = Some(DogSource::Adopted {
            grants: None,
            path: "/opt/bin/shep-discord".to_string(),
            channel: false,
        });
        let text = render(&Reading {
            flock: vec![dog],
            ..reading()
        });
        assert!(
            text.contains(r#"shep_dog_up{dog="shep-discord",source="adopted"} 1"#),
            "{text}"
        );
    }

    /// A sheep in no fold renders an empty label value, not the word
    /// `None` that an `Option`'s own formatting would have produced.
    #[test]
    fn a_sheep_in_no_fold_renders_an_empty_fold_label() {
        let mut info = sample_info("web");
        info.fold = None;
        let text = render(&Reading {
            flock: vec![info],
            ..reading()
        });
        assert!(text.contains(r#"fold="""#), "{text}");
        assert!(!text.contains(r#"fold="None""#), "{text}");
    }

    #[test]
    fn every_metric_name_carries_one_help_one_type_and_contiguous_series() {
        let mut dog = sample_info("bark");
        dog.dog = Some(DogSource::BuiltIn { grants: None });
        let mut idle = sample_info("worker");
        idle.cpu_percent = None;
        idle.memory_bytes = None;
        let text = render(&Reading {
            flock: vec![sample_info("web"), idle, dog],
            ..reading()
        });
        let lines: Vec<&str> = text.lines().collect();

        let names = [
            "shep_sheep_cpu_percent",
            "shep_sheep_memory_bytes",
            "shep_sheep_restart_total",
            "shep_sheep_uptime_seconds",
            "shep_sheep_status",
            "shep_dog_up",
            "shep_daemon_up",
            "shep_daemon_pid",
            "shep_host_memory_total_bytes",
            "shep_host_memory_used_bytes",
            "shep_host_processes",
            "shep_host_uptime_seconds",
        ];

        let is_series_for = |line: &str, name: &str| {
            line.strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '))
        };

        for name in names {
            let help_count = lines
                .iter()
                .filter(|l| l.starts_with(&format!("# HELP {name} ")))
                .count();
            let type_count = lines
                .iter()
                .filter(|l| l.starts_with(&format!("# TYPE {name} ")))
                .count();
            assert_eq!(help_count, 1, "{name} must carry exactly one HELP line");
            assert_eq!(type_count, 1, "{name} must carry exactly one TYPE line");

            let series_indices: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, l)| is_series_for(l, name))
                .map(|(i, _)| i)
                .collect();
            assert!(
                !series_indices.is_empty(),
                "{name} must have at least one series in this fixture"
            );
            let first = *series_indices.first().unwrap();
            let last = *series_indices.last().unwrap();
            assert_eq!(
                last - first + 1,
                series_indices.len(),
                "{name}'s series are not contiguous: {series_indices:?}"
            );
        }
        assert!(text.ends_with('\n'), "exposition must end with a newline");
    }

    #[test]
    fn escape_label_value_handles_backslash_quote_and_newline() {
        assert_eq!(super::escape_label_value("plain"), "plain");
        assert_eq!(super::escape_label_value(r#"a"b"#), r#"a\"b"#);
        assert_eq!(super::escape_label_value(r"a\b"), r"a\\b");
        assert_eq!(super::escape_label_value("a\nb"), r"a\nb");
        assert_eq!(super::escape_label_value(r#"we"b\x"#), r#"we\"b\\x"#);
    }

    /// One [`SamplePoint`] with `labels`, which is the store's own shape.
    fn point(labels: &[(&str, &str)], value: f64) -> SamplePoint {
        SamplePoint {
            labels: labels
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            value,
        }
    }

    /// One [`SampleFamily`] as [`super::super::samples::SampleStore`] would
    /// hand it over.
    fn family(
        name: &str,
        kind: MetricType,
        help: Option<&str>,
        points: Vec<SamplePoint>,
    ) -> SampleFamily {
        SampleFamily {
            name: name.to_string(),
            help: help.map(str::to_string),
            kind,
            points,
        }
    }

    /// A [`Reading`] with `samples` on it and nothing else changed.
    fn with_samples(samples: Vec<SampleFamily>) -> Reading {
        Reading {
            samples,
            ..reading()
        }
    }

    #[test]
    fn a_counter_sample_is_typed_counter_in_the_exposition() {
        let text = render(&with_samples(vec![family(
            "requests_total",
            MetricType::Counter,
            Some("Requests served."),
            vec![point(&[("sheep", "web"), ("id", "3")], 4.0)],
        )]));
        assert!(
            text.contains("# HELP requests_total Requests served."),
            "{text}"
        );
        assert!(text.contains("# TYPE requests_total counter"), "{text}");
        assert!(
            text.contains(r#"requests_total{sheep="web",id="3"} 4"#),
            "{text}"
        );
    }

    /// The same shape as
    /// [`every_metric_name_carries_one_help_one_type_and_contiguous_series`],
    /// for a name two senders both report.
    #[test]
    fn two_senders_of_one_name_share_one_help_and_type_block() {
        let text = render(&with_samples(vec![family(
            "requests",
            MetricType::Gauge,
            Some("Requests in flight."),
            vec![
                point(&[("sheep", "web"), ("id", "3"), ("fold", "")], 1.0),
                point(&[("dog", "bark"), ("source", "built-in")], 2.0),
            ],
        )]));
        let lines: Vec<&str> = text.lines().collect();
        let name = "requests";
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.starts_with(&format!("# HELP {name} ")))
                .count(),
            1,
            "{name} must carry exactly one HELP line: {text}"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.starts_with(&format!("# TYPE {name} ")))
                .count(),
            1,
            "{name} must carry exactly one TYPE line: {text}"
        );
        let series: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                l.strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '))
            })
            .map(|(i, _)| i)
            .collect();
        assert_eq!(series.len(), 2, "{text}");
        assert_eq!(
            series[1] - series[0] + 1,
            series.len(),
            "{name}'s series are not contiguous: {series:?}"
        );
    }

    #[test]
    fn a_label_value_and_help_text_are_escaped() {
        let text = render(&with_samples(vec![family(
            "queue_depth",
            MetricType::Gauge,
            Some("jobs \\\"quoted\".\nSecond line."),
            vec![point(&[("model", "we\"b\\x\n")], 1.0)],
        )]));
        assert!(
            text.contains(r#"queue_depth{model="we\"b\\x\n"} 1"#),
            "{text}"
        );
        // A HELP line is one line: the newline inside it is escaped, and so is
        // the backslash. A double quote needs no escaping in help text, only
        // in a label value.
        assert!(
            text.contains(r#"# HELP queue_depth jobs \\"quoted".\nSecond line."#),
            "{text}"
        );
        assert_eq!(
            text.lines()
                .filter(|l| l.starts_with("# HELP queue_depth"))
                .count(),
            1,
            "an unescaped newline in help text split the line: {text}"
        );
    }

    #[test]
    fn a_family_with_no_help_renders_no_help_line() {
        let text = render(&with_samples(vec![family(
            "queue_depth",
            MetricType::Gauge,
            None,
            vec![point(&[("model", "qwen")], 1.0)],
        )]));
        assert!(!text.contains("# HELP queue_depth"), "{text}");
        assert!(text.contains("# TYPE queue_depth gauge"), "{text}");
    }

    /// The samples come after every built-in group, so a scraper reading the
    /// reference series first never reads a sample in front of them.
    #[test]
    fn channel_samples_render_after_the_builtin_groups() {
        let text = render(&Reading {
            flock: vec![sample_info("web")],
            samples: vec![family(
                "rps",
                MetricType::Gauge,
                Some("Requests per second."),
                vec![point(&[("sheep", "web"), ("id", "3")], 2.0)],
            )],
            ..reading()
        });
        let lines: Vec<&str> = text.lines().collect();
        let last_builtin = lines
            .iter()
            .rposition(|l| l.starts_with("shep_host_uptime_seconds"))
            .expect("the host group is in every reading");
        let sample = lines
            .iter()
            .position(|l| l.starts_with("# TYPE rps "))
            .expect("the sample is rendered: {text}");
        assert!(sample > last_builtin, "{text}");
    }

    /// The wire can carry a value the text format has no decimal spelling for.
    /// `inf` is not one of the three names a scraper accepts.
    #[test]
    fn a_non_finite_sample_renders_the_formats_own_spelling() {
        let text = render(&with_samples(vec![family(
            "queue_depth",
            MetricType::Gauge,
            None,
            vec![
                point(&[("model", "nan")], f64::NAN),
                point(&[("model", "up")], f64::INFINITY),
                point(&[("model", "down")], f64::NEG_INFINITY),
            ],
        )]));
        assert!(text.contains(r#"queue_depth{model="nan"} NaN"#), "{text}");
        assert!(text.contains(r#"queue_depth{model="up"} +Inf"#), "{text}");
        assert!(text.contains(r#"queue_depth{model="down"} -Inf"#), "{text}");
        assert!(!text.contains(" inf"), "{text}");
    }

    /// The [`Reading`] [`GOLDEN_EXPOSITION`] was captured from: one plain
    /// sheep, one idle sheep with no CPU or memory sample, one dog, and
    /// one sheep whose name needs all three escapes while in a non-online
    /// state, so every label path and all six status spellings appear in
    /// it. `pub(super)`: the OTLP encoder's drift guard uses it too.
    pub(in crate::dog::metrics) fn golden_reading() -> Reading {
        let mut dog = sample_info("bark");
        dog.dog = Some(DogSource::BuiltIn { grants: None });
        let mut odd = sample_info("we\"b\\x\n");
        odd.status = ProcStatus::WaitingRestart;
        let mut idle = sample_info("worker");
        idle.cpu_percent = None;
        idle.memory_bytes = None;
        Reading {
            flock: vec![sample_info("web"), idle, dog, odd],
            ..reading()
        }
    }

    #[test]
    fn a_full_flock_renders_exactly_the_bytes_the_baseline_did() {
        assert_eq!(render(&golden_reading()), GOLDEN_EXPOSITION);
    }

    /// Rewrites the golden file from the current `render`. Deliberate only:
    /// `#[ignore]`d, and a no-op unless `BLESS` is set, so a stray
    /// `--ignored` run cannot silently move the pin.
    #[test]
    #[ignore = "writes testdata/exposition_golden.prom; run it only to bless an intentional format change"]
    fn bless_exposition_golden() {
        if std::env::var_os("BLESS").is_none() {
            return;
        }
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/dog/metrics/testdata/exposition_golden.prom"
        );
        std::fs::write(path, render(&golden_reading())).unwrap();
    }
}
