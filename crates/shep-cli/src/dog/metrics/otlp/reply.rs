//! Reading a collector's answer: `ExportMetricsServiceResponse`, and why a
//! 2xx that names dropped points is a failed export.
//!
//! The request half is the `encode` module beside this one. This half is its
//! own module because it is the shape the collector chose rather than the
//! shape this dog writes, and the two are read in opposite directions.

use serde::Deserialize;

/// The most of a collector's reason this keeps. A collector spelling out
/// what it rejected would otherwise put a paragraph in the dog's log, where
/// the reason is one line.
const MAX_REASON_CHARS: usize = 200;

/// An OTLP `int64` in JSON. The encoding writes 64-bit integers as decimal
/// strings, so `"3"` is the spec's spelling; a collector that writes a bare
/// `3` is read too rather than turned into a failed export over a spelling.
#[derive(Debug, Clone, Copy)]
struct JsonInt(u64);

impl<'de> Deserialize<'de> for JsonInt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Reader;

        impl serde::de::Visitor<'_> for Reader {
            type Value = JsonInt;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a decimal int64, quoted or bare")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<JsonInt, E> {
                value.parse::<u64>().map(JsonInt).map_err(E::custom)
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<JsonInt, E> {
                Ok(JsonInt(value))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<JsonInt, E> {
                u64::try_from(value).map(JsonInt).map_err(E::custom)
            }
        }

        deserializer.deserialize_any(Reader)
    }
}

/// `ExportMetricsPartialSuccess`: what a collector names about the points it
/// did not take. Both fields are absent on a collector that took everything
/// and echoes the object anyway, which is a clean export.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartialSuccess {
    /// How many points the collector dropped.
    #[serde(default)]
    rejected_data_points: Option<JsonInt>,
    /// The collector's own words about why.
    #[serde(default)]
    error_message: Option<String>,
}

/// `ExportMetricsServiceResponse`: the answer a collector gives to the POST
/// [`super::encode::encode`] wrote. Only `partialSuccess` is read.
///
/// An unknown field is ignored rather than refused: a collector adding one
/// must not break an export that worked.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportReply {
    /// Why the collector took less than it was sent. Absent, `null`, or an
    /// object naming neither dropped points nor a reason, means it took
    /// everything.
    #[serde(default)]
    partial_success: Option<PartialSuccess>,
}

/// `data point` or `data points`, so a collector that dropped one is not
/// reported as several.
fn points(count: u64) -> &'static str {
    if count == 1 {
        "data point"
    } else {
        "data points"
    }
}

/// Why a collector's 2xx answer says it dropped points, `None` when it took
/// the whole payload.
///
/// An empty body, or a `partialSuccess` naming neither a count nor a reason,
/// means everything landed. A body this cannot read is reported rather than
/// passed. The reason carries the collector's own text, so it is sanitised
/// and cut at [`MAX_REASON_CHARS`].
pub fn rejection(body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    let Ok(reply) = serde_json::from_str::<ExportReply>(trimmed) else {
        return Some("the collector answered something that was not OTLP/JSON".to_owned());
    };
    // An absent or `null` `partialSuccess` names nothing the collector dropped.
    let partial = reply.partial_success?;

    let dropped = partial
        .rejected_data_points
        .map(|count| count.0)
        .unwrap_or(0);
    let (sanitised, _stripped) =
        crate::terminal_safe::sanitise(partial.error_message.unwrap_or_default().trim());
    let message = sanitised.trim();
    if dropped == 0 && message.is_empty() {
        return None;
    }

    let reason = match (dropped, message.is_empty()) {
        (0, false) => message.to_owned(),
        (count, true) => format!("{count} {} rejected", points(count)),
        (count, false) => format!("{count} {} rejected: {message}", points(count)),
    };
    Some(reason.chars().take(MAX_REASON_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty answer is a 204, or a 200 that said nothing, and an empty
    /// `partialSuccess` object is a collector that took everything echoing
    /// the field back. Fails if any of those started reading as a dropped
    /// export, which is also what stops `recovered` from ever being logged.
    #[test]
    fn an_answer_that_names_nothing_means_the_collector_took_everything() {
        for body in [
            "",
            "  \n ",
            "{}",
            r#"{"partialSuccess":null}"#,
            r#"{"partialSuccess":{}}"#,
            r#"{"partialSuccess":{"rejectedDataPoints":"0"}}"#,
            r#"{"partialSuccess":{"errorMessage":""}}"#,
        ] {
            assert_eq!(rejection(body), None, "{body}");
        }
    }

    /// The encoding's own shape: `partialSuccess` is an object carrying
    /// `rejectedDataPoints`, an int64 and therefore a decimal string, and
    /// `errorMessage`. A bare number is read too, so a collector that writes
    /// JSON loosely is not a failed export over a spelling.
    #[test]
    fn a_partial_success_names_what_the_collector_dropped() {
        for (body, want) in [
            (
                r#"{"partialSuccess":{"rejectedDataPoints":"1"}}"#,
                "1 data point rejected",
            ),
            (
                r#"{"partialSuccess":{"rejectedDataPoints":"3"}}"#,
                "3 data points rejected",
            ),
            (
                r#"{"partialSuccess":{"rejectedDataPoints":"1","errorMessage":"bad point"}}"#,
                "1 data point rejected: bad point",
            ),
            (
                r#"{"partialSuccess":{"errorMessage":"disk full"}}"#,
                "disk full",
            ),
            (
                r#"{"partialSuccess":{"rejectedDataPoints":2}}"#,
                "2 data points rejected",
            ),
            (
                // A field this module does not read must not break an answer
                // it does.
                r#"{"partialSuccess":{"rejectedDataPoints":"1","future":[1]},"futureToo":true}"#,
                "1 data point rejected",
            ),
        ] {
            assert_eq!(rejection(body).as_deref(), Some(want), "{body}");
        }
    }

    /// A 2xx this cannot read is not evidence that anything was accepted,
    /// a `partialSuccess` spelled as a string included: that is not the
    /// encoding's shape, and guessing at it is how a real rejection gets a
    /// line that says nothing about it.
    #[test]
    fn an_answer_that_is_not_otlp_json_is_reported_rather_than_ignored() {
        for body in [
            "Gateway Timeout",
            "null",
            r#"{"partialSuccess":"1 data point dropped"}"#,
        ] {
            assert_eq!(
                rejection(body).as_deref(),
                Some("the collector answered something that was not OTLP/JSON"),
                "{body}"
            );
        }
    }

    /// The reason carries a collector's own text: it reaches the dog's log
    /// sanitised, and cut at the budget rather than as a paragraph.
    #[test]
    fn a_reason_is_sanitised_and_cut_before_it_reaches_the_log() {
        let long = "x".repeat(10 * MAX_REASON_CHARS);
        let reason = rejection(&format!(
            "{{\"partialSuccess\":{{\"errorMessage\":\"{long}\"}}}}"
        ))
        .unwrap();
        assert_eq!(reason.chars().count(), MAX_REASON_CHARS);

        let control = rejection(
            r#"{"partialSuccess":{"rejectedDataPoints":"1","errorMessage":"a\u001b[31mb"}}"#,
        )
        .unwrap();
        assert_eq!(control, "1 data point rejected: a[31mb");
        assert!(!control.chars().any(char::is_control), "{control}");
    }
}
