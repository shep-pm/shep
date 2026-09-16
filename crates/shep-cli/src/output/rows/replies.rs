//! Rows for a daemon action reply: `shep trigger`, `shep signal`, `shep
//! send-line`, and the bark log's own listing.

use serde::Serialize;
use shep_core::barks::{Bark, SinkOutcome};
use shep_core::protocol::{
    ActionOutcome, ActionReply, LineOutcome, LineReply, SignalOutcome, SignalReply,
};

use crate::output::Render;
use crate::style::Presentation;
use crate::vocabulary::Role;

use super::toolkit::{Paint, paint, reply_paint};

/// `Response::Triggered(Vec<ActionReply>)`: one row per matched sheep, each
/// carrying what happened when the daemon tried to deliver `shep trigger`'s
/// action to it.
///
/// A newtype for the orphan rule; `transparent`, so `--format json` carries
/// each reply as the daemon sent it, `body` untruncated and with its newlines
/// intact. The table cannot; see [`Self::rows`].
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct TriggeredRows(pub Vec<ActionReply>);

/// A `Replied` body longer than this many `char`s is truncated in the table,
/// never in JSON. 80 leaves room for ID/NAME/OUTCOME on an ordinary terminal;
/// `render_table` cannot wrap.
pub(crate) const TRIGGER_BODY_PREVIEW_CHARS: usize = 80;

// Shared scaffolding for the three per-sheep reply tables. All three render
// `["ID", "NAME", "OUTCOME", "DETAIL"]` and share the JSON keys, the
// priorities and the paint dispatch; each verb has its own
// `describe_*_outcome` for the `(OUTCOME, DETAIL)` pair.
struct ReplyRows;

impl ReplyRows {
    fn headers() -> &'static [&'static str] {
        &["ID", "NAME", "OUTCOME", "DETAIL"]
    }

    fn row(id: u32, name: &str, outcome: &str, detail: String) -> Vec<String> {
        vec![
            id.to_string(),
            name.to_string(),
            outcome.to_string(),
            detail,
        ]
    }

    fn rows_for(
        rows: Vec<Vec<String>>,
        presentation: Presentation,
        status_word: bool,
    ) -> Vec<Vec<String>> {
        paint(
            rows,
            Self::headers(),
            presentation,
            status_word,
            |header, cell, _index| reply_paint(header, cell),
        )
    }

    // Parallel to `headers()`. DETAIL is the sole extra, and dropping it is
    // the only narrowing these tables do.
    const PRIORITIES: &'static [u8] = &[0, 0, 0, 6];
}

// One JSON key rule for the three per-sheep reply tables; the panic names the
// concrete type. A macro, not a shared fn: rustc's dead-code pass cannot see
// a use that occurs only inside another trait impl's body.
macro_rules! reply_rows_json_key {
    ($caller:expr, $header:expr) => {{
        let caller: &'static str = $caller;
        let header: &str = $header;
        match header {
            "ID" => "id",
            "NAME" => "name",
            // Both columns read the one `outcome` object, so both sit in
            // `assert_no_drift`'s `formatted` list.
            "OUTCOME" | "DETAIL" => "outcome",
            other => panic!("{caller}::headers() does not include {other:?}"),
        }
    }};
}

impl Render for TriggeredRows {
    fn headers() -> &'static [&'static str] {
        ReplyRows::headers()
    }

    /// One row per matched sheep. `OUTCOME` is [`ActionOutcome`]'s `kind`
    /// tag; `DETAIL` is where the four variants differ, via
    /// [`describe_outcome`].
    ///
    /// A `Replied` body is capped by [`preview_body`]: `render_table` writes
    /// exactly one line per row, so a multi-line body would desync the table.
    /// `--format json` is untouched.
    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|reply| {
                let (outcome, detail) = describe_outcome(&reply.outcome);
                ReplyRows::row(reply.id, &reply.name, outcome, detail)
            })
            .collect()
    }

    /// Shared with the other two per-sheep reply tables; see
    /// [`ReplyRows::rows_for`].
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        ReplyRows::rows_for(self.rows(), presentation, status_word)
    }

    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        reply_rows_json_key!("TriggeredRows", header)
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    const PRIORITIES: &'static [u8] = ReplyRows::PRIORITIES;
}

