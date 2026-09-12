//! Fixtures the pane test modules share.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use shep_client::RequestError;
use shep_core::config::{AppConfig, ProbeConfig, ProbeKind};
use shep_core::protocol::{
    BusEvent, DogSource, Lamb, ProcessInfo, Response, RpcError, RpcErrorCode, SheepConfigView,
};
use shep_core::status::ProcStatus;
use shep_core::values::UpDuration;

use super::super::app::{
    ActionVerb, App, Body, CloseDialog, Control, Effect, KeyPress, LambWalk, Msg, RevealedValue,
    RowKey, Sent, SettingsRow,
};
use super::super::level::Level;
use super::super::pane::{ConfigPane, ReloadKind};
use super::super::secrets::{SecretRow, SecretsModel, Source};
use super::super::source::HostSample;
use super::super::tail::{Stream, Tail, TailLine};
use super::super::theme::Palette;
use crate::commands::settings::{DogView, ScalarView, SettingField, SettingsSnapshot};
use crate::secret_readers::Reader;
use crate::style::StyleSource;

/// What the last dial said when the ladder ran out, in
/// `super::super::source::LinkError::Unreachable`'s own shape.
///
/// The link panel renders it verbatim, so every test that freezes a
/// dashboard feeds it verbatim rather than inventing a shorter sentence the
/// panel would never see.
pub const FROZEN_WHY: &str = "the shepherd did not answer: could not connect to `/home/ada/.shep/run/shep.sock`: Connection refused (os error 61)";

/// No colour at all: the palette every fixture uses unless the test is about
/// colour.
pub fn plain() -> Palette {
    Palette::detect(None, None, None)
}

/// The 256-colour palette, for the two tests that assert on a specific
/// foreground.
pub fn coloured() -> Palette {
    Palette::detect(None, Some(OsStr::new("xterm-256color")), None)
}

/// A dashboard with `flock` listed and nothing else applied.
pub fn app_with(flock: Vec<ProcessInfo>, palette: Palette) -> App {
    let t0 = Instant::now();
    let mut app = App::new(
        palette,
        Control::ReadOnly,
        "/home/ada/.shep".to_string(),
        t0,
    );
    app.update(Msg::Snapshot {
        rows: flock,
        at: t0,
    });
    app
}

/// One online sheep named `name`, carrying `fold`, for the fold-grouping
/// tests.
pub fn sheep_in_fold(id: u32, name: &str, fold: Option<&str>) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online)
        .pid(Some(1000 + id))
        .uptime_ms(60_000)
        .fold(fold.map(str::to_string))
        .build()
}

/// One sheep in `fold` with a chosen status, for the mixed-status rows.
#[must_use]
pub fn sheep_in_fold_with_status(
    id: u32,
    name: &str,
    fold: Option<&str>,
    status: ProcStatus,
) -> ProcessInfo {
    ProcessInfo::builder(id, name, status)
        .pid(Some(1000 + id))
        .uptime_ms(60_000)
        .fold(fold.map(str::to_string))
        .build()
}

/// One online sheep named `name`, carrying `fold`, `uptime_ms`, `memory` and
/// `restarts`: the shape [`crate::lookout::app::App::fold_totals`]'s rollup
/// test needs numbers to sum and to take the minimum of.
pub fn sheep_with(
    id: u32,
    name: &str,
    fold: Option<&str>,
    uptime_ms: u64,
    memory: Option<u64>,
    restarts: u32,
) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online)
        .pid(Some(1000 + id))
        .uptime_ms(uptime_ms)
        .memory_bytes(memory)
        .restarts(restarts)
        .fold(fold.map(str::to_string))
        .build()
}

/// One instance of a grouped app, at `slot`, carrying `fold`: the same
/// shape [`sheep_in_fold`] builds, with an instance slot set so
/// [`super::super::app::App::is_grouped`] gathers it under a
/// [`RowKey::Group`] header.
pub fn instance_in_fold(id: u32, name: &str, slot: u32, fold: Option<&str>) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online)
        .pid(Some(1000 + id))
        .uptime_ms(60_000)
        .instance(Some(slot))
        .fold(fold.map(str::to_string))
        .build()
}

/// `count` online sheep, the first `with_readings` of which report cpu and
/// memory. The rest report neither, which is the case the `-` assertions need.
pub fn flock_of(count: u32, with_readings: u32) -> Vec<ProcessInfo> {
    (0..count)
        .map(|id| {
            let reports = id < with_readings;
            ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Online)
                .pid(Some(48_000 + id))
                .uptime_ms(4_512_000)
                .cpu_percent(reports.then_some(3.5))
                .memory_bytes(reports.then_some(182 << 20))
                .out_file(Some(format!("/home/ada/.shep/logs/sheep-{id}-out.log")))
                .err_file(Some(format!("/home/ada/.shep/logs/sheep-{id}-err.log")))
                .build()
        })
        .collect()
}

/// Two sheep and one dog, for the flock table's section-header tests: a
/// [`RowKey::Section`] splits the two kinds.
pub fn app_with_a_dog() -> App {
    let flock = vec![
        ProcessInfo::builder(1, "web", ProcStatus::Online)
            .pid(Some(48_001))
            .uptime_ms(4_512_000)
            .build(),
        ProcessInfo::builder(2, "api", ProcStatus::Online)
            .pid(Some(48_002))
            .uptime_ms(4_512_000)
            .build(),
        ProcessInfo::builder(90, "otel", ProcStatus::Online)
            .pid(Some(90_000))
            .dog(Some(DogSource::BuiltIn))
            .build(),
    ];
    app_with(flock, plain())
}

/// A dashboard with `otel` adopted from `/opt/otel`, selected, and the
/// control gate open.
pub fn app_with_a_dog_selected_and_control() -> App {
    let decoy = ProcessInfo::builder(0, "!decoy", ProcStatus::Online).build();
    let dog = ProcessInfo::builder(90, "otel", ProcStatus::Online)
        .pid(Some(90_000))
        .dog(Some(DogSource::Adopted {
            path: "/opt/otel".to_string(),
        }))
        .build();
    let mut app = app_with(vec![decoy, dog], plain());
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::SelectDown));
    app
}

/// The same, with `otel` built in rather than adopted, so it carries no path.
pub fn app_with_a_built_in_dog_selected_and_control() -> App {
    let decoy = ProcessInfo::builder(0, "!decoy", ProcStatus::Online).build();
    let dog = ProcessInfo::builder(90, "otel", ProcStatus::Online)
        .pid(Some(90_000))
        .dog(Some(DogSource::BuiltIn))
        .build();
    let mut app = app_with(vec![decoy, dog], plain());
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::SelectDown));
    app
}

/// One plausible host reading: the same numbers the gallery's scenes use, so
/// a failure here and a frame under review name the same figures.
pub fn sample() -> HostSample {
    HostSample {
        load: (2.31, 4.10, 3.88),
        cores: Some(10),
        memory_total_bytes: 32 << 30,
        memory_used_bytes: 12 * (1 << 30) + (410 << 20),
        uptime_seconds: 6 * 86_400 + 3 * 3_600,
    }
}

/// A dashboard that has had one host sample applied.
pub fn with_host(sample: HostSample, flock: Vec<ProcessInfo>) -> App {
    let mut app = app_with(flock, plain());
    app.update(Msg::Host {
        sample: Some(sample),
    });
    app
}

/// A dashboard with no host reading. The two ways of having none are not the
/// same state: `unsupported: true` applies `Msg::Host { sample: None }`, the
/// signal a `sysinfo` that does not support the platform produces, and the
/// strip says so. `unsupported: false` applies no `Msg::Host` at all, the
/// state before the first heartbeat, and the strip says `not read yet`
/// instead.
pub fn with_host_none(flock: Vec<ProcessInfo>, unsupported: bool) -> App {
    let mut app = app_with(flock, plain());
    if unsupported {
        app.update(Msg::Host { sample: None });
    }
    app
}

/// A dashboard with a flock of three sheep, the first one selected, and
/// `tail` applied as this refresh's feed.
pub fn with_feed(tail: Tail) -> App {
    let mut app = app_with(flock_of(3, 0), plain());
    app.update(Msg::Bleats { tail });
    app
}

