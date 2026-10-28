//! The versioned output envelope and its two renderings: a JSON envelope
//! (`--format json`) and a padded table (`--format table`, the default).
//!
//! [`Render`] is the single source of truth for both: a payload type
//! implements it once, in [`rows`], and [`emit`] renders it either way, so
//! a field missing from `rows()` fails that type's anti-drift test.
//!
//! `bleats` does not go through this module: a follow has no end, so it
//! emits its own newline-delimited JSON instead of an envelope. This
//! module names no shep-client type and compiles on every target.

mod described;
mod diagnostics;
mod flock;
// `pub(crate)`: `commands/bleats` is outside `output` and renders every
// log line through these three functions.
pub(crate) mod log_line;

// `pub(crate)`: `lookout::theme`'s test module calls `paint::style_for`
// directly to pin the anstyle and ratatui colour bindings against each
// other, and neither `lookout` nor `style` is a descendant of `output`.
pub(crate) mod paint;
mod rows;
mod table;
// `pub(crate)` for `width::char_columns` and `width::text_columns`, which
// `lookout::view::flock::fit` and the view's measuring sites pad and sum
// by: one rule for how wide a `char` draws, shared rather than a second
// copy that drifts. The same reasoning `paint` above is public for.
pub(crate) mod width;

use std::io;

use serde::Serialize;
use shep_core::protocol::SheepRefusal;

use crate::exit::ExitCode;

pub use described::emit_described;
pub use diagnostics::{emit_error, emit_notice};
pub use flock::emit_flock;
pub(crate) use flock::{HostListing, emit_fleet_flock, emit_origin_flock};

// Re-exported for `commands/`, which names every one of these at its own
// crate-root import. `commands/` is `#[cfg(unix)]`-gated, so on Windows
// nothing names them and `unused_imports` still flags it there.
#[cfg_attr(windows, allow(unused_imports))]
pub use rows::{
    AvailableDogRows, BarkRows, CopyChange, CopyReport, DeletedIds, DescribedSecret, DogActionRow,
    DogActionStatus, DogRows, EmptiedFile, EmptiedFiles, FlockRows, FlushedRows, HostEntry,
    HostRows, ImportEnvRow, ImportEnvRows, ImportRow, ImportRows, KillRow, KvEntry, KvRows,
    KvUnsetRow, LabelledLambRows, LambRows, RolledSheep, RolledSheepRows, SavedRollRow,
    SecretKeyRow, SecretKeyRows, SecretSlotRow, SecretStatus, SecretValueRow, SentLineRows,
    SetReport, SettingsAnswer, SignalledRows, StartupStep, StartupSteps, TriggeredRows,
};
pub use table::{human_bytes, human_duration, local_timestamp, render_table};

// `pub(crate)`, not part of the block above: outside this module both are
// named only by `lookout`, `exit_cell` by the flock table's EXIT column and
// `cfg_cell` by its CFG column and the sheep detail pane.
pub(crate) use rows::{cfg_cell, exit_cell};

use crate::cli::Format;
use crate::style::Presentation;

/// Bumped only for a breaking change to any command's `data` shape.
/// Additive fields do not bump it.
pub const SCHEMA_VERSION: u32 = 1;

/// The `--format json` envelope every command renders into, `bleats`
/// excepted (module docs above).
#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub struct OutputEnvelope<'a, T> {
    /// [`SCHEMA_VERSION`] at the time this envelope was produced.
    pub schema_version: u32,
    /// The verb that produced this envelope (`"flock"`, `"ping"`, ...).
    pub command: &'a str,
    /// The command's own payload.
    pub data: T,
}