/// [`TriggeredRows::rows`]'s per-outcome split: the `OUTCOME` label and the
/// `DETAIL` text.
///
/// `ActionOutcome` is `#[non_exhaustive]`; the wildcard arm renders a variant
/// this client predates as `unknown` with its `Debug` form.
pub(crate) fn describe_outcome(outcome: &ActionOutcome) -> (&'static str, String) {
    match outcome {
        ActionOutcome::Replied { body } => ("replied", preview_body(body)),
        // Names the config field: an operator learns it here or in `--help`.
        ActionOutcome::NoChannel => (
            "no_channel",
            "no shepherd channel — set channel = true, or wait_ready / \
             shutdown_with_message, which imply it"
                .to_string(),
        ),
        ActionOutcome::Skipped => (
            "skipped",
            "mid-reload — a fresh instance is replacing this one".to_string(),
        ),
        ActionOutcome::TimedOut => (
            "timed_out",
            "no reply within the app's own action_timeout".to_string(),
        ),
        other => ("unknown", format!("{other:?}")),
    }
}

/// Collapses a `Replied` body to one line, capped at
/// [`TRIGGER_BODY_PREVIEW_CHARS`] `char`s. Embedded `\n`/`\r` become the
/// two-character escapes, and a body the cap cuts off ends in `...`.
pub(crate) fn preview_body(body: &str) -> String {
    let mut preview = String::new();
    let mut truncated = false;
    for (seen, ch) in body.chars().enumerate() {
        if seen == TRIGGER_BODY_PREVIEW_CHARS {
            truncated = true;
            break;
        }
        match ch {
            '\n' => preview.push_str("\\n"),
            '\r' => preview.push_str("\\r"),
            other => preview.push(other),
        }
    }
    if truncated {
        preview.push_str("...");
    }
    preview
}

/// `Response::Signalled(Vec<SignalReply>)`: one row per matched sheep, each
/// carrying what happened when the shepherd tried to deliver `shep signal`'s
/// signal to it.
///
/// Shaped like [`TriggeredRows`]: the selector grammar makes a mixed flock
/// the normal case, so the outcome is per row.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct SignalledRows(pub Vec<SignalReply>);

impl Render for SignalledRows {
    fn headers() -> &'static [&'static str] {
        ReplyRows::headers()
    }

    /// One row per matched sheep. `OUTCOME` is [`SignalOutcome`]'s `kind`
    /// tag; `DETAIL` comes from [`describe_signal_outcome`].
    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|reply| {
                let (outcome, detail) = describe_signal_outcome(&reply.outcome);
                ReplyRows::row(reply.id, &reply.name, outcome, detail)
            })
            .collect()
    }

    /// Shared with the other two per-sheep reply tables; see
    /// [`ReplyRows::rows_for`].
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        ReplyRows::rows_for(self.rows(), presentation, status_word)
    }

    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        reply_rows_json_key!("SignalledRows", header)
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    const PRIORITIES: &'static [u8] = ReplyRows::PRIORITIES;
}

/// [`SignalledRows::rows`]'s per-outcome split. `SignalOutcome` is
/// `#[non_exhaustive]`; the wildcard arm renders a variant this client
/// predates as `unknown` with its `Debug` form.
fn describe_signal_outcome(outcome: &SignalOutcome) -> (&'static str, String) {
    match outcome {
        SignalOutcome::Delivered => ("delivered", String::new()),
        SignalOutcome::NotRunning => ("not_running", "no live process to signal".to_string()),
        SignalOutcome::Failed { reason } => ("failed", reason.clone()),
        other => ("unknown", format!("{other:?}")),
    }
}

/// `Response::SentLine(Vec<LineReply>)`: one row per matched sheep, each
/// carrying what happened when the shepherd tried to write `shep whisper`'s
/// line to its stdin.
///
/// Shaped like [`TriggeredRows`]/[`SignalledRows`], for the same reason.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct SentLineRows(pub Vec<LineReply>);