/// Like [`with_feed`], but selects sheep `id` first, for the tests that need
/// the header to name a specific sheep.
pub fn with_feed_and_selection(tail: Tail, id: u32) -> App {
    let mut app = app_with(flock_of(3, 0), plain());
    for _ in 0..id {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
    app.update(Msg::Bleats { tail });
    app
}

/// Like [`with_feed`], but with an explicit palette, for the one test that
/// asserts on a specific foreground colour.
pub fn with_feed_and_palette(tail: Tail, palette: Palette) -> App {
    let mut app = app_with(flock_of(3, 0), palette);
    app.update(Msg::Bleats { tail });
    app
}

/// A dashboard with an empty flock: nothing is selected, so the feed's own
/// "no sheep is selected" line is what renders.
pub fn with_no_selection() -> App {
    app_with(Vec::new(), plain())
}

/// A two-sheep dashboard with the selection walked onto `info`, on row 1
/// rather than row 0, so a `selected_row()` that fell back to the first row
/// would be caught. Both properties are asserted below rather than assumed.
pub fn with_selection(info: ProcessInfo) -> App {
    with_selection_and_palette(info, plain())
}

/// The same, at a given palette.
///
/// The decoy is named `!decoy` rather than `decoy`: the table reads by name,
/// and `!` sorts below every ASCII letter and digit, so the decoy is row 0
/// whatever the sheep under test is called.
pub fn with_selection_and_palette(info: ProcessInfo, palette: Palette) -> App {
    assert!(
        info.id > 0,
        "the decoy takes id 0, so the sheep under test cannot"
    );
    let wanted = info.id;
    let decoy = ProcessInfo::builder(0, "!decoy", ProcStatus::Online).build();
    let mut app = app_with(vec![decoy, info], palette);
    app.update(Msg::Key(KeyPress::SelectDown));
    assert_eq!(
        app.selected(),
        Some(RowKey::Sheep(wanted)),
        "the sheep under test must end up selected, and on row 1: the mutation \
         this fixture exists to catch reads row 0 instead"
    );
    app
}

/// A sheep whose listing carries lambs.
///
/// `ListFlock` never populates this field, so this fixture cannot occur live:
/// the pane must not mention lambs even when handed some.
pub fn sheep_with_lambs() -> ProcessInfo {
    ProcessInfo::builder(9, "gateway", ProcStatus::Online)
        .pid(Some(48_301))
        .lambs(Some(vec![
            Lamb::new(48_302, "node"),
            Lamb::new(48_303, "sh"),
        ]))
        .build()
}

/// [`with_selection`] over [`sheep_with_lambs`] (id 9), with one lamb reading
/// applied for that sheep.
pub fn with_lamb_reading(walk: LambWalk) -> App {
    with_lamb_reading_for(9, walk)
}

/// The same, with the reading pinned to `id` instead, so a test can hand the
/// pane a reading that belongs to a different sheep.
pub fn with_lamb_reading_for(id: u32, walk: LambWalk) -> App {
    let mut app = with_selection(sheep_with_lambs());
    app.update(Msg::Replied {
        sent: Sent::Lambs { id },
        result: reply_for(id, &walk),
    });
    app
}

/// [`with_lamb_reading`] plus the `Instant` the dashboard started at, for the
/// one test that needs to tick the clock forward itself.
pub fn app_with_lamb_reading_at(walk: LambWalk) -> (App, Instant) {
    let t0 = Instant::now();
    let mut app = with_selection(sheep_with_lambs());
    app.update(Msg::Tick { now: t0 });
    app.update(Msg::Replied {
        sent: Sent::Lambs { id: 9 },
        result: reply_for(9, &walk),
    });
    (app, t0)
}

/// The reply that makes the reducer record `walk`. There is no way to set a
/// `LambWalk` directly and there should not be: a fixture that reached past
/// `on_lambs` would stop testing the mapping this pane depends on.
///
/// `Failed` is produced by an `Err` rather than by an unrecognised `Ok`,
/// because the two are the same state and `Err` is the one an operator
/// actually meets.
fn reply_for(id: u32, walk: &LambWalk) -> Result<Response, RequestError> {
    let lambs = match walk {
        LambWalk::Failed => return Err(RequestError::Closed),
        LambWalk::NotWalked => None,
        LambWalk::Walked(lambs) => Some(lambs.clone()),
    };
    Ok(Response::Described(vec![
        ProcessInfo::builder(id, "gateway", ProcStatus::Online)
            .pid(Some(48_301))
            .lambs(lambs)
            .build(),
    ]))
}

/// The pane's lamb line alone, for the tests that compare two renderings of
/// it. Panics if the pane has none, so a regression that dropped the line
/// entirely cannot pass by comparing two absences.
pub fn lamb_line_of(app: &App) -> String {
    render_all(&super::detail::detail_lines(app, 200))
        .lines()
        .find(|line| line.starts_with("lambs  "))
        .map(str::to_string)
        .expect("the pane has a lamb line")
}

/// A dashboard with twelve sheep and a full bleats feed, for the checks that
/// need every pane to have more than it can show.
pub fn full_app() -> App {
    let mut app = app_with(flock_of(12, 12), plain());
    app.update(Msg::Bleats {
        tail: Tail {
            lines: (0..10)
                .map(|n| line(Stream::Out, &format!("line-{n}")))
                .collect(),
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 1_024,
            note: None,
        },
    });
    app
}

/// The full-screen bleats pane, open on `web`, with all three filter axes
/// set (stream `err`, level `warn`, match `pool`) over a feed mixing one
/// line that survives every axis with three that each fail exactly one, for
/// the filter row's own tests.
///
/// Filters are stacked through [`App::bleats_pane_mut_for_tests`] rather
/// than through `o`, `m` and `/`, so a test naming the axes it wants does not
/// have to walk each cycle to reach them.
pub fn bleats_pane_with_filters() -> App {
    let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    app.update(Msg::Bleats {
        tail: Tail {
            lines: vec![
                line(Stream::Err, "ERROR pool exhausted"), // all three hold
                line(Stream::Out, "ERROR pool exhausted"), // wrong stream
                line(Stream::Err, "INFO pool warming"),    // below the minimum
                line(Stream::Err, "ERROR disk full"),      // no match
            ],
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 128,
            note: None,
        },
    });
    app.update(Msg::Key(KeyPress::Bleats));
    let pane = app
        .bleats_pane_mut_for_tests()
        .expect("Msg::Key(KeyPress::Bleats) opened the pane on the sheep selected above");
    pane.set_stream(Some(Stream::Err));
    pane.set_min_level(Some(Level::Warn));
    pane.set_match("pool".to_string());
    app
}

/// The full-screen bleats pane, open on `web`, over a feed of `n` lines
/// numbered `line-0`..`line-{n-1}`, oldest first — enough to exceed any
/// test's body height, for the scrolling and follow tests.
pub fn bleats_pane_with_lines(n: u32) -> App {
    let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    app.update(Msg::Bleats {
        tail: Tail {
            lines: (0..n)
                .map(|i| line(Stream::Out, &format!("line-{i}")))
                .collect(),
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 1_024,
            note: None,
        },
    });
    app.update(Msg::Key(KeyPress::Bleats));
    app
}

/// A feed whose newest lines are short and whose older ones are long, so a
/// page sized from the tail is far too many lines once the view is scrolled
/// back into the long stretch.
///
/// The shape a wrap-aware page step has to survive: `page_amount_up` measures
/// from the tail, and a tail of one-row lines says "a page is N lines" while
/// the older region draws each of those lines as three rows.
#[must_use]
pub fn bleats_pane_with_mixed_line_lengths() -> App {
    let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    // Heights cycling 1, 2, 3, 4 rows rather than a uniform block. Uniform
    // costs make a backward count and a window's own length agree, which is
    // exactly the case that hides a direction-mismatched page size; the gap
    // only appears where consecutive lines wrap to different heights.
    let mut lines: Vec<TailLine> = (0..40)
        .map(|i| {
            let padding = "y".repeat(50 * (i % 4));
            line(Stream::Out, &format!("old-{i} {padding}"))
        })
        .collect();
    lines.extend((0..40).map(|i| line(Stream::Out, &format!("new-{i}"))));
    app.update(Msg::Bleats {
        tail: Tail {
            lines,
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 1_024,
            note: None,
        },
    });
    app.update(Msg::Key(KeyPress::Bleats));
    app
}

/// A feed whose long line is double-width characters, so its wrapped height
/// depends on display columns rather than `char` count.
///
/// Every other wrap fixture here is single-width ASCII, where `char_columns`
/// and a naive per-`char` count agree. That makes them blind to the exact
/// regression this repo has already fixed on two other branches: a
/// full-width character occupies two columns, so 60 of them wrap to twice
/// the rows 60 ASCII characters would.
#[must_use]
pub fn bleats_pane_with_a_wide_line() -> App {
    let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    app.update(Msg::Bleats {
        tail: Tail {
            lines: vec![line(Stream::Out, &"\u{5e83}".repeat(60))],
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 1_024,
            note: None,
        },
    });
    app.update(Msg::Key(KeyPress::Bleats));
    app
}

/// The full-screen bleats pane, open on `web`, over a feed with one line
/// comfortably wider than 80 columns, for the wrap tests.
pub fn bleats_pane_with_long_line() -> App {
    let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    app.update(Msg::Bleats {
        tail: Tail {
            lines: vec![
                line(Stream::Out, "short line"),
                line(Stream::Out, &"x".repeat(200)),
            ],
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 1_024,
            note: None,
        },
    });
    app.update(Msg::Key(KeyPress::Bleats));
    app
}

/// [`super::bleats_full::draw`]'s own lines, for a test that needs the
/// bleats pane's rendered rows without a [`Buffer`] round trip. Thin
/// wrapper: [`super::bleats_full::draw_lines`] is `pub(crate)` for exactly
/// this, but lives in a sibling module the top-level fixture callers in
/// `app.rs` do not otherwise reach.
///
/// [`Buffer`]: ratatui::buffer::Buffer
pub fn draw_lines(app: &App, width: u16, rows: usize) -> Vec<Line<'static>> {
    super::bleats_full::draw_lines(app, width, rows)
}