/// The two streams a command writes to.
///
/// Production wires the process's own; tests build a pair of `Vec<u8>`
/// through `Streams::bare`, which is what makes every renderer assertion
/// hermetic and safe under the parallel `cargo test` gate. `&mut dyn Write`
/// has no `Debug`, so this prints `Streams { .. }` and nothing else.
pub struct Streams<'a> {
    /// Rendered command output: what `emit` writes to.
    ///
    /// `commands/` is `#[cfg(unix)]`-gated, so on Windows nothing reads
    /// this field and `dead_code` still flags it there.
    #[cfg_attr(windows, allow(dead_code))]
    pub out: &'a mut dyn io::Write,
    /// Diagnostics and errors: what `emit_error` writes to.
    pub err: &'a mut dyn io::Write,
    /// How much this invocation dresses up its output.
    ///
    /// Carried here rather than as a global: presentation inputs are
    /// parameters in this crate, never a call inside the rendering
    /// function. `Presentation::BARE` is the safe default, so a
    /// construction that reaches for the wrong value renders plain output.
    pub style: Presentation,
    /// How this invocation renders: a table for a person, or JSON for a
    /// script.
    ///
    /// Carried here for the same reason as `style`: it reaches every
    /// command already.
    pub fmt: Format,
}

impl std::fmt::Debug for Streams<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Streams").finish_non_exhaustive()
    }
}

impl Streams<'_> {
    /// Prints `message` as an error, and hands back the code it printed,
    /// so a caller names it once.
    ///
    /// The write's own failure is discarded: a closed stderr must not
    /// change what shep exits with.
    pub fn fail(&mut self, code: ExitCode, message: &str) -> ExitCode {
        let _ = emit_error(&mut *self.err, self.fmt, code.code_str(), message);
        code
    }

    /// Prints `message` as a notice, on stdout: the notice that is the
    /// command's whole answer.
    ///
    /// Discards its write's failure for the same reason [`Self::fail`]
    /// does. A notice beside a separate primary output, like `init`'s
    /// shadowed-file one, calls [`emit_notice`] with `streams.err` instead.
    pub fn note(&mut self, code: &str, message: &str) {
        let _ = emit_notice(&mut *self.out, self.fmt, code, message);
    }

    /// Prints `message` as a notice, on stderr.
    ///
    /// The stream is the whole difference from [`Self::note`]: a decision
    /// about the reader, not severity. Keeping these off stdout lets
    /// `shep dogs --available --format json | jq` work while the operator
    /// still sees that entries were skipped.
    pub fn aside(&mut self, code: &str, message: &str) {
        let _ = emit_notice(&mut *self.err, self.fmt, code, message);
    }
}

/// The constructor the command tests build their streams from, test-only
/// because production wires the process's own streams and the level the
/// operator resolved, in `dispatch.rs`.
///
/// `BARE` because these tests assert on words, not colour. Takes
/// `&mut dyn io::Write` rather than `&mut Vec<u8>` so a test can hand it a
/// writer that fails, or one that discards.
#[cfg(test)]
impl<'a> Streams<'a> {
    /// A `Streams` that writes to `out` and `err` exactly as given, at
    /// [`Presentation::BARE`] and format `fmt`.
    pub(crate) fn bare(
        out: &'a mut dyn io::Write,
        err: &'a mut dyn io::Write,
        fmt: Format,
    ) -> Self {
        Self {
            out,
            err,
            style: Presentation::BARE,
            fmt,
        }
    }
}

