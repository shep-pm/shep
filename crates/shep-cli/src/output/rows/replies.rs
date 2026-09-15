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

use super::process::{Paint, paint, reply_paint};

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