/// One sheep, `catcher`, selected, with a two-line feed applied and its log
/// paths pointing at real files in a leaked tempdir, so `fs::metadata` in
/// [`super::detail::log_row`] succeeds the way it would against a live
/// sheep's own logs.
///
/// The tempdir is whatever [`tempfile`] resolves for the host: no attempt is
/// made here to force it short. A prior version tried, picking `/tmp` on
/// unix and `RUNNER_TEMP` on Windows, because `log_row`'s own tests once
/// asserted against hardcoded widths (160/70/60) that only produced the
/// intended three-tier behaviour when the path was short. `RUNNER_TEMP` is
/// unset outside GitHub Actions, so a real Windows box fell to the OS
/// default there — a ~35-column prefix under the user profile — and failed
/// the width-160 test for a reason that had nothing to do with the code
/// under test. Those tests now derive their widths from this fixture's own
/// rendered path lengths (see `detail::tests::log_row_thresholds`), so no
/// path-length assumption belongs here any more.
///
/// The tempdir is leaked (`TempDir::keep`) rather than dropped: dropping it
/// would delete the files before the test that calls this reads them, and
/// the OS reclaims a leaked temp directory on its own schedule regardless.
pub fn app_fixture() -> App {
    let dir = tempfile::Builder::new()
        .prefix("shep-fx-")
        .tempdir()
        .expect("a tempdir for the fixture's logs");
    let out_path = dir.path().join("catcher-out.log");
    let err_path = dir.path().join("catcher-err.log");
    std::fs::write(&out_path, b"listening on :8080\n").expect("write the out log");
    std::fs::write(&err_path, b"warn: retrying upstream\n").expect("write the err log");
    let _ = dir.keep();

    let info = ProcessInfo::builder(7, "catcher", ProcStatus::Online)
        .pid(Some(48_107))
        .uptime_ms(4_512_000)
        .out_file(Some(out_path.display().to_string()))
        .err_file(Some(err_path.display().to_string()))
        .build();
    let mut app = with_selection(info);
    app.update(Msg::Bleats {
        tail: Tail {
            lines: vec![
                line(Stream::Out, "listening on :8080"),
                line(Stream::Err, "warn: retrying upstream"),
            ],
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 1_024,
            note: None,
        },
    });
    app
}

/// One tail line, tagged with the stream it came from.
pub fn line(stream: Stream, text: &str) -> TailLine {
    TailLine {
        stream,
        text: text.to_string(),
    }
}