impl Render for SentLineRows {
    fn headers() -> &'static [&'static str] {
        ReplyRows::headers()
    }

    /// One row per matched sheep. `OUTCOME` is [`LineOutcome`]'s `kind` tag;
    /// `DETAIL` comes from [`describe_line_outcome`].
    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|reply| {
                let (outcome, detail) = describe_line_outcome(&reply.outcome);
                ReplyRows::row(reply.id, &reply.name, outcome, detail)
            })
            .collect()
    }

    /// Shared with the other two per-sheep reply tables; see
    /// [`ReplyRows::rows_for`].
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        ReplyRows::rows_for(self.rows(), presentation, status_word)
    }

    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        reply_rows_json_key!("SentLineRows", header)
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    const PRIORITIES: &'static [u8] = ReplyRows::PRIORITIES;
}

/// [`SentLineRows::rows`]'s per-outcome split. `LineOutcome` is
/// `#[non_exhaustive]`; the wildcard arm renders a variant this client
/// predates as `unknown` with its `Debug` form.
fn describe_line_outcome(outcome: &LineOutcome) -> (&'static str, String) {
    match outcome {
        LineOutcome::Sent => ("sent", String::new()),
        // Names the config field, as `describe_outcome`'s `NoChannel` does.
        LineOutcome::NoStdin => ("no_stdin", "no stdin pipe — set stdin = true".to_string()),
        LineOutcome::NotWritten { reason } => ("not_written", reason.clone()),
        other => ("unknown", format!("{other:?}")),
    }
}

/// `Vec<Bark>`, `shep barks`' payload, newest last as it sits on disk and as
/// `--tail` counts from.
///
/// Never built from a `Response`: `barks` reads `barks.jsonl` directly, so
/// the history survives the shepherd.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct BarkRows(pub Vec<Bark>);

impl Render for BarkRows {
    fn headers() -> &'static [&'static str] {
        &["WHEN", "RULE", "SUBJECT", "MESSAGE", "SINKS"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|b| {
                vec![
                    crate::output::local_timestamp(b.at_ms),
                    b.rule.clone(),
                    b.subject.clone(),
                    b.message.clone(),
                    sinks_cell(&b.sinks),
                ]
            })
            .collect()
    }

    /// SINKS alone: `Meadow` when every sink took the bark, `Bark` when the
    /// cell carries [`sinks_cell`]'s `(failed)`, the dash rule when there
    /// were no sinks.
    ///
    /// WHEN stays plain although ID elsewhere is chrome: a timestamp is what
    /// an operator scans an alert feed by.
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        let rows = self.rows();
        paint(
            rows,
            Self::headers(),
            presentation,
            status_word,
            |header, cell, _index| match (header, cell) {
                ("SINKS", "-") => Paint::Default,
                ("SINKS", sinks) if sinks.contains("(failed)") => Paint::Role(Role::Bark),
                ("SINKS", _) => Paint::Role(Role::Meadow),
                _ => Paint::Default,
            },
        )
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "WHEN" => "at_ms",
            "RULE" => "rule",
            "SUBJECT" => "subject",
            "MESSAGE" => "message",
            "SINKS" => "sinks",
            other => panic!("BarkRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    // Parallel to `headers()`. MESSAGE, unbounded free text, drops before
    // SINKS; both can be lost.
    const PRIORITIES: &'static [u8] = &[0, 0, 0, 7, 6];
}