/// Implemented once per payload. The two methods are the only place a
/// field's presence is decided; `rows::assert_no_drift` compares
/// `headers()` against the serialized keys, so a field added to one and
/// forgotten in the other fails that test.
///
/// Not object-safe: [`headers`](Render::headers) has no receiver and
/// `Serialize` is not dyn-compatible, so every call site is static.
#[allow(dead_code)]
pub trait Render: Serialize {
    /// Column headers for table output.
    fn headers() -> &'static [&'static str];
    /// One row per record, cells in `headers()` order.
    fn rows(&self) -> Vec<Vec<String>>;
    /// The rows as this presentation wants them rendered.
    ///
    /// Defaults to [`Self::rows`]; an override calls [`rows::paint`],
    /// which keys each cell on the column's NAME, not its index, so
    /// reordering columns cannot repoint a paint rule. Only called from
    /// `table_of`'s boxed path; the plain path calls [`render_table`],
    /// keeping `bare` byte-identical.
    fn rows_for(&self, _presentation: Presentation, _status_word: bool) -> Vec<Vec<String>> {
        self.rows()
    }
    /// Table header -> JSON key.
    ///
    /// Defaults to the lowercased header; a table whose key is renamed
    /// overrides it, and an override panics on a header outside its own
    /// `headers()`. No rendering path calls it: only `rows`' anti-drift
    /// tests do, which fail on a header with no key or a key with no header.
    fn json_key_for(header: &str) -> String {
        header.to_lowercase()
    }
    /// Serialized fields that legitimately have no column, each with a
    /// comment giving the reason. Empty by default.
    ///
    /// This constant is the only thing standing between an unmapped
    /// `Serialize` field and a silently-widened `assert_no_drift`
    /// (rows.rs): an entry proves the counts match, never why a field
    /// belongs here. Every entry must carry its own inline `//` comment.
    const JSON_ONLY: &'static [&'static str] = &[];

    /// Per-column drop priority for [`table::render_boxed`], parallel to
    /// [`Self::headers`]: index `i` here is the priority of column `i`
    /// there. `0` never drops; the all-zero default opts an impl out of
    /// narrowing. `priorities_line_up_with_headers_for_every_render_impl`
    /// enforces the rule every impl in [`rows`] follows.
    const PRIORITIES: &'static [u8] = &[];
}

/// Writes `value` to `out` as one JSON line: the serialized value, then a
/// newline.
///
/// The one place a `--format json` envelope reaches a writer, so the
/// newline rule is decided once: a change to it moves every verb's bytes
/// at once rather than some of them. `bleats` stays outside, as the module
/// docs say: a follow has no end, so it has no envelope to end.
///
/// Compact [`serde_json`] output.
///
/// # Errors
/// The underlying write failed.
pub(crate) fn write_json_line<T: Serialize + ?Sized>(
    out: &mut dyn io::Write,
    value: &T,
) -> io::Result<()> {
    serde_json::to_writer(&mut *out, value)?;
    writeln!(out)
}

/// Renders `data` to `out` as `fmt` calls for, boxed or plain per `style`.
///
/// Called by every command in `commands/` once it has a real payload to
/// render: `write_outcome(emit(&mut *streams.out, fmt, "<verb>", data,
/// streams.style))` is the shape all of them share.
///
/// # Errors
/// The underlying write failed.
pub fn emit<T: Render>(
    out: &mut dyn io::Write,
    fmt: Format,
    command: &str,
    data: T,
    style: Presentation,
) -> io::Result<()> {
    match fmt {
        Format::Json => {
            let envelope = OutputEnvelope {
                schema_version: SCHEMA_VERSION,
                command,
                data,
            };
            write_json_line(&mut *out, &envelope)
        }
        Format::Table => write!(out, "{}", table_of(&data, style)),
    }
}

/// Renders one [`Render`] payload as [`render_table`] or
/// [`table::render_boxed`], whichever `presentation.level` calls for.
///
/// Factored out so `emit`, `emit_flock` and `emit_described` decide this
/// once. `presentation.width` is [`terminal_width`] resolved at the seam,
/// injected rather than measured. Renders twice when the first pass drops
/// a column: the STATUS word drops first.
pub(crate) fn table_of<T: Render>(data: &T, presentation: Presentation) -> String {
    if !presentation.level.boxes() {
        return render_table(data);
    }
    let headers = T::headers();
    let width = presentation.width;
    let wide = table::render_boxed_ex(
        headers,
        &data.rows_for(presentation, true),
        T::PRIORITIES,
        width,
    );
    if wide.dropped.is_empty() {
        return wide.rendered;
    }
    table::render_boxed_ex(
        headers,
        &data.rows_for(presentation, false),
        T::PRIORITIES,
        width,
    )
    .rendered
}