/// One rendered line, styles discarded.
pub fn rendered(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// Several rendered lines, newline-joined. Newline-joined and not
/// concatenated, so an assertion can anchor on a line boundary.
pub fn render_all(lines: &[Line<'static>]) -> String {
    lines.iter().map(rendered).collect::<Vec<_>>().join("\n")
}

/// Four sheep at ids 1..=4 named `web`, `api`, `web-worker`, `cron`, with
/// `query` typed into the filter box and applied. An empty `query` leaves the
/// dashboard unfiltered, which is what the "nothing changed" assertions need.
///
/// Two of the four contain `web`, with `api` between them, so a fixture that
/// stepped over hidden rows would show up as a wrong count rather than as a
/// passing test.
pub fn filtered_app(query: &str) -> App {
    filtered_app_of(named_flock(), query)
}

/// [`filtered_app`] over an explicit flock, for the empty-flock mirror.
pub fn filtered_app_of(flock: Vec<ProcessInfo>, query: &str) -> App {
    let mut app = app_with(flock, plain());
    if !query.is_empty() {
        app.update(Msg::Key(KeyPress::FilterStart));
        for typed in query.chars() {
            app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
    }
    app
}

/// The same four sheep with `query` half-typed and the box still open: no
/// `TextApply`, which is the whole difference from [`filtered_app`].
pub fn editing_app(query: &str) -> App {
    let mut app = app_with(named_flock(), plain());
    app.update(Msg::Key(KeyPress::FilterStart));
    for typed in query.chars() {
        app.update(Msg::Key(KeyPress::TextChar(typed)));
    }
    app
}

/// The four named sheep the filter fixtures share. `flock_of` names its sheep
/// `sheep-0`..`sheep-N`, which every query would match or miss together.
fn named_flock() -> Vec<ProcessInfo> {
    [(1, "web"), (2, "api"), (3, "web-worker"), (4, "cron")]
        .into_iter()
        .map(|(id, name)| {
            ProcessInfo::builder(id, name, ProcStatus::Online)
                .pid(Some(48_000 + id))
                .uptime_ms(4_512_000)
                .build()
        })
        .collect()
}

/// [`filtered_app`]'s four sheep with the gate open and the cursor on `api`
/// at id 2, which is the sheep every action assertion in this file names.
///
/// The cursor is walked to `api` rather than moved a fixed number of rows:
/// the table reads by name, so which row `api` occupies depends on the
/// other sheep's names, not on its id.
pub fn allowed_app() -> App {
    let mut app = app_with(named_flock(), plain());
    app.set_control_for_tests(Control::Allowed);
    for _ in 0..named_flock().len() {
        if app.selected() == Some(RowKey::Sheep(2)) {
            break;
        }
        app.update(Msg::Key(KeyPress::SelectDown));
    }
    assert_eq!(
        app.selected(),
        Some(RowKey::Sheep(2)),
        "the cursor must end up on api"
    );
    app
}

/// [`allowed_app`] with `verb` armed and nothing sent.
pub fn armed_app(verb: ActionVerb) -> App {
    let mut app = allowed_app();
    app.update(Msg::Key(KeyPress::Action(verb)));
    app
}

/// [`armed_app`] confirmed: the request is out and the reply has not landed.
pub fn acting_app(verb: ActionVerb) -> App {
    let mut app = armed_app(verb);
    app.update(Msg::Key(KeyPress::Confirm));
    app
}

/// An armed confirm with a filter applied and a notice standing, so the bar
/// has something in all three slots at once.
///
/// Order matters: the notice must be raised after arming, since `on_key`'s
/// normal branch clears it, and `Msg::Event` never passes through `on_key`.
pub fn armed_app_with_a_filter_and_a_notice() -> App {
    let mut app = filtered_app("api");
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
    app.update(Msg::Event(BusEvent::Dropped { count: 3 }));
    app
}

/// A plausible settings snapshot: every scalar rendered as if `shep.toml`
/// declared it, and two candidate dogs, one enabled, for the tests that
/// need a screen with real rows rather than a fresh home's all-default one.
pub fn settings_snapshot() -> SettingsSnapshot {
    let config = |value: &str| ScalarView {
        value: value.to_string(),
        source: StyleSource::Config,
    };
    SettingsSnapshot {
        log_level: config("warn"),
        log_json: config("false"),
        socket: config("/home/ada/.shep/run/shep.sock"),
        max_cron_sleep: config("30s"),
        allow_control: config("false"),
        style_level: config("full"),
        // The document declares it, so the file and the resolved value
        // agree.
        style_level_in_file: Some("full".to_string()),
        dogs: vec![
            DogView {
                name: "bark".to_string(),
                enabled: false,
                adopted_path: None,
            },
            DogView {
                name: "metrics".to_string(),
                enabled: true,
                adopted_path: None,
            },
        ],
    }
}

/// A dashboard with the settings screen already open on
/// [`settings_snapshot`], the gate closed ([`Control::ReadOnly`]).
pub fn app_in_settings() -> App {
    let mut app = app_with(flock_of(3, 0), plain());
    app.update(Msg::Key(KeyPress::Settings));
    app.update(Msg::Settings {
        result: Ok(settings_snapshot()),
    });
    app
}

/// [`app_in_settings_with_control`] with the cursor already moved onto
/// `field`'s row, by real `SelectDown` keypresses rather than poking the
/// cursor index directly.
pub fn app_in_settings_on(field: SettingField) -> App {
    let mut app = app_in_settings_with_control();
    let target = app
        .settings()
        .unwrap()
        .rows()
        .iter()
        .position(|row| *row == SettingsRow::Scalar(field))
        .expect("field is one of the six scalar rows Settings::rows always carries");
    for _ in 0..target {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
    app
}

/// [`app_in_settings_with_control`], but built from a caller-visible `t0`
/// rather than [`Instant::now`] read inside this function: an expiry test
/// needs to hand `Msg::Tick` an instant it can do arithmetic against.
pub fn app_in_settings_at() -> (App, Instant) {
    let t0 = Instant::now();
    let mut app = App::new(plain(), Control::Allowed, "/home/ada/.shep".to_string(), t0);
    app.update(Msg::Key(KeyPress::Settings));
    app.update(Msg::Settings {
        result: Ok(settings_snapshot()),
    });
    (app, t0)
}

/// The settings screen with `[style] level` SHADOWED: the document says
/// `full`, `source` is the layer that outranked it, and the level in force
/// is `bare`. The cursor sits on the style row, moved there by real
/// keypresses the same way [`app_in_settings_on`] moves it.
///
/// The one state where a scalar's value in force and its value on disk
/// disagree. Every other field's layers belong to the shepherd's process,
/// where lookout can see neither.
pub fn app_in_settings_with_shadowed_style(source: StyleSource) -> App {
    let mut app = app_in_settings_on(SettingField::StyleLevel);
    let mut snapshot = settings_snapshot();
    snapshot.style_level = ScalarView {
        value: "bare".to_string(),
        source,
    };
    snapshot.style_level_in_file = Some("full".to_string());
    app.update(Msg::Settings {
        result: Ok(snapshot),
    });
    app
}

/// [`app_in_settings`] with the control gate open, for the one test that
/// proves an action key stays unreachable even when actions would otherwise
/// be permitted.
pub fn app_in_settings_with_control() -> App {
    let mut app = app_in_settings();
    app.set_control_for_tests(Control::Allowed);
    app
}

/// [`settings_snapshot`]'s own scalars, with `dogs` replaced, for the
/// dogs-table tests, which need particular names, `enabled` bits, and a
/// matching or mismatching flock.
fn settings_snapshot_with_dogs(dogs: Vec<DogView>) -> SettingsSnapshot {
    SettingsSnapshot {
        dogs,
        ..settings_snapshot()
    }
}

/// `otel` runs online while the file disables it: a removed name still
/// running. `ledger` is enabled in the file and absent from the flock: a dog
/// that failed to start. Exercises [`super::settings::dog_rows`]'s join, not
/// the toggle.
pub fn app_in_settings_with_dog_drift() -> App {
    let flock = vec![
        ProcessInfo::builder(90, "otel", ProcStatus::Online)
            .pid(Some(90_000))
            .dog(Some(DogSource::BuiltIn))
            .build(),
    ];
    let mut app = app_with(flock, plain());
    app.update(Msg::Key(KeyPress::Settings));
    app.update(Msg::Settings {
        result: Ok(settings_snapshot_with_dogs(vec![
            // Real paths, not `None`: `otel` and `ledger` are adopted dogs,
            // and every value in `[daemon] adopted_dogs` is a path.
            DogView {
                name: "otel".to_string(),
                enabled: false,
                adopted_path: Some(PathBuf::from("/usr/local/bin/shep-otel")),
            },
            DogView {
                name: "ledger".to_string(),
                enabled: true,
                adopted_path: Some(PathBuf::from("/opt/ledger/bin/dog")),
            },
        ])),
    });
    app
}

/// `bark` is up but has never completed a handshake
/// (`handshook: Some(false)`), so [`super::settings::dog_rows`] must read it
/// `silent`, not `online`, the same correction [`crate::vocabulary::Reported`]
/// makes for the flock table.
pub fn app_in_settings_with_silent_dog() -> App {
    let flock = vec![
        ProcessInfo::builder(91, "bark", ProcStatus::Online)
            .pid(Some(91_000))
            .dog(Some(DogSource::BuiltIn))
            .handshook(Some(false))
            .build(),
    ];
    let mut app = app_with(flock, plain());
    app.update(Msg::Key(KeyPress::Settings));
    app.update(Msg::Settings {
        result: Ok(settings_snapshot_with_dogs(vec![DogView {
            name: "bark".to_string(),
            enabled: true,
            adopted_path: None,
        }])),
    });
    app
}

/// Two candidate dogs for the toggle tests: `metrics` disabled, `otel`
/// enabled, so a test can pick whichever direction it means to arm.
fn settings_snapshot_for_toggle_tests() -> SettingsSnapshot {
    settings_snapshot_with_dogs(vec![
        DogView {
            name: "metrics".to_string(),
            enabled: false,
            adopted_path: None,
        },
        DogView {
            name: "otel".to_string(),
            enabled: true,
            adopted_path: Some(PathBuf::from("/usr/local/bin/shep-otel")),
        },
    ])
}

/// A dashboard with the settings screen open on
/// [`settings_snapshot_for_toggle_tests`], the control gate open, and the
/// cursor moved onto `name`'s dog row by real `SelectDown` keypresses.
///
/// The six scalar rows always sort first in `Settings::rows`, so the dog at
/// index `i` of [`settings_snapshot_for_toggle_tests`]'s own list sits at
/// row `6 + i`.
pub fn app_in_settings_on_dog(name: &str) -> App {
    let mut app = app_with(flock_of(3, 0), plain());
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Settings));
    app.update(Msg::Settings {
        result: Ok(settings_snapshot_for_toggle_tests()),
    });
    let dog_index = settings_snapshot_for_toggle_tests()
        .dogs
        .iter()
        .position(|dog| dog.name == name)
        .expect("name is one of settings_snapshot_for_toggle_tests's own dogs");
    for _ in 0..(6 + dog_index) {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
    app
}

/// [`app_in_settings_on_dog`], named for the test that means to start on an
/// already-enabled dog: same fixture, same mechanism, a name that reads
/// what the test is asserting on without checking the dogs list.
pub fn app_in_settings_on_enabled_dog(name: &str) -> App {
    app_in_settings_on_dog(name)
}

/// One sheep's config as the shepherd would answer it: `web`, with two
/// fields an operator has overridden, one parked until a respawn, and two
/// env keys whose values the view never carries.
pub fn sheep_config_view() -> SheepConfigView {
    sheep_config_view_parking(vec!["kill_signal".to_string()])
}

/// [`sheep_config_view`] with `pending` in place of the one field it parks.
fn sheep_config_view_parking(pending: Vec<String>) -> SheepConfigView {
    let mut config = AppConfig {
        name: "web".to_string(),
        script: "./srv".to_string(),
        args: vec!["--port".to_string(), "8080".to_string()],
        max_restarts: 32,
        instances: 3,
        ..AppConfig::default()
    };
    config
        .env
        .insert("DB_HOST".to_string(), "db.internal".to_string());
    config
        .env
        .insert("LOG_LEVEL".to_string(), "debug".to_string());
    SheepConfigView::new(
        config,
        vec!["max_restarts".to_string(), "reuse_port".to_string()],
        pending,
    )
}

/// A [`ConfigPane`] over `web`, with `kill_timeout` and `graceful_timeout`
/// set to round numbers a close dialog's own copy names literally, `5s`
/// and `10s`: the sheep's own values a test can assert on verbatim, rather
/// than a millisecond count `resolved_display` would leave bare.
fn close_dialog_pane(
    wait_ready: bool,
    has_probe: bool,
    reuse_port: bool,
    instances: u32,
) -> ConfigPane {
    let config = AppConfig {
        name: "web".to_string(),
        kill_timeout: UpDuration::from_millis(5_000),
        graceful_timeout: UpDuration::from_millis(10_000),
        wait_ready,
        reuse_port,
        instances,
        readiness_probe: has_probe.then(|| ProbeConfig {
            kind: ProbeKind::Tcp,
            target: "127.0.0.1:8080".into(),
            interval: UpDuration::from_millis(10_000),
            timeout: UpDuration::from_millis(5_000),
            failure_threshold: 3,
        }),
        ..AppConfig::default()
    };
    ConfigPane::sheep(SheepConfigView::new(config, Vec::new(), Vec::new()))
}

/// The pid every hand-built close dialog names, so a test reading the
/// heading's right clause has one number to match rather than whichever
/// the flock fixture handed out.
const DIALOG_PID: u32 = 71_578;

/// A close dialog naming `unsent` filed edits and `parked` shepherd
/// fields, over a plain overlapping-reload sheep: what
/// [`close_dialog_lines`](crate::lookout::view::pane::close_dialog_lines)'s
/// own heading and naming-sentence tests read, without driving a real key
/// sequence to raise one.
pub fn close_dialog_with(unsent: usize, parked: usize) -> CloseDialog {
    let pane = close_dialog_pane(true, false, false, 1);
    let unsent_fields = (0..unsent).map(|i| format!("field{i}")).collect();
    CloseDialog::new(
        unsent_fields,
        parked,
        &pane,
        ProcStatus::Online,
        Some(DIALOG_PID),
        Instant::now(),
    )
}

/// The same dialog [`close_dialog_with`] builds, over a sheep the
/// shepherd runs several of: no one pid to name, so the heading's right
/// clause carries none.
pub fn close_dialog_without_a_pid() -> CloseDialog {
    let pane = close_dialog_pane(true, false, false, 2);
    CloseDialog::new(
        vec!["cwd".to_string()],
        0,
        &pane,
        ProcStatus::Online,
        None,
        Instant::now(),
    )
}

/// A close dialog over a sheep whose reload takes `kind` and reaches
/// `instances` of it: what the reload row's own tests read. One unsent
/// field and nothing parked, since the reload row draws the same either
/// way and a test on it should not have to explain the heading too.
pub fn close_dialog_reloading(kind: ReloadKind, instances: u32) -> CloseDialog {
    let (wait_ready, has_probe, reuse_port) = match kind {
        // `reload_mode`'s own rule: `!wait_ready && has_probe && !reuse_port`
        // is `Serial`, anything else is `Overlap`.
        ReloadKind::Overlap => (true, false, false),
        ReloadKind::Serial => (false, true, false),
    };
    let pane = close_dialog_pane(wait_ready, has_probe, reuse_port, instances);
    CloseDialog::new(
        vec!["cwd".to_string()],
        0,
        &pane,
        ProcStatus::Online,
        Some(DIALOG_PID),
        Instant::now(),
    )
}

/// A close dialog raised over a pane with `cwd` really filed (it needs a
/// respawn), and, when `with_live` is set, `max_restarts` filed alongside
/// it (`ApplyGroup::Live`, so the running sheep already takes it): what
/// the "everything else you changed is already live" sentence's own tests
/// read.
///
/// Driven through [`file_edit`] rather than handed synthetic names, unlike
/// [`close_dialog_with`]: `CloseDialog::live` is not a parameter, it is
/// read off the pane's own filed set, so the set has to be real for it to
/// answer anything.
pub fn close_dialog_with_live_edit(with_live: bool) -> CloseDialog {
    let mut app = app_in_sheep_pane_with_nothing_parked();
    file_edit(&mut app, "cwd", "/srv/app");
    if with_live {
        file_edit(&mut app, "max_restarts", "9");
    }
    let pane = app.config_pane().expect("the pane is open");
    CloseDialog::new(
        pane.unsent_fields_needing_a_respawn(),
        pane.parked_count(),
        pane,
        ProcStatus::Online,
        Some(DIALOG_PID),
        Instant::now(),
    )
}

/// The one line in `lines` starting with `prefix`, after trimming leading
/// whitespace: what a close dialog's own option-row tests read, so a test
/// for the reload row does not pass off the first row that merely
/// contains the letter somewhere in its sentence.
///
/// # Panics
///
/// Panics if no line starts with `prefix`, which is a fixture bug rather
/// than a failure the test is about.
#[track_caller]
pub fn row_starting_with(lines: &[Line<'static>], prefix: &str) -> String {
    lines
        .iter()
        .map(rendered)
        .find(|line| line.trim_start().starts_with(prefix))
        .unwrap_or_else(|| panic!("no row starts with {prefix:?}"))
}

/// The sheep pane, `cwd` filed and the close dialog raised the way `esc`
/// raises it for real (`App::close_offer`, rather than a synthetic
/// `CloseDialog::new`): what this task's own box, borderless and mute-pass
/// tests draw a frame from.
fn app_with_close_dialog_and_palette(palette: Palette) -> App {
    let mut app = with_selection_and_palette(
        ProcessInfo::builder(9, "web", ProcStatus::Online)
            .pid(Some(48_000))
            .build(),
        palette,
    );
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(sheep_config_view_parking(
            Vec::new(),
        )))),
    });
    file_edit(&mut app, "cwd", "/srv/app");
    app.update(Msg::Key(KeyPress::Escape));
    assert!(
        app.close_dialog().is_some(),
        "close_offer refused to raise a dialog"
    );
    app
}