/// Renders one [`Bark::sinks`] list for the `SINKS` column: `ops` for a
/// delivered sink, `ops(failed)` for a refused one. Never the sink's own
/// error text, which can quote a webhook's HTTP response; `--format json`
/// carries that in full.
///
/// `-` for an empty list, which per [`Bark::sinks`] means the shepherd wrote
/// the record itself.
pub(crate) fn sinks_cell(sinks: &[SinkOutcome]) -> String {
    if sinks.is_empty() {
        return "-".to_string();
    }
    sinks
        .iter()
        .map(|outcome| {
            if outcome.error.is_some() {
                format!("{}(failed)", outcome.sink)
            } else {
                outcome.sink.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::super::tests::{assert_no_drift, coloured, painted};
    use super::*;

    fn sample_replies() -> TriggeredRows {
        TriggeredRows(vec![
            ActionReply {
                id: 1,
                name: "web".to_string(),
                outcome: ActionOutcome::Replied {
                    body: "pong".to_string(),
                },
            },
            ActionReply {
                id: 2,
                name: "worker".to_string(),
                outcome: ActionOutcome::NoChannel,
            },
        ])
    }

    /// OUTCOME and DETAIL both derive from `outcome`, a nested object, so
    /// both sit in `assert_no_drift`'s `formatted` list. Its key and
    /// cell-count checks still run.
    #[test]
    fn triggered_rows_do_not_drift() {
        assert_no_drift(&sample_replies(), |j| &j[0], &["OUTCOME", "DETAIL"]);
    }

    #[test]
    fn triggered_rows_render_id_name_and_outcome_kind() {
        let rows = sample_replies().rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "1");
        assert_eq!(rows[0][1], "web");
        assert_eq!(rows[0][2], "replied");
        assert_eq!(rows[1][0], "2");
        assert_eq!(rows[1][1], "worker");
        assert_eq!(rows[1][2], "no_channel");
    }

    /// An operator reading a `no_channel` row must find the config field
    /// that would have avoided it in the row itself, not only in `--help`.
    #[test]
    fn a_no_channel_detail_names_the_config_field() {
        let rows = sample_replies().rows();
        let detail = &rows[1][3];
        assert!(
            detail.contains("channel = true"),
            "a no_channel row must name the field that opens one: {detail}"
        );
        assert!(
            detail.contains("wait_ready") && detail.contains("shutdown_with_message"),
            "and the two fields that imply it: {detail}"
        );
    }

    #[test]
    fn skipped_and_timed_out_details_say_why() {
        let skipped = describe_outcome(&ActionOutcome::Skipped).1;
        assert!(skipped.to_lowercase().contains("reload"), "{skipped}");

        let timed_out = describe_outcome(&ActionOutcome::TimedOut).1;
        assert!(
            timed_out.to_lowercase().contains("action_timeout"),
            "{timed_out}"
        );
    }

    #[test]
    fn a_short_single_line_body_previews_unchanged() {
        assert_eq!(preview_body("pong"), "pong");
    }

    /// [`preview_body`]'s `seen == TRIGGER_BODY_PREVIEW_CHARS` check fires
    /// one character late, so only a body past the cap is truncated.
    #[test]
    fn a_body_exactly_at_the_cap_is_not_truncated() {
        let exact = "x".repeat(TRIGGER_BODY_PREVIEW_CHARS);
        assert_eq!(preview_body(&exact), exact);
    }

    #[test]
    fn a_body_past_the_cap_is_truncated_with_a_trailing_marker() {
        let over = "x".repeat(TRIGGER_BODY_PREVIEW_CHARS + 1);
        let preview = preview_body(&over);
        let expected = "x".repeat(TRIGGER_BODY_PREVIEW_CHARS) + "...";
        assert_eq!(preview, expected);
    }

    /// A multi-line body would otherwise split a table row across output
    /// lines (`TriggeredRows::rows`).
    #[test]
    fn embedded_newlines_and_carriage_returns_are_escaped_not_literal() {
        let preview = preview_body("line one\nline two\r\nline three");
        assert!(!preview.contains('\n'));
        assert!(!preview.contains('\r'));
        assert!(preview.contains("\\n"));
        assert!(preview.contains("\\r"));
    }

    /// Fails if truncation or escaping leaks into `Serialize` instead of
    /// staying in [`TriggeredRows::rows`].
    #[test]
    fn json_carries_the_real_body_the_table_cannot() {
        let long_body = format!(
            "{}\nsecond line",
            "x".repeat(TRIGGER_BODY_PREVIEW_CHARS * 2)
        );
        let replies = TriggeredRows(vec![ActionReply {
            id: 1,
            name: "web".to_string(),
            outcome: ActionOutcome::Replied {
                body: long_body.clone(),
            },
        }]);
        let json = serde_json::to_value(&replies).unwrap();
        assert_eq!(json[0]["outcome"]["body"], long_body);

        let table_cell = &replies.rows()[0][3];
        assert_ne!(
            *table_cell, long_body,
            "the table cell must be the collapsed preview, not the real body"
        );
    }

    fn sample_signal_replies() -> SignalledRows {
        SignalledRows(vec![
            SignalReply {
                id: 1,
                name: "web".to_string(),
                outcome: SignalOutcome::Delivered,
            },
            SignalReply {
                id: 2,
                name: "worker".to_string(),
                outcome: SignalOutcome::NotRunning,
            },
        ])
    }

    /// OUTCOME and DETAIL both derive from `outcome`, a nested JSON object
    /// rather than a scalar, as in `triggered_rows_do_not_drift`.
    #[test]
    fn signalled_rows_do_not_drift() {
        assert_no_drift(&sample_signal_replies(), |j| &j[0], &["OUTCOME", "DETAIL"]);
    }

    #[test]
    fn signalled_rows_render_id_name_and_outcome_kind() {
        let rows = sample_signal_replies().rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "1");
        assert_eq!(rows[0][1], "web");
        assert_eq!(rows[0][2], "delivered");
        assert_eq!(rows[1][0], "2");
        assert_eq!(rows[1][1], "worker");
        assert_eq!(rows[1][2], "not_running");
    }

    #[test]
    fn a_failed_signal_details_the_kernels_reason() {
        let rows = SignalledRows(vec![SignalReply {
            id: 1,
            name: "web".to_string(),
            outcome: SignalOutcome::Failed {
                reason: "No such process".to_string(),
            },
        }])
        .rows();
        assert_eq!(rows[0][2], "failed");
        assert_eq!(rows[0][3], "No such process");
    }

    fn sample_line_replies() -> SentLineRows {
        SentLineRows(vec![
            LineReply {
                id: 1,
                name: "repl".to_string(),
                outcome: LineOutcome::Sent,
            },
            LineReply {
                id: 2,
                name: "worker".to_string(),
                outcome: LineOutcome::NoStdin,
            },
        ])
    }

    /// OUTCOME and DETAIL both derive from `outcome`, a nested JSON object
    /// rather than a scalar, as in `triggered_rows_do_not_drift`.
    #[test]
    fn sent_line_rows_do_not_drift() {
        assert_no_drift(&sample_line_replies(), |j| &j[0], &["OUTCOME", "DETAIL"]);
    }

    #[test]
    fn sent_line_rows_render_id_name_and_outcome_kind() {
        let rows = sample_line_replies().rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "1");
        assert_eq!(rows[0][1], "repl");
        assert_eq!(rows[0][2], "sent");
        assert_eq!(rows[1][0], "2");
        assert_eq!(rows[1][1], "worker");
        assert_eq!(rows[1][2], "no_stdin");
    }

    /// The `whisper` sibling of `a_no_channel_detail_names_the_config_field`.
    #[test]
    fn a_no_stdin_detail_names_the_config_field() {
        let rows = sample_line_replies().rows();
        let detail = &rows[1][3];
        assert!(
            detail.contains("stdin = true"),
            "a no_stdin row must name the field that opens one: {detail}"
        );
    }

    #[test]
    fn a_not_written_line_details_the_reason() {
        let rows = SentLineRows(vec![LineReply {
            id: 1,
            name: "repl".to_string(),
            outcome: LineOutcome::NotWritten {
                reason: "pipe is full".to_string(),
            },
        }])
        .rows();
        assert_eq!(rows[0][2], "not_written");
        assert_eq!(rows[0][3], "pipe is full");
    }

    /// One bark delivered to a live sink and one the shepherd wrote itself
    /// with no sinks, shared by every test below.
    fn sample_barks() -> BarkRows {
        BarkRows(vec![
            Bark {
                at_ms: 1_700_000_000_000,
                rule: "restart-storm".to_string(),
                subject: "web".to_string(),
                message: "3 restarts in 60s".to_string(),
                sinks: vec![SinkOutcome {
                    sink: "ops".to_string(),
                    error: None,
                }],
            },
            Bark {
                at_ms: 1_700_000_060_000,
                rule: "daemon".to_string(),
                subject: "worker".to_string(),
                message: "restart budget exhausted".to_string(),
                sinks: vec![],
            },
        ])
    }

    /// `WHEN` and `SINKS` are both human renderings of their own JSON field,
    /// so both sit in `formatted`.
    #[test]
    fn bark_rows_do_not_drift() {
        assert_no_drift(&sample_barks(), |j| &j[0], &["WHEN", "SINKS"]);
    }

    /// `sinks_cell`'s coverage: delivered, refused, and a shepherd-authored
    /// bark with no sinks at all.
    #[test]
    fn sinks_render_delivered_failed_and_empty() {
        let delivered = Bark {
            sinks: vec![SinkOutcome {
                sink: "ops".to_string(),
                error: None,
            }],
            ..sample_barks().0[0].clone()
        };
        assert_eq!(sinks_cell(&delivered.sinks), "ops");

        let failed = Bark {
            sinks: vec![SinkOutcome {
                sink: "ops".to_string(),
                error: Some("connection refused".to_string()),
            }],
            ..sample_barks().0[0].clone()
        };
        assert_eq!(sinks_cell(&failed.sinks), "ops(failed)");

        assert_eq!(sinks_cell(&[]), "-");
    }

    /// A comma-separated list, each sink carrying its own label.
    #[test]
    fn multiple_sinks_each_carry_their_own_outcome() {
        let sinks = vec![
            SinkOutcome {
                sink: "ops".to_string(),
                error: None,
            },
            SinkOutcome {
                sink: "oncall".to_string(),
                error: Some("timed out".to_string()),
            },
        ];
        assert_eq!(sinks_cell(&sinks), "ops, oncall(failed)");
    }

    /// The cell carries no more than the sink's name plus `(failed)`, never
    /// the error string, which can quote a webhook's HTTP response.
    #[test]
    fn a_failed_sinks_error_text_never_reaches_the_cell() {
        let sinks = vec![SinkOutcome {
            sink: "ops".to_string(),
            error: Some("HTTP 401 from discord.com/api/webhooks/...".to_string()),
        }];
        let cell = sinks_cell(&sinks);
        assert_eq!(cell, "ops(failed)");
        assert!(
            !cell.contains("401") && !cell.contains("discord"),
            "the error text must stay out of the table cell: {cell}"
        );
    }

    /// `shep barks` is newest-last, matching the file on disk.
    #[test]
    fn bark_rows_stay_in_the_order_they_were_given() {
        let rows = sample_barks().rows();
        assert_eq!(rows[0][2], "web", "the older bark stays first");
        assert_eq!(rows[1][2], "worker", "the newer bark stays last");
    }

    /// Driven through a real `TriggeredRows`, so it covers the wiring as well
    /// as the tiers.
    #[test]
    fn a_reply_table_colours_its_outcome_and_leaves_its_detail_alone() {
        let rows = TriggeredRows(vec![
            ActionReply {
                id: 0,
                name: "web".to_string(),
                outcome: ActionOutcome::Replied {
                    body: "swept 3".to_string(),
                },
            },
            ActionReply {
                id: 1,
                name: "api".to_string(),
                outcome: ActionOutcome::TimedOut,
            },
        ])
        .rows_for(coloured(), true);

        assert_eq!(rows[0][0], painted("0", Role::Ink3), "ID is chrome");
        assert_eq!(rows[0][1], "web", "NAME is plain");
        assert_eq!(rows[0][2], painted("replied", Role::Meadow));
        assert_eq!(rows[0][3], "swept 3", "DETAIL carries no colour");
        assert_eq!(rows[1][2], painted("timed_out", Role::Bark));
        assert_eq!(
            rows[1][3], "no reply within the app's own action_timeout",
            "and neither does a failure's DETAIL"
        );
    }

    /// fails if the `-` placeholder rule stops reaching a column whose own
    /// rule declined to paint it: `BarkRows` returns [`Paint::Default`] for a
    /// SINKS cell holding `-`, and `Paint::Default` carries the rule.
    #[test]
    fn a_placeholder_falls_back_to_the_shared_rule() {
        let rows = BarkRows(vec![Bark {
            at_ms: 0,
            rule: "restart-storm".to_string(),
            subject: "web".to_string(),
            message: "restarted 5 times".to_string(),
            sinks: Vec::new(),
        }])
        .rows_for(coloured(), true);
        assert_eq!(rows[0][4], painted("-", Role::Ink3), "no sinks reads as -");
    }

    #[test]
    fn a_bark_whose_sink_refused_is_marked() {
        let bark = |error: Option<String>| Bark {
            at_ms: 0,
            rule: "restart-storm".to_string(),
            subject: "web".to_string(),
            message: "restarted 5 times".to_string(),
            sinks: vec![SinkOutcome {
                sink: "ops".to_string(),
                error,
            }],
        };
        let delivered = BarkRows(vec![bark(None)]).rows_for(coloured(), true);
        assert_eq!(delivered[0][4], painted("ops", Role::Meadow));

        let refused =
            BarkRows(vec![bark(Some("connection refused".to_string()))]).rows_for(coloured(), true);
        assert_eq!(refused[0][4], painted("ops(failed)", Role::Bark));
    }
}
