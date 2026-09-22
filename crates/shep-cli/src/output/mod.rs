//! The versioned output envelope and its two renderings: a JSON envelope
//! (`--format json`) and a padded table (`--format table`, the default).
//!
//! [`Render`] is the single source of truth for both: a payload type
//! implements it once, in [`rows`], and [`emit`] renders it either way. A
//! field added to `Serialize` and forgotten in `rows()` fails that type's
//! anti-drift test rather than silently vanishing from the table.
//!
//! `bleats` does not go through this module: a follow has no end, so it
//! emits its own newline-delimited JSON instead of an envelope.
//!
//! This module names no shep-client type and compiles on every target.

mod described;
mod diagnostics;
mod flock;

// `pub(crate)`: `lookout::theme`'s test module calls `paint::style_for`
// directly to pin the anstyle and ratatui colour bindings against each
// other, and neither `lookout` nor `style` is a descendant of `output`.
pub(crate) mod paint;
mod rows;
mod table;
// `pub(crate)` for `width::char_columns`, which `lookout::view::flock::fit`
// pads by: one rule for how wide a `char` draws, shared by the two surfaces
// that pad a cell, rather than a second copy that drifts on the first
// double-width name. The same reasoning `paint` above is public for.
pub(crate) mod width;

use std::io;

use serde::Serialize;
use shep_core::protocol::SheepRefusal;

use crate::exit::ExitCode;

pub use described::emit_described;
pub use diagnostics::{emit_error, emit_notice};
pub use flock::emit_flock;

// Re-exported for `commands/`, which names every one of these at its own
// crate-root import. `commands/` is `#[cfg(unix)]`-gated, so on Windows
// nothing names them and `unused_imports` still flags it there.
#[cfg_attr(windows, allow(unused_imports))]
pub use rows::{
    AvailableDogRows, BarkRows, DeletedIds, DescribedSecret, DogRow, DogRows, EmptiedFile,
    EmptiedFiles, FlockRows, FlushedRows, ImportEnvRow, ImportEnvRows, ImportRow, ImportRows,
    KillRow, KvEntry, KvRows, KvUnsetRow, LambRows, RolledSheep, RolledSheepRows, SavedRollRow,
    SecretKeyRow, SecretKeyRows, SecretSlotRow, SecretStatus, SecretValueRow, SentLineRows,
    SignalledRows, StartupStep, StartupSteps, TriggeredRows,
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
/// Production wires the process's own; tests wire a pair of `Vec<u8>`, which
/// is what makes every renderer assertion hermetic and safe under the
/// parallel `cargo test` gate. `&mut dyn Write` has no `Debug`, so this needs
/// a manual one: print `Streams { .. }` and nothing else (pinned by this
/// module's own `streams_debug_is_the_redacted_placeholder` test).
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
    /// function (`commands/daemon.rs`'s `ansi_enabled` follows the same
    /// rule for `NO_COLOR`).
    ///
    /// `Presentation::BARE` is the safe default: a construction that
    /// reaches for the wrong value renders exactly what shep printed
    /// before this feature existed.
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
    /// Prints `message` as an error, and hands back the code it printed.
    ///
    /// Returning the code lets a caller write
    /// `return streams.fail(ExitCode::Usage, &message)` rather than naming
    /// the code twice.
    ///
    /// The write's own failure is discarded: a closed stderr must not
    /// change what shep exits with.
    pub fn fail(&mut self, code: ExitCode, message: &str) -> ExitCode {
        let _ = emit_error(&mut *self.err, self.fmt, code.code_str(), message);
        code
    }

    /// Prints `message` as a notice, on stdout.
    ///
    /// Discards its write's failure for the same reason [`Self::fail`]
    /// does. Stdout only: a minority of notices belong on stderr instead
    /// (a warning beside a separate primary output, like `init`'s
    /// shadowed-file notice), which call [`emit_notice`] directly with
    /// `streams.err`. This method is for the majority shape, a notice
    /// that is the command's whole answer.
    pub fn note(&mut self, code: &str, message: &str) {
        let _ = emit_notice(&mut *self.out, self.fmt, code, message);
    }

    /// Prints `message` as a notice, on stderr.
    ///
    /// The stream is the whole difference from [`Self::note`]: a decision
    /// about the reader, not severity. `note` carries what the command
    /// produced; this carries what somebody should know about the run
    /// without it being the answer they asked for.
    ///
    /// Keeping those off stdout lets `shep dogs --available --format json
    /// | jq` work while the operator still sees that entries were skipped.
    /// Discards its write's failure for the same reason [`Self::fail`]
    /// does.
    pub fn aside(&mut self, code: &str, message: &str) {
        let _ = emit_notice(&mut *self.err, self.fmt, code, message);
    }
}