/// The same, at [`plain`].
pub fn app_with_close_dialog() -> App {
    app_with_close_dialog_and_palette(plain())
}

/// A frame with the close dialog open, drawn at `width` x `height`: what
/// the box and borderless width tests read the dialog's own margin
/// arithmetic against.
pub fn render_dialog(width: u16, height: u16) -> Buffer {
    render(&app_with_close_dialog(), width, height)
}

/// The row in `buffer` containing `needle`.
///
/// # Panics
///
/// Panics if no row contains `needle`, a fixture bug rather than a failure
/// the test is about.
#[track_caller]
pub fn row_containing(buffer: &Buffer, needle: &str) -> String {
    rows_of(buffer)
        .into_iter()
        .find(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("no row contains {needle:?}"))
}

/// [`super::pane::draw_pane`] alone, straight into a fresh buffer at
/// `width` x `height`, with the close dialog raised: what the mute-pass
/// test reads a cell from, since [`render`] draws the whole frame and the
/// config pane does not start at the buffer's own origin there.
pub fn draw_pane_with_dialog(width: u16, height: u16) -> Buffer {
    draw_pane_with_dialog_and_palette(width, height, plain())
}

/// The same, at `palette`: what the `NO_COLOR` mute-pass test reads.
pub fn draw_pane_with_dialog_and_palette(width: u16, height: u16, palette: Palette) -> Buffer {
    let app = app_with_close_dialog_and_palette(palette);
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    let pane = app.config_pane().expect("the pane is open");
    super::pane::draw_pane(&app, pane, area, &mut buffer);
    buffer
}

/// The palette `NO_COLOR` selects: no ink anywhere, so the mute pass's own
/// second call (`palette.muted()`) is a no-op.
pub fn no_color() -> Palette {
    Palette::detect(Some(OsStr::new("1")), None, None)
}

/// The style [`plain`]'s ink leaves a cell in once the mute pass has run:
/// the pane's own reset-then-muted sequence, replayed here so a fixture
/// never has to agree with a colour literal in `theme.rs` by coincidence.
pub fn plain_dimmed() -> Style {
    Style::reset().patch(plain().muted())
}

/// The bark dog's `[bark]` section as `Request::DogConfig` would answer it:
/// a comment, two scalars, and a sink holding a webhook credential.
///
/// The comment is load-bearing rather than decoration: a write goes out as
/// the WHOLE section, so a pane that re-rendered it from the parsed values
/// would delete this line on the operator's own keystroke.
pub fn dog_section() -> String {
    "# how often\npoll = \"60s\"\nhistory_bytes = 4096\n\n[sinks.ops]\nkind = \"slack\"\nurl = \"https://hooks.example/x\"\n"
        .to_string()
}

/// A dashboard with the bark dog's config pane open, opened the way the
/// event loop opens it: `e` on the settings screen's own dog row, then the
/// schema its binary answered with, then the shepherd's section. The
/// control gate is open, so the pane can write.
///
/// bark is [`settings_snapshot`]'s first dog, and the six scalar rows
/// always sort ahead of the dogs, so row 6 is its row.
pub fn app_in_dog_pane() -> App {
    let mut app = app_with(flock_of(3, 0), plain());
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Settings));
    app.update(Msg::Settings {
        result: Ok(settings_snapshot()),
    });
    for _ in 0..6 {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
    app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::DogPane {
        name: "bark".to_string(),
        adopted_path: None,
        result: Ok(crate::dog::builtin_schema("bark").expect("bark is a built-in")),
    });
    app.update(Msg::Replied {
        sent: Sent::DogSection {
            name: "bark".to_string(),
        },
        result: Ok(Response::DogSection {
            toml: dog_section().into(),
        }),
    });
    app
}

/// [`app_in_dog_pane`] with two edits filed, driven by real key presses:
/// `poll` typed, then `history_bytes` typed.
///
/// Two, and not one, because a batch of one cannot tell a loop from a
/// `take(1)`. What `closing_a_dog_pane_sends_one_write_for_two_edits`
/// needs: proof that a dog's batch is one `Sent::SetDogSection`, not two.
pub fn app_in_dog_pane_with_two_edits() -> App {
    let mut app = app_in_dog_pane();
    for (key, typed) in [("poll", "45s"), ("history_bytes", "8192")] {
        select_field(&mut app, key);
        app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..64 {
            app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for character in typed.chars() {
            app.update(Msg::Key(KeyPress::TextChar(character)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
    }
    assert_eq!(
        app.config_pane().expect("the pane is open").edits().len(),
        2,
        "the fixture files two edits"
    );
    app
}

/// A sheep pane, control open, with exactly the env keys named. Values are
/// what the fixture's own caller reads to know what it asked for; no value
/// for any key ever reaches the pane itself, since `SheepConfigView::new`
/// strips them before the struct is built.
pub fn app_in_sheep_pane_with_env(env: &[(&str, &str)]) -> App {
    let mut app = with_selection(
        ProcessInfo::builder(9, "web", ProcStatus::Online)
            .pid(Some(48_000))
            .build(),
    );
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Edit));
    let mut config = AppConfig {
        name: "web".to_string(),
        ..AppConfig::default()
    };
    for (key, value) in env {
        config.env.insert((*key).to_string(), (*value).to_string());
    }
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(SheepConfigView::new(
            config,
            Vec::new(),
            Vec::new(),
        )))),
    });
    app
}

/// Walks an open config pane's cursor onto the env row named `key`, the
/// way [`select_field`] walks it onto a field: no fixture reaches into the
/// pane to place it.
///
/// # Panics
///
/// Panics if the pane is closed or has no env key by that name, which is a
/// fixture bug rather than a failure the test is about.
#[track_caller]
pub fn select_env_key(app: &mut App, key: &str) {
    let pane = app.config_pane().expect("the pane is open");
    let index = pane
        .rows()
        .iter()
        .position(|row| match row {
            crate::lookout::pane::PaneRow::Env(env_index) => {
                pane.env_key_names().get(*env_index).map(String::as_str) == Some(key)
            }
            crate::lookout::pane::PaneRow::Field(_) | crate::lookout::pane::PaneRow::AddEnv => {
                false
            }
        })
        .unwrap_or_else(|| panic!("no env key named {key}"));
    app.update(Msg::Key(KeyPress::SelectFirst));
    for _ in 0..index {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
}

/// Opens the editor on whatever row the cursor is already on, types
/// `text`, and applies it: `Confirm` then a character at a time then
/// `TextApply`, the way an operator drives either editor this pane opens.
pub fn type_into_the_open_editor(app: &mut App, text: &str) {
    app.update(Msg::Key(KeyPress::Confirm));
    for character in text.chars() {
        app.update(Msg::Key(KeyPress::TextChar(character)));
    }
    app.update(Msg::Key(KeyPress::TextApply));
}

/// [`app_in_sheep_pane`] with the control gate open: the pane can write.
///
/// The gate is set BEFORE the pane opens, so nothing about how it opened
/// depends on it, which is what makes a read-only refusal and a permitted
/// write comparable frames.
pub fn app_in_sheep_pane_with_control() -> App {
    let mut app = with_selection(
        ProcessInfo::builder(9, "web", ProcStatus::Online)
            .pid(Some(48_000))
            .build(),
    );
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(sheep_config_view()))),
    });
    app
}

/// [`app_in_sheep_pane_with_control`], named for the tests about the apply
/// menu: [`sheep_config_view`] parks `kill_signal` and nothing else.
pub fn app_in_sheep_pane_with_a_parked_field() -> App {
    app_in_sheep_pane_with_control()
}

/// The same pane over a sheep whose config parks two fields.
pub fn app_in_sheep_pane_with_two_parked_fields() -> App {
    app_in_sheep_pane_parking(vec!["kill_signal".to_string(), "script".to_string()])
}

/// The same pane over a sheep the running process is fully caught up with.
pub fn app_in_sheep_pane_with_nothing_parked() -> App {
    app_in_sheep_pane_parking(Vec::new())
}

/// [`app_in_sheep_pane_with_control`] over a view parking `pending`.
fn app_in_sheep_pane_parking(pending: Vec<String>) -> App {
    let mut app = with_selection(
        ProcessInfo::builder(9, "web", ProcStatus::Online)
            .pid(Some(48_000))
            .build(),
    );
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(sheep_config_view_parking(
            pending,
        )))),
    });
    app
}

/// A dashboard with `web` selected and its config pane open, opened the way
/// the event loop opens it: `e`, then the shepherd's own reply.
///
/// Read-only by default, since nothing here calls
/// `set_control_for_tests(Control::Allowed)`: the tests this fixture backs
/// are about reading and about the closed gate. [`file_edit`] opens the
/// gate itself for the tests that need to file an edit here.
pub fn app_in_sheep_pane() -> App {
    let mut app = with_selection(
        ProcessInfo::builder(9, "web", ProcStatus::Online)
            .pid(Some(48_000))
            .build(),
    );
    app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(sheep_config_view()))),
    });
    app
}