/// The terminal's width, or 80 when there is not one
///
/// A width of `0`, which some terminals and CI harnesses report, counts as
/// absent: `render_boxed` would otherwise drop every droppable column.
///
/// `pub(crate)` for its one caller, `entry.rs`, which resolves
/// [`crate::style::Presentation::width`] once rather than per table.
pub(crate) fn terminal_width() -> usize {
    crossterm::terminal::size().map_or(80, |(w, _)| match w {
        0 => 80,
        w => usize::from(w),
    })
}

/// The `--format json` envelope for a partly-succeeded, partly-refused
/// verb: [`OutputEnvelope`] plus a `refused` key beside `data`.
///
/// One object, because `cli.rs` publishes `--format json` as one object
/// per invocation and a staged reload has two halves to report. A key
/// beside `data` is additive, so [`SCHEMA_VERSION`] does not move, and
/// `refused` is dropped when empty rather than rendered as `[]`.
#[derive(Debug, Serialize)]
#[cfg_attr(windows, allow(dead_code))]
struct PartialEnvelope<'a, T> {
    /// [`SCHEMA_VERSION`] at the time this envelope was produced.
    schema_version: u32,
    /// The verb that produced this envelope.
    command: &'a str,
    /// What the verb did: the same payload [`OutputEnvelope`] carries.
    data: T,
    /// What it did not do, one entry per app the shepherd refused.
    #[serde(skip_serializing_if = "no_refusals")]
    refused: &'a [SheepRefusal],
}

/// Whether `refused` is empty, for [`PartialEnvelope`]'s
/// `skip_serializing_if`.
///
/// Takes a double reference because serde hands the attribute a reference
/// to the field, and the field is itself a slice reference.
#[cfg_attr(windows, allow(dead_code))]
fn no_refusals(refused: &&[SheepRefusal]) -> bool {
    refused.is_empty()
}

/// Renders `data` and `refused` to `out` as one `--format json` envelope.
///
/// JSON only, and the caller checks that: the table rendering of a partial
/// answer is a fresh flock listing plus a line on stderr, which
/// `commands::lifecycle` composes itself out of [`emit_flock`] and
/// [`Streams::fail`].
///
/// # Errors
/// The underlying write failed.
#[cfg_attr(windows, allow(dead_code))]
pub fn emit_partial<T: Render>(
    out: &mut dyn io::Write,
    command: &str,
    data: T,
    refused: &[SheepRefusal],
) -> io::Result<()> {
    let envelope = PartialEnvelope {
        schema_version: SCHEMA_VERSION,
        command,
        data,
        refused,
    };
    write_json_line(&mut *out, &envelope)
}