/// Implemented once per command payload. The two methods are the only
/// place a field's presence is decided; `rows::assert_no_drift` compares
/// `headers()` against the serialized keys per payload, so a field added
/// to one and forgotten in the other fails that test rather than passing
/// silently.
///
/// Not object-safe: [`headers`](Render::headers) has no receiver and
/// `Serialize` cannot be a dyn-compatible supertrait, so `Box<dyn Render>`
/// does not compile. Every call site knows its payload type statically;
/// [`emit`] dispatches generically, never dynamically.
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
    /// reordering columns cannot silently repoint a paint rule. The
    /// default skips the `-` placeholder rule, which every table that
    /// can render a `-` reaches through [`rows::Paint::Default`] instead.
    ///
    /// Only called from `table_of`'s boxed path; the plain path calls
    /// [`render_table`], keeping `bare` byte-identical. `status_word` is
    /// a plain parameter, not a `Presentation` field: it is `table_of`'s
    /// per-attempt retry knob.
    fn rows_for(&self, _presentation: Presentation, _status_word: bool) -> Vec<Vec<String>> {
        self.rows()
    }
    /// Table header -> JSON key, the documented name mapping
    /// (`UPTIME` -> `uptime_ms`, and so on).
    ///
    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    fn json_key_for(header: &str) -> &'static str;
    /// Serialized fields that legitimately have no column, each with a
    /// comment giving the reason. Usually empty.
    ///
    /// This constant is the only thing standing between an unmapped
    /// `Serialize` field and a silently-widened, unreviewed pass of
    /// `assert_no_drift` (rows.rs): an entry proves the count of covered
    /// keys matches, never why a field belongs here. Every entry an impl
    /// adds must carry its own inline `//` comment stating that reason
    /// (`"note", // internal only, never shown to a user`).
    const JSON_ONLY: &'static [&'static str];

    /// Per-column drop priority for [`table::render_boxed`], parallel to
    /// [`Self::headers`]: index `i` here is the priority of column `i`
    /// there. `0` never drops. The default is all zeros; leaving a real
    /// impl at the default silently opts it out of narrowing.
    ///
    /// The rule every impl in [`rows`] follows: `0` for identity and
    /// status-shaped columns, `1`-`5` reserved for [`rows::FlockRows`]'s
    /// own five columns (UPTIME 1, PID 2, MEM 3, RESTARTS 4, CPU 5), `6`
    /// and up for everything else, ranked per table by how droppable it
    /// is. `priorities_line_up_with_headers_for_every_render_impl`
    /// (rows.rs) is the anti-drift gate enforcing it.
    const PRIORITIES: &'static [u8] = &[];
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
            serde_json::to_writer(&mut *out, &envelope)?;
            writeln!(out)
        }
        Format::Table => write!(out, "{}", table_of(&data, style)),
    }
}

/// Renders one [`Render`] payload as [`render_table`] or
/// [`table::render_boxed`], whichever `presentation.level` calls for.
///
/// Factored out so `emit`, `emit_flock` and `emit_described` make this
/// decision once. `presentation.width` is [`terminal_width`] already
/// resolved at the seam, injected here rather than measured, so this
/// stays testable at any width.
///
/// Renders twice when the first pass drops a column: the STATUS word
/// drops first, so a first attempt asks [`Render::rows_for`] with the
/// word on, and only if [`table::render_boxed_ex`] hid a column does a
/// second attempt ask again with the word off.
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

/// The terminal's width, or 80 when there is not one.
///
/// `crossterm` is a `shep-cli` dependency only inside its `cfg(unix)`
/// block, so a Windows build does not link a terminal stack it can never
/// use. A width of `0`, which some terminals and CI harnesses report, is
/// treated the same as absent: `render_boxed` would otherwise read it as
/// drop every droppable column.
///
/// `pub(crate)` rather than private: its one caller, `lib.rs`'s
/// `run_argv`, resolves [`crate::style::Presentation::width`] once at the
/// seam, never [`table_of`] itself.
pub(crate) fn terminal_width() -> usize {
    #[cfg(unix)]
    {
        crossterm::terminal::size().map_or(80, |(w, _)| match w {
            0 => 80,
            w => usize::from(w),
        })
    }
    #[cfg(not(unix))]
    {
        80
    }
}

/// The `--format json` envelope for a verb that did part of what it was
/// asked and was refused the rest: [`OutputEnvelope`] plus a `refused` key
/// beside `data`.
///
/// One object, because `cli.rs` publishes `--format json` as one object per
/// invocation and a staged reload has two halves to report. A key added
/// beside `data` is additive, so it does not move [`SCHEMA_VERSION`], whose
/// rule is a rename, a removal or a retype of `data` itself.
///
/// `refused` is dropped when empty rather than rendered as `[]`: a reload
/// that refused nothing is every reload bar the staged walk, and those keep
/// the exact three fields every other verb prints.
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
    serde_json::to_writer(&mut *out, &envelope)?;
    writeln!(out)
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

    use shep_core::protocol::{DogSource, ProcessInfo};
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
        vec![sheep_info("web"), dog_info("bark", DogSource::BuiltIn)]
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
        let mut info = dog_info(name, DogSource::BuiltIn);
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
    /// directly, the same two calls `table_of`'s own two-pass retry makes.
    ///
    /// Width 90, not this module's usual 80: `SMIT` and `CFG` cost the
    /// same extra columns `output/table.rs`'s own tests record.
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