/// [`app_in_sheep_pane`], explicit about the gate rather than reading it
/// off that fixture's own default: a read-only pane is the one fact a test
/// on this cares about, and this name says so without depending on
/// `app_in_sheep_pane`'s default staying what it is today.
pub fn app_in_sheep_pane_read_only() -> App {
    let mut app = app_in_sheep_pane();
    app.set_control_for_tests(Control::ReadOnly);
    app
}

/// [`app_in_sheep_pane`], over a sheep the shepherd reports stopped: nothing
/// holds the old config, so the close dialog's `R`/`L` half has nothing to
/// offer and `esc` never asks about a respawn. Nothing parked either, since
/// a stopped sheep has no running process for the shepherd to have parked
/// a write against.
pub fn app_in_sheep_pane_on_a_stopped_sheep() -> App {
    let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Stopped).build());
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(sheep_config_view_parking(
            Vec::new(),
        )))),
    });
    app
}

/// [`app_in_sheep_pane`], over a sheep the shepherd reports `Stopping`: its
/// drainee is going away and is not a restart target
/// ([`ProcStatus::Stopping`]'s own doc), so this is the other half of
/// "not running" `App::sheep_is_running` excludes, alongside `Stopped`.
pub fn app_in_sheep_pane_on_a_draining_sheep() -> App {
    let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Stopping).build());
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(sheep_config_view_parking(
            Vec::new(),
        )))),
    });
    app
}

/// Files an edit for `key` through the real keys an operator would press:
/// select it, open the editor, replace the buffer with `value`, apply.
///
/// Grants control first: filing an edit is not what a read-only test is
/// about, and every caller of this fixture wants the edit to land.
pub fn file_edit(app: &mut App, key: &str, value: &str) {
    app.set_control_for_tests(Control::Allowed);
    select_field(app, key);
    app.update(Msg::Key(KeyPress::Confirm));
    for _ in 0..64 {
        app.update(Msg::Key(KeyPress::TextBackspace));
    }
    for character in value.chars() {
        app.update(Msg::Key(KeyPress::TextChar(character)));
    }
    app.update(Msg::Key(KeyPress::TextApply));
}

/// A frame already drawn, at `width` x `height`: every secrets-pane test
/// reads cells straight off this rather than the `String` `render_text`
/// gives, since a column offset only means something against the buffer it
/// came from.
pub fn render(app: &App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| super::draw(app, frame)).unwrap();
    terminal.backend().buffer().clone()
}

/// `buffer` as one `String` per row, for a `contains` search across whatever
/// line carries the text (a group header, say) rather than one column.
pub fn rows_of(buffer: &Buffer) -> Vec<String> {
    crate::lookout::frames::render_text(buffer)
        .lines()
        .map(str::to_string)
        .collect()
}

/// The secrets pane, opened and loaded: `DB_PASSWORD` set for `production`
/// only, `ELSEWHERE_ONLY` set for `ci` only, `SET_EVERYWHERE` set for `all`,
/// `SET_IN_ALL_THREE` set for every environment slot including `all`, the
/// tab on `production` (the first load's own default, since `environment`
/// below is what it asks for).
pub fn app_with_secrets() -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(SecretsModel {
            environments: vec![
                "all".to_string(),
                "ci".to_string(),
                "production".to_string(),
            ],
            rows: vec![
                SecretRow {
                    key: "DB_PASSWORD".to_string(),
                    source: Source::Operator,
                    in_force: Some("production".to_string()),
                    set_in: vec!["production".to_string()],
                    byte_len: Some(9),
                    readers: Vec::new(),
                },
                SecretRow {
                    key: "ELSEWHERE_ONLY".to_string(),
                    source: Source::Operator,
                    in_force: None,
                    set_in: vec!["ci".to_string()],
                    byte_len: None,
                    readers: Vec::new(),
                },
                SecretRow {
                    key: "SET_EVERYWHERE".to_string(),
                    source: Source::Operator,
                    in_force: Some("all".to_string()),
                    set_in: vec!["all".to_string()],
                    byte_len: Some(4),
                    readers: Vec::new(),
                },
                SecretRow {
                    key: "SET_IN_ALL_THREE".to_string(),
                    source: Source::Operator,
                    in_force: Some("all".to_string()),
                    set_in: vec![
                        "all".to_string(),
                        "ci".to_string(),
                        "production".to_string(),
                    ],
                    byte_len: Some(4),
                    readers: Vec::new(),
                },
            ],
            ..SecretsModel::default()
        })),
    });
    app
}

/// The secrets pane with one row whose value is exactly `MAX_VALUE_BYTES`
/// long, for the column-overflow test.
pub fn app_with_a_maximum_length_secret() -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(SecretsModel {
            environments: vec!["production".to_string()],
            rows: vec![SecretRow {
                key: "HUGE".to_string(),
                source: Source::Operator,
                in_force: Some("production".to_string()),
                set_in: vec!["production".to_string()],
                byte_len: Some(shep_core::secrets::MAX_VALUE_BYTES),
                readers: Vec::new(),
            }],
            ..SecretsModel::default()
        })),
    });
    app
}

/// The secrets pane with one row from a provider's own cache, for the
/// read-only-group test.
pub fn app_with_a_pushed_secret() -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(SecretsModel {
            environments: vec!["production".to_string()],
            rows: vec![SecretRow {
                key: "vercel/API_TOKEN".to_string(),
                source: Source::Namespace("vercel".to_string()),
                in_force: Some("production".to_string()),
                set_in: vec!["production".to_string()],
                byte_len: Some(6),
                readers: Vec::new(),
            }],
            ..SecretsModel::default()
        })),
    });
    app
}

/// One operator row and one provider row, for the `+ new key` affordance's
/// own position test: it has to draw between the two groups, not just at
/// either end the way [`app_with_secrets`] (no namespace) or
/// [`app_with_a_pushed_secret`] (no operator row) alone would show.
pub fn app_with_secrets_and_a_provider_row() -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(SecretsModel {
            environments: vec!["production".to_string()],
            rows: vec![
                SecretRow {
                    key: "DB_PASSWORD".to_string(),
                    source: Source::Operator,
                    in_force: Some("production".to_string()),
                    set_in: vec!["production".to_string()],
                    byte_len: Some(9),
                    readers: Vec::new(),
                },
                SecretRow {
                    key: "vercel/API_TOKEN".to_string(),
                    source: Source::Namespace("vercel".to_string()),
                    in_force: Some("production".to_string()),
                    set_in: vec!["production".to_string()],
                    byte_len: Some(6),
                    readers: Vec::new(),
                },
            ],
            ..SecretsModel::default()
        })),
    });
    app
}

/// Operator, namespace, operator, in that order: `SecretsModel::rows` is
/// contiguous by source everywhere else in this pane, so this is the one
/// fixture that is not, for the test pinning `SecretsPane::new_key_anchor`
/// as the single computation both the cursor and the renderer read.
pub fn app_with_interleaved_secret_sources() -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(SecretsModel {
            environments: vec!["production".to_string()],
            rows: vec![
                SecretRow {
                    key: "FIRST_OPERATOR_KEY".to_string(),
                    source: Source::Operator,
                    in_force: Some("production".to_string()),
                    set_in: vec!["production".to_string()],
                    byte_len: Some(1),
                    readers: Vec::new(),
                },
                SecretRow {
                    key: "vercel/API_TOKEN".to_string(),
                    source: Source::Namespace("vercel".to_string()),
                    in_force: Some("production".to_string()),
                    set_in: vec!["production".to_string()],
                    byte_len: Some(6),
                    readers: Vec::new(),
                },
                SecretRow {
                    key: "SECOND_OPERATOR_KEY".to_string(),
                    source: Source::Operator,
                    in_force: Some("production".to_string()),
                    set_in: vec!["production".to_string()],
                    byte_len: Some(2),
                    readers: Vec::new(),
                },
            ],
            ..SecretsModel::default()
        })),
    });
    app
}

/// The value [`app_with_secrets_and_reads`] stores, and so the exact text a
/// reveal has to put on screen.
pub const REVEALED_VALUE: &str = "hunter2-not-really";

/// The secrets pane over a real store under `home`, `DB_PASSWORD` selected
/// and the reveal gate set by `allow_read`.
///
/// A file on disk rather than a hand-built model, because a reveal reads
/// its one value back out of the store: a fixture that only listed rows
/// could not exercise one. The caller owns `home` and has to keep it alive
/// for as long as the app.
pub fn app_with_secrets_and_reads(home: &Path, allow_read: bool) -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(secrets_model(home, allow_read))),
    });
    app
}

/// The model [`app_with_secrets_and_reads`] loads, and the store on disk it
/// reads from, so a test can load it a second time with the gate moved.
pub fn secrets_model(home: &Path, allow_read: bool) -> SecretsModel {
    let store = home.join("secrets.json");
    shep_core::secrets::set(&store, "DB_PASSWORD", "production", REVEALED_VALUE).unwrap();
    SecretsModel {
        environments: vec!["all".to_string(), "production".to_string()],
        rows: vec![
            SecretRow {
                key: "DB_PASSWORD".to_string(),
                source: Source::Operator,
                in_force: Some("production".to_string()),
                set_in: vec!["production".to_string()],
                byte_len: Some(REVEALED_VALUE.len()),
                readers: Vec::new(),
            },
            // Never selected, so a test can hold one row against the other
            // and see that a reveal reaches exactly one of them.
            SecretRow {
                key: "OTHER_KEY".to_string(),
                source: Source::Operator,
                in_force: Some("production".to_string()),
                set_in: vec!["production".to_string()],
                byte_len: Some(3),
                readers: Vec::new(),
            },
        ],
        allow_read,
        store,
        ..SecretsModel::default()
    }
}

