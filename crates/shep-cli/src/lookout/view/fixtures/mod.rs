//! Fixtures the pane test modules share.

mod flock;
mod host;
mod palette;
mod refusals;
mod render;

pub use self::flock::{
    acting_app, allowed_app, app_with, app_with_a_built_in_dog_selected_and_control,
    app_with_a_dog, app_with_a_dog_selected_and_control, armed_app,
    armed_app_with_a_filter_and_a_notice, editing_app, filtered_app, filtered_app_of, flock_of,
    full_app, instance_in_fold, sheep_in_fold, sheep_in_fold_with_status, sheep_with,
    with_no_selection, with_selection, with_selection_and_palette,
};
pub use self::host::{sample, with_host, with_host_none};
pub use self::palette::{coloured, no_color, plain, plain_dimmed};
pub use self::refusals::{a_refusal, invalid_config};
pub use self::render::{render, render_all, rendered, row_containing, row_starting_with, rows_of};

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use shep_client::RequestError;
use shep_core::config::{AppConfig, ProbeConfig, ProbeKind};
use shep_core::protocol::{DogSource, Lamb, ProcessInfo, Response, SheepConfigView};
use shep_core::status::ProcStatus;
use shep_core::values::UpDuration;

use crate::commands::settings::{DogView, ScalarView, SettingField, SettingsSnapshot};
use crate::lookout::app::{
    App, Body, CloseDialog, Control, Effect, KeyPress, LambWalk, Msg, RevealedValue, Sent,
    SettingsRow,
};
use crate::lookout::level::Level;
use crate::lookout::pane::{ConfigPane, ReloadKind};
use crate::lookout::secrets::{SecretRow, SecretsModel, Source};
use crate::lookout::tail::{Stream, Tail, TailLine};
use crate::lookout::theme::Palette;
use crate::secret_readers::Reader;
use crate::style::StyleSource;

/// What the last dial said when the ladder ran out, in
/// `crate::lookout::source::LinkError::Unreachable`'s own shape.
///
/// The link panel renders it verbatim, so every test that freezes a
/// dashboard feeds it verbatim rather than inventing a shorter sentence the
/// panel would never see.
pub const FROZEN_WHY: &str = "the shepherd did not answer: could not connect to `/home/ada/.shep/run/shep.sock`: Connection refused (os error 61)";

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
    render_all(&crate::lookout::view::detail::detail_lines(app, 200))
        .lines()
        .find(|line| line.starts_with("lambs  "))
        .map(str::to_string)
        .expect("the pane has a lamb line")
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

/// [`crate::lookout::view::bleats_full::draw`]'s own lines, for a test that needs the
/// bleats pane's rendered rows without a [`Buffer`] round trip. Thin
/// wrapper: [`crate::lookout::view::bleats_full::draw_lines`] is `pub(crate)` for exactly
/// this, but lives in a sibling module the top-level fixture callers in
/// `app.rs` do not otherwise reach.
///
/// [`Buffer`]: ratatui::buffer::Buffer
pub fn draw_lines(app: &App, width: u16, rows: usize) -> Vec<Line<'static>> {
    crate::lookout::view::bleats_full::draw_lines(app, width, rows)
}

/// One sheep, `catcher`, selected, with a two-line feed applied and its log
/// paths pointing at real files in a leaked tempdir, so `fs::metadata` in
/// [`crate::lookout::view::detail::log_row`] succeeds the way it would against a live
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
/// that failed to start. Exercises [`crate::lookout::view::settings::dog_rows`]'s join, not
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
/// (`handshook: Some(false)`), so [`crate::lookout::view::settings::dog_rows`] must read it
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
/// [`close_dialog_lines`](crate::lookout::view::pane::close::close_dialog_lines)
/// reads in its own heading and naming-sentence tests, without driving a
/// real key sequence to raise one.
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

/// [`crate::lookout::view::pane::draw_pane`] alone, straight into a fresh buffer at
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
    crate::lookout::view::pane::draw_pane(&app, pane, area, &mut buffer);
    buffer
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