/// Turns the result of an `emit`/`emit_error` write into the exit code that
/// write earned.
///
/// A write failure is [`ExitCode::Failure`], except
/// [`io::ErrorKind::BrokenPipe`], which is [`ExitCode::Success`]:
/// `shep flock | head` closes the pipe on purpose, and that is not a
/// failed command.
#[must_use]
pub fn write_outcome(result: io::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::Success,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::Success,
        Err(_) => ExitCode::Failure,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::ffi::OsStr;

    use shep_core::protocol::{DogSource, HostUsage, ProcessInfo};
    use shep_core::status::ProcStatus;

    use crate::output::rows::tests::{dog_info, sample_flock, sample_info};
    use crate::style::StyleLevel;

    use super::*;

    /// A sheep named `name`, otherwise `rows::tests::sample_info`'s usual
    /// fixture. A thin wrapper rather than reaching for that function
    /// directly: this module's own tests build listings by name (`"web"` a
    /// sheep, `"bark"` a dog), and this is the sheep half of that shape.
    pub(crate) fn sheep_info(name: &str) -> ProcessInfo {
        sample_info(1, name, 60_000)
    }

    /// One sheep (`"web"`), one dog (`"bark"`): the smallest listing that
    /// exercises `emit_flock`'s split, shared by the three tests below.
    pub(crate) fn mixed_listing() -> Vec<ProcessInfo> {
        vec![
            sheep_info("web"),
            dog_info("bark", DogSource::BuiltIn { grants: None }),
        ]
    }

    /// Pins the JSON envelope's exact shape (`--format json` is a stability
    /// surface, same discipline as the wire protocol). A field renamed or
    /// reordered here is a `schema_version` bump, not a silent re-accept.
    #[test]
    fn the_json_envelope_shape_is_pinned() {
        let out = OutputEnvelope {
            schema_version: SCHEMA_VERSION,
            command: "flock",
            data: sample_flock(),
        };
        insta::assert_json_snapshot!(out);
    }

    /// The one write every `--format json` envelope goes through: the
    /// value's compact JSON, then exactly one newline, and nothing between
    /// them. The literal is the spec: `--format json` is one object per
    /// invocation, so a second newline (or none) is a shape change.
    #[test]
    fn write_json_line_ends_one_line_with_a_newline() {
        #[derive(Serialize)]
        struct OneLine {
            name: &'static str,
        }
        let mut out = Vec::new();
        write_json_line(&mut out, &OneLine { name: "web" }).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "{\"name\":\"web\"}\n");
    }

    /// Pins the fleet envelope's exact shape: a local row, a row from `amp2`,
    /// a host with usage, one whose platform `sysinfo` cannot read (`null`)
    /// and one that listed its rows but would not answer the usage request
    /// (no entry).
    #[test]
    fn the_fleet_json_envelope_shape_is_pinned() {
        let reading = HostUsage {
            cpu_percent: Some(11.5),
            memory_used_bytes: 4_294_967_296,
            memory_total_bytes: 8_589_934_592,
            disk_bytes_per_second: Some((1_024, 2_048)),
            network_bytes_per_second: None,
        };
        let listing = |host: Option<&str>, name: &str, usage| HostListing {
            host: host.map(str::to_owned),
            procs: vec![sheep_info(name)],
            usage,
        };
        let mut out = Vec::new();
        emit_fleet_flock(
            &mut out,
            "flock",
            vec![
                listing(None, "web", Some(Some(reading))),
                listing(Some("amp2"), "api", Some(Some(reading))),
                listing(Some("amp3"), "cache", Some(None)),
                listing(Some("amp4"), "old", None),
            ],
        )
        .unwrap();
        let out: serde_json::Value = serde_json::from_slice(&out).unwrap();
        insta::assert_json_snapshot!(out);
    }

    /// `emit` must not put the envelope wrapper on the table surface, and
    /// must not put the table on the JSON surface. An implementation that
    /// ignored `fmt` and always JSON-encoded would pass both format tests
    /// above individually but fail this one.
    #[test]
    fn emit_honours_the_format_it_is_given() {
        let mut json_out = Vec::new();
        emit(
            &mut json_out,
            Format::Json,
            "flock",
            sample_flock(),
            Presentation::BARE,
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&json_out).unwrap();
        assert_eq!(parsed["command"], "flock");
        assert_eq!(parsed["data"].as_array().unwrap().len(), 3);

        let mut table_out = Vec::new();
        emit(
            &mut table_out,
            Format::Table,
            "flock",
            sample_flock(),
            Presentation::BARE,
        )
        .unwrap();
        let text = String::from_utf8(table_out).unwrap();
        assert!(text.contains("NAME"));
        assert!(
            !text.contains("schema_version"),
            "the envelope is a JSON-only concept"
        );
    }

    /// A silent dog: process up, and it has never answered this shepherd.
    /// `given_up` is the latch: `Some(true)` for a dog the shepherd has
    /// stopped restarting, `Some(false)` for one it is still waiting on,
    /// `None` for a shepherd too old to have an opinion.
    pub(crate) fn silent_dog(name: &str, given_up: Option<bool>) -> ProcessInfo {
        let mut info = dog_info(name, DogSource::BuiltIn { grants: None });
        info.status = ProcStatus::Online;
        info.handshook = Some(false);
        info.dog_stale = given_up;
        info
    }

    /// `Streams` carries `&mut dyn io::Write`, which has no `Debug` of its
    /// own, so the manual impl is the only thing standing between a future
    /// refactor and a `Debug` that leaks whatever the streams hold.
    #[test]
    fn streams_debug_is_the_redacted_placeholder() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        assert_eq!(format!("{streams:?}"), "Streams { .. }");
    }

    /// `Streams::bare` is what every command test builds its streams from,
    /// so it must hand each buffer to the field the caller named: a test
    /// that reads `out` would otherwise pass on what the command wrote to
    /// `err`. Both writes go through the struct's own methods, so this
    /// pins the fields and not just the constructor.
    #[test]
    fn bare_streams_write_to_the_buffers_given_and_carry_their_format() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (fmt, style) = {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Json);
            streams.note("OUT", "to-out");
            streams.aside("ERR", "to-err");
            (streams.fmt, streams.style)
        };

        let out_text = String::from_utf8(out).unwrap();
        let err_text = String::from_utf8(err).unwrap();
        assert!(
            out_text.contains("to-out") && !out_text.contains("to-err"),
            "`out` must hold only what the command wrote to stdout: {out_text:?}"
        );
        assert!(
            err_text.contains("to-err") && !err_text.contains("to-out"),
            "`err` must hold only what the command wrote to stderr: {err_text:?}"
        );
        assert_eq!(fmt, Format::Json);
        assert_eq!(style, Presentation::BARE);
    }

    #[test]
    fn write_outcome_treats_a_broken_pipe_as_success() {
        // `shep flock | head` closes the pipe on purpose; that is not a
        // failed command.
        let broken = io::Error::from(io::ErrorKind::BrokenPipe);
        assert_eq!(write_outcome(Err(broken)), ExitCode::Success);
    }

    #[test]
    fn write_outcome_treats_every_other_write_error_as_failure() {
        let other = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(write_outcome(Err(other)), ExitCode::Failure);
    }

    #[test]
    fn write_outcome_treats_ok_as_success() {
        assert_eq!(write_outcome(Ok(())), ExitCode::Success);
    }

    /// `NO_COLOR` removes colour at `full`, leaving sheep and boxes alone.
    /// Asserted on the rendered string, not the resolved [`Presentation`]:
    /// the struct could fold `NO_COLOR` in correctly and a bug in
    /// `rows::status_cell` could still emit an escape regardless.
    #[test]
    fn no_color_at_full_keeps_sheep_and_boxes_but_drops_colour() {
        let presentation =
            Presentation::new(StyleLevel::Full, Some(OsStr::new("1")), None, None, 80);
        assert!(
            !presentation.colour,
            "NO_COLOR must veto colour even at full"
        );

        let flock = FlockRows(vec![
            ProcessInfo::builder(1, "web", ProcStatus::Online).build(),
        ]);
        let rendered = table_of(&flock, presentation);

        assert!(
            rendered.contains("(o.o)"),
            "full still draws the face: {rendered}"
        );
        assert!(rendered.contains('┌'), "full still draws boxes: {rendered}");
        assert!(
            !rendered.contains('\u{1b}'),
            "NO_COLOR must leave no escape byte: {rendered:?}"
        );
    }

    /// The byte-identical rule, made mechanical: `bare` must never emit an
    /// ANSI escape, regardless of status or how loud the environment's
    /// colour support would otherwise be.
    #[test]
    fn bare_emits_no_escape_at_all() {
        let flock = FlockRows(vec![
            ProcessInfo::builder(1, "web", ProcStatus::Errored).build(),
        ]);
        let rendered = table_of(&flock, Presentation::BARE);
        assert!(!rendered.contains('\u{1b}'), "{rendered:?}");
        assert!(
            rendered.contains("errored"),
            "today's plain word survives: {rendered}"
        );
        assert!(!rendered.contains("(x.x)"), "no face at bare: {rendered}");
    }

    /// The face appears at `full`; at `plain` the plain word alone does
    /// (`plain` is "no sheep", not "no colour"); neither survives at
    /// `bare`.
    ///
    /// Run with `-- --nocapture` to read what each level looks like: an
    /// exact-string test proves the code matches a string, not that the
    /// result is legible.
    #[test]
    fn the_three_levels_render_the_status_column_differently_and_look_right() {
        let flock = FlockRows(vec![
            ProcessInfo::builder(1, "web", ProcStatus::Online).build(),
            ProcessInfo::builder(2, "worker", ProcStatus::Errored).build(),
            ProcessInfo::builder(3, "cron", ProcStatus::Stopped).build(),
        ]);

        let full = table_of(
            &flock,
            Presentation::new(
                StyleLevel::Full,
                None,
                Some(OsStr::new("xterm-256color")),
                None,
                80,
            ),
        );
        println!("--- full ---\n{full}");
        assert!(full.contains("(o.o)"), "{full}");
        assert!(full.contains("(x.x)"), "{full}");
        assert!(full.contains("(-.-)"), "{full}");
        assert!(
            full.contains('\u{1b}'),
            "full at a deep terminal colours the cell: {full:?}"
        );

        let plain = table_of(
            &flock,
            Presentation::new(
                StyleLevel::Plain,
                None,
                Some(OsStr::new("xterm-256color")),
                None,
                80,
            ),
        );
        println!("--- plain ---\n{plain}");
        assert!(!plain.contains("(o.o)"), "no face at plain: {plain}");
        assert!(plain.contains("online"), "{plain}");
        assert!(plain.contains('\u{1b}'), "plain still colours: {plain:?}");

        let bare = table_of(&flock, Presentation::BARE);
        println!("--- bare ---\n{bare}");
        assert!(!bare.contains("(o.o)"), "{bare}");
        assert!(!bare.contains('\u{1b}'), "{bare:?}");
    }

    /// The STATUS word drops before any whole column does.
    /// `waiting-restart` (15 characters) is the longest status word,
    /// chosen so face-plus-word forces a column past a width face-alone
    /// fits. Exercises `Render::rows_for` and `table::render_boxed_ex`
    /// directly, the two calls `table_of`'s two-pass retry makes.
    ///
    /// Width 90, not this module's usual 80: `SMIT` and `CFG` cost columns.
    #[test]
    fn the_word_drops_before_a_whole_column_does() {
        let flock = FlockRows(vec![
            ProcessInfo::builder(1, "a", ProcStatus::WaitingRestart).build(),
        ]);
        let presentation = Presentation::new(StyleLevel::Full, None, None, None, 90);
        let headers = FlockRows::headers();

        let wide = table::render_boxed_ex(
            headers,
            &flock.rows_for(presentation, true),
            FlockRows::PRIORITIES,
            90,
        );
        assert!(
            !wide.dropped.is_empty(),
            "face-plus-word should already force a drop at 90: {}",
            wide.rendered
        );

        let narrow = table::render_boxed_ex(
            headers,
            &flock.rows_for(presentation, false),
            FlockRows::PRIORITIES,
            90,
        );
        assert!(
            narrow.dropped.is_empty(),
            "face-alone should fit every column at 90: {}",
            narrow.rendered
        );
        assert!(narrow.rendered.contains("FOLD"), "{}", narrow.rendered);
        assert!(narrow.rendered.contains("(>_<)"), "{}", narrow.rendered);
        assert!(
            !narrow.rendered.contains("waiting-restart"),
            "{}",
            narrow.rendered
        );
    }

    /// The JSON arms serialize the payload directly and never call
    /// `rows`/`rows_for`.
    #[test]
    fn colour_never_reaches_format_json() {
        let flock = FlockRows(vec![
            ProcessInfo::builder(1, "web", ProcStatus::Errored).build(),
        ]);
        let presentation = Presentation::new(
            StyleLevel::Full,
            None,
            Some(OsStr::new("xterm-256color")),
            None,
            80,
        );
        let mut out = Vec::new();
        emit(&mut out, Format::Json, "flock", flock, presentation).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains('\u{1b}'), "{text}");

        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["data"][0]["status"], "errored");
    }
}