/// One operator row, `DB_PASSWORD` set for `all`, over three environment
/// tabs (`all` first, so `TabNext` actually moves): the shared model behind
/// every set-a-value fixture below.
fn secrets_write_model() -> SecretsModel {
    SecretsModel {
        environments: vec![
            "all".to_string(),
            "ci".to_string(),
            "production".to_string(),
        ],
        rows: vec![SecretRow {
            key: "DB_PASSWORD".to_string(),
            source: Source::Operator,
            in_force: Some("all".to_string()),
            set_in: vec!["all".to_string()],
            byte_len: Some(9),
            readers: Vec::new(),
        }],
        ..SecretsModel::default()
    }
}

/// The secrets pane, opened and loaded on [`secrets_write_model`], read-only
/// (the default [`Control`]).
pub fn app_with_secrets_read_only() -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "all".to_string(),
        result: Ok(Box::new(secrets_write_model())),
    });
    app
}

/// [`app_with_secrets_read_only`] with the control gate open, for every
/// test that means to write.
pub fn app_with_secrets_and_control() -> App {
    let mut app = app_with_secrets_read_only();
    app.set_control_for_tests(Control::Allowed);
    app
}

/// [`app_with_secrets`] with the control gate open: a named tab
/// (`production`) holding rows whose value comes from the `all` slot, which
/// is the shape a delete has to refuse rather than retarget.
pub fn app_with_secrets_on_a_named_tab_and_control() -> App {
    let mut app = app_with_secrets();
    app.set_control_for_tests(Control::Allowed);
    app
}

/// [`app_with_secrets_and_control`] with `DB_PASSWORD`'s value input open:
/// `Enter` on the operator's only row, already selected by default.
pub fn app_typing_a_value() -> App {
    let mut app = app_with_secrets_and_control();
    app.update(Msg::Key(KeyPress::Confirm));
    app
}

/// [`app_with_secrets_and_control`] with `DB_PASSWORD`'s delete armed:
/// `D` on the operator's only row, already selected by default.
pub fn app_armed_to_delete_a_secret() -> App {
    let mut app = app_with_secrets_and_control();
    app.update(Msg::Key(KeyPress::SecretDelete));
    app
}

/// [`app_with_secrets_and_control`] with the `+ new key` row's name input
/// open: `G` lands on the trailing `+ new key` row, then `Enter` opens it.
pub fn app_typing_a_new_key() -> App {
    let mut app = app_with_secrets_and_control();
    app.update(Msg::Key(KeyPress::SelectLast));
    app.update(Msg::Key(KeyPress::Confirm));
    app
}

/// [`app_with_a_pushed_secret`] with the control gate open, for the test
/// proving a provider row refuses a write anyway.
pub fn app_with_a_pushed_secret_selected_and_control() -> App {
    let mut app = app_with_a_pushed_secret();
    app.set_control_for_tests(Control::Allowed);
    app
}

/// [`app_revealing`] with the control gate open, for the test proving a
/// successful write clears a reveal.
pub fn app_revealing_with_control() -> App {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_revealing(dir.path());
    app.set_control_for_tests(Control::Allowed);
    // Leaked deliberately: the fixture owns no `home` the caller can keep
    // alive, and the pane never touches the directory again once revealed.
    let _ = dir.keep();
    app
}

/// Presses `v` and does the read its effect asks for, handing back the
/// answer rather than applying it, so a test can move the pane underneath a
/// reveal that is still in flight.
///
/// # Panics
///
/// If `v` raised no read, since the caller is about to answer one.
#[track_caller]
pub fn ask_to_reveal(app: &mut App) -> Msg {
    let Effect::RevealSecret {
        store,
        provider_cache,
        row,
        environment,
    } = app.update(Msg::Key(KeyPress::Reveal))
    else {
        panic!("`v` over an open gate reads the store");
    };
    Msg::Revealed {
        key: row.key.clone(),
        environment,
        value: crate::lookout::secrets::stored_value(&store, &provider_cache, &row)
            .map(RevealedValue),
    }
}

/// The same pane with `DB_PASSWORD` already on screen, revealed the way an
/// operator reveals it: the keypress, the read it asks for, and the answer.
///
/// # Panics
///
/// If the reveal did not land, so a test asserting that some trigger clears
/// one cannot pass against an app that never revealed anything.
#[track_caller]
pub fn app_revealing(home: &Path) -> App {
    let mut app = app_with_secrets_and_reads(home, true);
    let answer = ask_to_reveal(&mut app);
    app.update(answer);
    assert!(
        matches!(app.body(), Body::Secrets(pane) if pane.reveal.is_some()),
        "the fixture starts with a value on screen"
    );
    app
}

/// A single operator row, `DB_PASSWORD`, selected by default, named by two
/// sheep: `catcher` online, `web` not. For the WHO READS IT panel's own
/// caption test.
fn app_with_one_row_and_readers(readers: Vec<Reader>) -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(SecretsModel {
            environments: vec!["production".to_string()],
            rows: vec![SecretRow {
                key: "DB_PASSWORD".to_string(),
                source: Source::Operator,
                in_force: Some("production".to_string()),
                set_in: vec!["production".to_string()],
                byte_len: Some(9),
                readers,
            }],
            // Readers and the age both come off the muster roll, so a
            // fixture with one and not the other is a state the loader
            // cannot reach.
            roll_age: Some(Duration::from_secs(184)),
            ..SecretsModel::default()
        })),
    });
    app
}

/// The secrets pane rendered with `DB_PASSWORD` named by one online and one
/// offline reader.
pub fn render_secrets_with_readers() -> Buffer {
    let app = app_with_one_row_and_readers(vec![
        Reader {
            name: "catcher".to_string(),
            environment: "production".to_string(),
            online: true,
        },
        Reader {
            name: "web".to_string(),
            environment: "production".to_string(),
            online: false,
        },
    ]);
    render(&app, 160, 48)
}

/// The secrets pane rendered with `DB_PASSWORD` named by five readers, more
/// than WHO READS IT has rows for. For the overflow line's own test.
pub fn render_secrets_with_more_readers_than_fit() -> Buffer {
    let app = app_with_one_row_and_readers(
        ["catcher", "web", "worker", "api", "scheduler"]
            .into_iter()
            .map(|name| Reader {
                name: name.to_string(),
                environment: "production".to_string(),
                online: true,
            })
            .collect(),
    );
    render(&app, 160, 48)
}

/// The secrets pane rendered with `[secrets] allow_read` off, the default
/// [`SecretsModel::allow_read`].
pub fn render_secrets_gate_shut() -> Buffer {
    render(&app_with_secrets(), 160, 48)
}

/// The secrets pane rendered with the muster roll `age` old.
pub fn render_secrets_with_roll_age(age: Duration) -> Buffer {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(SecretsModel {
            environments: vec!["production".to_string()],
            rows: vec![SecretRow {
                key: "DB_PASSWORD".to_string(),
                source: Source::Operator,
                in_force: Some("production".to_string()),
                set_in: vec!["production".to_string()],
                byte_len: Some(9),
                readers: Vec::new(),
            }],
            roll_age: Some(age),
            ..SecretsModel::default()
        })),
    });
    render(&app, 160, 48)
}

/// The secrets pane rendered with no muster roll at all, [`SecretsModel::roll_age`]
/// still `None`: the state the roll status line has to tell apart from a key
/// nothing reads.
///
/// The same screen [`render_secrets_gate_shut`] draws, since
/// `SecretsModel::default` shuts the gate and carries no roll at once. One
/// expression, two names, each saying which half its own test is about.
pub fn render_secrets_with_no_roll() -> Buffer {
    render_secrets_gate_shut()
}

/// Walks an open config pane's cursor onto `key`, the way an operator
/// walks it: `tab`/a digit onto `key`'s own group first, when it carries
/// one, then down the filtered list onto the row itself. No fixture
/// reaches into the pane to place it.
///
/// Panics if the pane is closed or has no field by that name, which is a
/// fixture bug rather than a failure the test is about.
pub fn select_field(app: &mut App, key: &str) {
    let pane = app.config_pane().expect("the pane is open");
    let group = pane
        .fields()
        .by_key(key)
        .unwrap_or_else(|| panic!("no field named {key}"))
        .group
        .clone();
    if let Some(group) = group
        && let Some(position) = shep_core::config::GROUP_ORDER
            .iter()
            .position(|known| *known == group)
    {
        let digit = u8::try_from(position + 1).expect("eight groups fit a u8");
        app.update(Msg::Key(KeyPress::Group(digit)));
    }
    let pane = app.config_pane().expect("the pane is open");
    let index = pane
        .rows()
        .iter()
        .position(|row| match row {
            crate::lookout::pane::PaneRow::Field(field_index) => {
                pane.fields().fields()[*field_index].key == key
            }
            crate::lookout::pane::PaneRow::Env(_) | crate::lookout::pane::PaneRow::AddEnv => false,
        })
        .unwrap_or_else(|| panic!("{key} is not in the active group's rows"));
    app.update(Msg::Key(KeyPress::SelectFirst));
    for _ in 0..index {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
}

/// [`app_in_sheep_pane_with_nothing_parked`] with two edits filed, driven
/// by real key presses: `cwd` typed, and `max_memory` typed.
///
/// Two, and not one, because a batch of one cannot tell a loop from a
/// `take(1)`. Two different fields rather than two shapes of edit, since
/// the set is keyed by field and a second edit to one key replaces it.
/// The two also carry different groups on purpose (`cwd` is `process`,
/// `max_memory` is `restart`), which is what the pending-edits section's
/// own tests need: a group's own field list only ever shows one group at
/// a time, so a test that an edit from elsewhere still turns up has to
/// file one there.
///
/// Nothing parked, so `Escape` writes and leaves rather than stopping to
/// offer the apply menu.
pub fn app_in_sheep_pane_with_two_edits() -> App {
    let mut app = app_in_sheep_pane_with_nothing_parked();
    for (key, typed) in [("cwd", "/srv/web"), ("max_memory", "40")] {
        select_field(&mut app, key);
        app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..64 {
            app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for character in typed.chars() {
            app.update(Msg::Key(KeyPress::TextChar(character)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
    }
    assert_eq!(
        app.config_pane().expect("the pane is open").edits().len(),
        2,
        "the fixture files two edits"
    );
    app
}

/// [`app_in_sheep_pane_with_nothing_parked`] with `cwd` alone filed: one
/// write, so a whole-batch refusal has exactly one ticket to refuse.
pub fn app_in_sheep_pane_with_one_edit() -> App {
    let mut app = app_in_sheep_pane_with_nothing_parked();
    file_edit(&mut app, "cwd", "/srv/web");
    assert_eq!(
        app.config_pane().expect("the pane is open").edits().len(),
        1,
        "the fixture files one edit"
    );
    app
}

/// The daemon's refusal for a write that fails config validation: what a
/// `cwd` the shepherd's user cannot enter comes back as.
pub fn invalid_config() -> RequestError {
    RequestError::Rpc(RpcError {
        code: RpcErrorCode::InvalidConfig,
        message: "cwd: no such directory".to_string(),
        daemon_version: None,
    })
}

/// The active group's own field rows, as their key names: a bounded slice
/// of the config pane's state rather than a search over the rendered
/// frame, which is what keeps a test on this from passing off a match in
/// the legend or another section.
///
/// # Panics
///
/// Panics if the pane is closed, which is a fixture bug rather than a
/// failure the test is about.
#[track_caller]
pub fn config_pane_field_rows_for_tests(app: &App) -> Vec<String> {
    let pane = app.config_pane().expect("the pane is open");
    pane.rows()
        .into_iter()
        .filter_map(|row| match row {
            crate::lookout::pane::PaneRow::Field(index) => {
                Some(pane.fields().fields()[index].key.clone())
            }
            crate::lookout::pane::PaneRow::Env(_) | crate::lookout::pane::PaneRow::AddEnv => None,
        })
        .collect()
}

/// The active group's own env rows: one entry per env key, then
/// `+ add a key`. Searched below the `env` section header and nowhere else,
/// so a test on this cannot pass off a match from the field list above it:
/// the keys and the field names share one namespace on screen, and a sheep
/// has fields called `user` and `env`.
///
/// Takes a `ConfigPane` directly rather than an `App`, since some of this
/// pane's own tests build one without a dashboard around it.
///
/// # Panics
///
/// Panics if it draws no row for a key or for `+ add a key`, which is a
/// fixture bug rather than a failure the test is about.
#[track_caller]
pub fn config_pane_env_rows_for_tests(pane: &crate::lookout::pane::ConfigPane) -> Vec<String> {
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), 160, 0);
    let rendered_lines: Vec<String> = lines.iter().map(rendered).collect();
    // The `env` section header is the bound. Everything above it is a field
    // row or chrome, and a prefix match over the whole frame would hand back
    // the `user` field's row for an env key named `user`.
    //
    // The header sits at column 2 and every row under it at column 3, which
    // is what tells the header apart from a field called `env`. It is not
    // matched whole because the explanation panel is merged to the right of
    // it at this width, so the line carries the panel's own row too.
    let header = rendered_lines
        .iter()
        .position(|line| {
            line.strip_prefix("  env")
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        })
        .expect("the pane draws an env section header");
    let env_rows = &rendered_lines[header + 1..];
    let mut rows: Vec<String> = pane
        .env_key_names()
        .iter()
        .map(|name| {
            env_rows
                .iter()
                .find(|line| {
                    line.trim_start_matches(['>', ' '])
                        .starts_with(name.as_str())
                })
                .unwrap_or_else(|| panic!("no row for env key {name}"))
                .clone()
        })
        .collect();
    rows.push(
        env_rows
            .iter()
            .find(|line| line.contains("add a key"))
            .expect("the pane draws a + add a key row")
            .clone(),
    );
    rows
}

/// Every filed edit's own key, as [`config_pane_field_rows_for_tests`] does
/// for the active group's own fields: a bounded slice of the pane's own
/// edit set, never a search over the rendered frame.
///
/// # Panics
///
/// Panics if the pane is closed.
#[track_caller]
pub fn config_pane_pending_rows_for_tests(app: &App) -> Vec<String> {
    let pane = app.config_pane().expect("the pane is open");
    pane.edits()
        .iter()
        .map(|(key, _)| match key {
            crate::lookout::edits::EditKey::Field(name) => name.clone(),
            crate::lookout::edits::EditKey::Env(name) => format!("env.{name}"),
        })
        .collect()
}

/// The one rendered line naming `key`, wherever it draws: the active
/// group's own field row, or the pending-edits section when `key` belongs
/// to a group not on screen. Bounded to that single row by stripping the
/// mark, lock and flag columns and requiring what is left to start with
/// `key`, which is what keeps this from matching a longer key or the
/// legend.
///
/// # Panics
///
/// Panics if the pane is closed or draws no row for `key`.
#[track_caller]
pub fn config_pane_row_for_tests(app: &App, key: &str) -> String {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), 160, 0);
    lines
        .iter()
        .map(rendered)
        .find(|line| {
            line.trim_start_matches(['>', ' ', '=', '~', '!', '*'])
                .starts_with(key)
        })
        .unwrap_or_else(|| panic!("no row for {key}"))
}

/// The pane's own title band, alone: line zero of the rendered frame, the
/// one line [`crate::lookout::view::pane::pane_lines`] ever puts the edit
/// count in.
///
/// # Panics
///
/// Panics if the pane is closed.
#[track_caller]
pub fn config_pane_title_band_for_tests(app: &App, width: u16) -> String {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), width, 0);
    rendered(&lines[0])
}

/// The pane's own tab row, alone: the one line naming every group in
/// [`shep_core::config::GROUP_ORDER`], found by its own `tab next group`
/// phrase rather than by a fixed index, so a chrome line gained or lost
/// above it does not silently move which row this reads.
///
/// # Panics
///
/// Panics if the pane is closed or draws no tab row at `width`.
#[track_caller]
pub fn config_pane_tab_row_for_tests(app: &App, width: u16) -> String {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), width, 0);
    lines
        .iter()
        .map(rendered)
        .find(|line| line.contains("tab next group"))
        .expect("the pane draws a tab row at this width")
}

/// Whether the pane draws a tab row at `width`, without panicking when it
/// does not: the non-panicking half of [`config_pane_tab_row_for_tests`],
/// for a caller (a dog pane, which has no groups) asserting the row's
/// absence rather than reading its content.
pub fn config_pane_draws_a_tab_row(app: &App, width: u16) -> bool {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), width, 0);
    lines
        .iter()
        .map(rendered)
        .any(|line| line.contains("tab next group"))
}

/// Whether the pane's own merged frame includes the explanation panel at
/// `width`: the wiring question `pane_lines` answers by width alone, which
/// [`config_pane_panel_for_tests`] cannot: that helper calls `panel_lines`
/// directly, and `panel_lines` draws unconditionally, carrying none of
/// `pane_lines`' own decision about whether the terminal is wide enough to
/// show it at all.
pub fn config_pane_draws_a_panel(app: &App, width: u16) -> bool {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), width, 0);
    lines
        .iter()
        .map(rendered)
        .any(|line| line.contains("FOCUSED"))
}

/// [`app_in_sheep_pane`], named for the one test that cares the palette
/// carries no colour. [`app_in_sheep_pane`] already builds on [`plain`], so
/// this alias adds no behaviour; it exists to make that guarantee visible
/// at the call site rather than left implicit in a fixture named for
/// something else.
pub fn app_with_plain_palette_in_sheep_pane() -> App {
    app_in_sheep_pane()
}

/// The explanation panel for whichever field the pane's own cursor is on,
/// as plain rows: what [`crate::lookout::view::pane::panel_lines`] draws,
/// styles dropped, at the app's own palette.
///
/// # Panics
///
/// Panics if the pane is closed.
#[track_caller]
pub fn config_pane_panel_for_tests(app: &App, width: u16) -> Vec<String> {
    let pane = app.config_pane().expect("the pane is open");
    crate::lookout::view::pane::panel_lines(pane, app.palette(), width)
        .iter()
        .map(rendered)
        .collect()
}

/// The explanation panel for the field named `key`, regardless of where the
/// pane's own cursor sits: a bounded look at one field's own panel content
/// rather than a walk that would first have to move the cursor there.
///
/// # Panics
///
/// Panics if the pane is closed or has no field named `key`.
#[track_caller]
pub fn config_pane_panel_focused_on(app: &App, key: &str, width: u16) -> Vec<String> {
    let pane = app.config_pane().expect("the pane is open");
    let field = pane
        .fields()
        .by_key(key)
        .unwrap_or_else(|| panic!("no field named {key}"));
    crate::lookout::view::pane::panel_for_field(field, pane, app.palette(), width)
        .iter()
        .map(rendered)
        .collect()
}

/// The shepherd's refusal of one write, for the tests about what a reply
/// says once the pane that asked for it has gone.
pub fn a_refusal() -> RequestError {
    RequestError::Rpc(shep_core::protocol::RpcError {
        code: shep_core::protocol::RpcErrorCode::InvalidConfig,
        message: "the store is locked by another shep".to_owned(),
        daemon_version: None,
    })
}
