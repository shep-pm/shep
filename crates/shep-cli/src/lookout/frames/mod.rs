//! Renders a `Buffer` to plain text or ANSI, and holds the scene list the
//! pinned snapshots and the gallery share.
//!
//! `docs/lookout/frames.txt` and `docs/lookout/frames.ansi` are generated
//! from this module's output, doubling as a rendered layout reference.
//!
//! Gated at the `mod` declaration with `#[cfg(test)]` rather than
//! `pub mod`: `lib.rs` exposes only three entry points, so an ordinary
//! `pub mod` here is unreachable from outside the crate and fails
//! `dead_code`.

mod build;
mod render;
mod scene;

// Re-exported so `crate::lookout::frames::render_text` keeps resolving for
// its callers elsewhere in `lookout` (dashboard snapshot tests, mostly)
// after this module split `render_text` out into its own file.
use render::render_ansi;
pub use render::render_text;
use scene::Scene;

use build::scene_with;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;

use shep_core::config::AppConfig;
use shep_core::protocol::{DogSource, ExitInfo, ProcessInfo, SheepConfigView};
use shep_core::status::ProcStatus;

use super::app::{App, KeyPress, Msg, RowKey, SettingsRow};
use super::tail::{Stream, Tail, TailLine};
use super::theme::Palette;
use crate::commands::settings::{DogView, ScalarView, SettingField, SettingsSnapshot};
use crate::style::StyleSource;

/// The gallery's own coloured palette (`xterm-256color`, the deep tier).
/// Every pinned `.snap` test and `docs/lookout/frames.ansi` render through
/// this one, so a real terminal at that tier sees exactly what they pin.
#[must_use]
fn coloured_palette() -> Palette {
    Palette::detect(None, Some(std::ffi::OsStr::new("xterm-256color")), None)
}

/// The flattened `NO_COLOR` palette. `docs/lookout/frames.txt` renders
/// through this one, not the coloured one: a plain-text gallery cannot
/// carry a painted background, so rendering it through the palette an
/// operator with `$NO_COLOR` set actually gets is what makes it an honest
/// picture rather than a coloured frame with the color silently missing.
#[must_use]
fn no_color_palette() -> Palette {
    Palette::detect(Some(std::ffi::OsStr::new("1")), None, None)
}

/// Builds one scene and returns its label with the buffer it drew into.
///
/// Renders at ten minutes of dashboard age, the same age the pinned
/// snapshots and `docs/lookout/frames.ansi` use, through
/// [`coloured_palette`].
#[must_use]
pub fn scene(which: Scene) -> (&'static str, Buffer) {
    (
        which.label(),
        scene_with(which, Duration::from_secs(600), coloured_palette()),
    )
}

/// Parks the gallery's cursor on the sheep with id `id`.
///
/// Walks by name since `SelectDown` moves one visible row and the table
/// reads by name, not id.
///
/// # Panics
///
/// If `id` is not in the flock, or is hidden by a filter.
#[track_caller]
fn select_id(app: &mut App, id: u32) {
    select_row(app, &RowKey::Sheep(id));
}

/// Parks the gallery's cursor on `name`'s group header.
///
/// # Panics
///
/// If `name` has no group header: an app with one instance, or an
/// instance that reports no slot.
#[track_caller]
fn select_group(app: &mut App, name: &str) {
    select_row(app, &RowKey::Group(name.to_string()));
}

/// Parks the gallery's cursor on `name`'s fold header.
///
/// # Panics
///
/// If `name` has no fold header: the table is not in the fold view yet, or
/// nothing in the flock carries that fold.
#[track_caller]
fn select_fold(app: &mut App, name: &str) {
    select_row(app, &RowKey::Fold(name.to_string()));
}

/// Walks the cursor to `key`, up or down as `key`'s position relative to the
/// current one calls for.
///
/// `Folds` needs the reverse direction: pressing `F` leaves the cursor on
/// whichever sheep the flat view had selected, which can sit past a fold's
/// header in the new, by-fold order, so a downward-only walk could never
/// reach it. Every other scene's target still happens to sit at or after the
/// starting row, so this is a superset of what a forward-only walk did, not
/// a behaviour change for them.
///
/// Budgeted by [`App::visible_rows`]'s length, not the flock count: a
/// grouped app's header adds a visible row beyond its own slots.
///
/// # Panics
///
/// If `key` is not a visible row.
#[track_caller]
fn select_row(app: &mut App, key: &RowKey) {
    for _ in 0..=app.visible_rows().len() {
        if app.selected().as_ref() == Some(key) {
            return;
        }
        let target = app.visible_rows().iter().position(|row| row == key);
        let step = match (app.selected_index(), target) {
            (Some(current), Some(target)) if target < current => KeyPress::SelectUp,
            _ => KeyPress::SelectDown,
        };
        app.update(Msg::Key(step));
    }
    panic!("the gallery cannot park its cursor on {key:?}");
}

/// Runs `rows` through two polls, two seconds apart, so
/// `App::record_samples` has something to difference: a fixture built from
/// one snapshot never gets past the first insert into `App::cpu_last`,
/// which returns nothing to draw rather than a false zero (see
/// `App::record_samples`'s own doc), and every CPU cell and sparkline in
/// the gallery rendered a bare `-` for exactly that reason until this
/// existed.
///
/// The second, final poll lands at `anchor` itself, not two seconds after
/// it: every uptime and age figure below reads from `anchor`, so parking
/// the *first*, throwaway poll two seconds *before* it is what keeps this
/// helper a pure addition rather than a two-second wobble on every uptime
/// in the gallery that used to land on a round minute.
///
/// `deltas` names each running sheep's id and its two `cpu_ms` readings,
/// oldest first; a row whose id is absent from `deltas` keeps whatever
/// `cpu_ms` its caller already gave it (`None`, for every fixture below),
/// so a stopped or errored row still reads a bare `-` on purpose.
///
/// Mutates `rows` in place across both polls, the same shape
/// `Scene::SheepPane`'s own fixture uses and `Scene::CfgDrift`'s does not:
/// `CfgDrift` polls a clone and leaves its own returned row's `cpu_ms` at
/// `None`, which is why that scene's CPU cell still reads `-` despite its
/// sparkline drawing a real shape.
fn poll_twice(
    app: &mut App,
    anchor: Instant,
    mut rows: Vec<ProcessInfo>,
    deltas: &[(u32, u64, u64)],
) -> Vec<ProcessInfo> {
    for &(id, first, _) in deltas {
        if let Some(row) = rows.iter_mut().find(|row| row.id == id) {
            row.cpu_ms = Some(first);
        }
    }
    app.update(Msg::Snapshot {
        rows: rows.clone(),
        at: anchor - Duration::from_secs(2),
    });
    for &(id, _, second) in deltas {
        if let Some(row) = rows.iter_mut().find(|row| row.id == id) {
            row.cpu_ms = Some(second);
        }
    }
    app.update(Msg::Snapshot {
        rows: rows.clone(),
        at: anchor,
    });
    rows
}

/// The bleats feed each scene is given, before `Msg::Bleats` carries it in.
///
/// Most scenes: six ordinary `Stream::Out` lines, no missed lines or
/// bytes.
///
/// `FeedGap`: thirty lines, `missed_lines: 500`, `missed_bytes:
/// 4_012_000`, so the header reads both a dropped-lines and a
/// never-read-bytes count.
///
/// `FeedMissing`: no lines, no counts, and a `note` mirroring
/// [`super::tail::read`]'s wording for a log file that was never created.
///
/// `WithDogs`: the selected row is the adopted `log-rotate` dog, so its
/// lines say what a log-rotate dog says rather than the default fixture's
/// web-server lines.
///
/// `Bleats`: sixteen lines, ten `out` and six `err`, eight carrying a level
/// word and eight carrying none, one of them 153 characters long. Built to
/// be filtered and wrapped, not read raw: see [`scene_with`]'s own
/// `Scene::Bleats` arm for the axes it stacks on top.
fn feed_for(which: Scene) -> Tail {
    match which {
        // Mirrors `run_ui`: an empty flock has no selected row, so no
        // `tail::read` call, just the pane's own "no sheep is selected"
        // header.
        Scene::Empty => Tail::default(),
        Scene::WithDogs => Tail {
            lines: [
                "rotated /var/log/api/access.log -> access.log.1",
                "compressed access.log.1 (4.2M -> 380K)",
                "pruned 2 archives older than 14 days",
                "next rotation in 6h",
            ]
            .into_iter()
            .map(|text| TailLine {
                stream: Stream::Out,
                text: text.to_string(),
            })
            .collect(),
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 256,
            note: None,
        },
        Scene::FeedGap => Tail {
            lines: (0..30)
                .map(|n| TailLine {
                    stream: Stream::Out,
                    text: format!("GET /v1/orders/{n} 200 {}ms", 8 + n % 40),
                })
                .collect(),
            missed_lines: 500,
            missed_bytes: 4_012_000,
            read_bytes: 65_536,
            note: None,
        },
        Scene::FeedMissing => Tail {
            lines: Vec::new(),
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 0,
            note: Some("this sheep has not written a log in this $SHEP_HOME".to_string()),
        },
        // Ten `out` lines and six `err`, eight carrying a real level word
        // (three `WARN`, two `DEBUG`, two `ERROR`, one `INFO`) and eight
        // carrying none, so the level axis has both a floor to apply and
        // unclassifiable lines to exempt from it. One line, the `WARN
        // retrying...` one, is 153 characters: long enough that
        // `Scene::Bleats`'s 100-column frame wraps it onto two rows.
        Scene::Bleats => Tail {
            lines: [
                (Stream::Out, "listening on 0.0.0.0:8080"),
                (
                    Stream::Err,
                    "WARN connection pool nearing capacity: 47/50 in use",
                ),
                (Stream::Out, "GET /v1/orders 200 12ms"),
                (
                    Stream::Out,
                    "WARN retrying upstream payment gateway after a timeout, backing off \
                     500ms before trying again with jitter added so the whole fleet does \
                     not retry at once",
                ),
                (
                    Stream::Err,
                    "INFO shutting down worker 2 for a rolling restart",
                ),
                (Stream::Out, "DEBUG cache warmed 128 keys in 4ms"),
                (Stream::Out, "POST /v1/orders 201 88ms"),
                (
                    Stream::Err,
                    "ERROR failed to write session cache: disk quota exceeded",
                ),
                (Stream::Out, "connection pool: 14/50 in use"),
                (
                    Stream::Out,
                    "ERROR panic recovered in worker 3: index out of bounds",
                ),
                (Stream::Err, "GET /healthz 200 3ms"),
                (Stream::Out, "GET /v1/orders/8821 200 9ms"),
                (Stream::Err, "DEBUG flushing metrics buffer"),
                (Stream::Out, "POST /v1/orders 500 210ms"),
                (
                    Stream::Err,
                    "WARN queue depth crossed 200, backpressure engaged",
                ),
                (Stream::Out, "GET /v1/orders/9013 200 7ms"),
            ]
            .into_iter()
            .map(|(stream, text)| TailLine {
                stream,
                text: text.to_string(),
            })
            .collect(),
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 2048,
            note: None,
        },
        _ => Tail {
            lines: [
                "listening on 0.0.0.0:8080",
                "GET /healthz 200 3ms",
                "GET /v1/orders 200 44ms",
                "POST /v1/orders 201 88ms",
                "GET /v1/orders/8821 200 9ms",
                "connection pool: 14/50 in use",
            ]
            .into_iter()
            .map(|text| TailLine {
                stream: Stream::Out,
                text: text.to_string(),
            })
            .collect(),
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 512,
            note: None,
        },
    }
}

/// One row's worth of shepherd reply, spelled out so each scene reads as
/// a plausible flock rather than six copies of one sheep.
///
/// The two log paths are derived from `name` and `id` rather than taken
/// as parameters: this already carries
/// `#[allow(clippy::too_many_arguments)]` at eight.
#[allow(clippy::too_many_arguments)]
fn sheep(
    id: u32,
    name: &str,
    status: ProcStatus,
    pid: Option<u32>,
    restarts: u32,
    cpu: Option<f32>,
    memory: Option<u64>,
    fold: Option<&str>,
) -> ProcessInfo {
    ProcessInfo::builder(id, name, status)
        .pid(pid)
        .restarts(restarts)
        .uptime_ms(4_512_000 + u64::from(id) * 91_000)
        .cpu_percent(cpu)
        .memory_bytes(memory)
        .fold(fold.map(str::to_string))
        .out_file(Some(format!("/home/ada/.shep/logs/{name}-{id}-out.log")))
        .err_file(Some(format!("/home/ada/.shep/logs/{name}-{id}-err.log")))
        // Derived from `status`, not a ninth parameter: a sheep that is
        // not running always has a reason it stopped, so no scene can
        // depict one with a blank EXIT column.
        .last_exit(match status {
            // Crashed on its own, and a restart is either pending or spent.
            ProcStatus::Errored | ProcStatus::WaitingRestart => Some(ExitInfo {
                code: Some(1),
                signal: None,
            }),
            // Stopped because shep asked it to, which is a signal.
            ProcStatus::Stopped => Some(ExitInfo {
                code: None,
                signal: Some(15),
            }),
            // Running, or on its way in or out: nothing has exited yet.
            ProcStatus::Online | ProcStatus::Starting | ProcStatus::Stopping => None,
        })
        .build()
}

/// An online sheep with `max_memory` set, [`sheep`]'s own default. Only
/// `Scene::MemCeiling` needs one, so this stays a thin wrapper rather than
/// a ninth parameter on `sheep` every other caller would have to pass
/// `None` for.
fn sheep_with_ceiling(id: u32, name: &str, memory: u64, ceiling: u64) -> ProcessInfo {
    let mut info = sheep(
        id,
        name,
        ProcStatus::Online,
        Some(48_300 + id),
        0,
        Some(2.0),
        Some(memory),
        Some("edge"),
    );
    info.max_memory = Some(ceiling);
    info
}

/// `web`'s own config, the way the shepherd would answer
/// `Request::SheepConfig`: every sheep-pane scene's config column reads
/// this rather than sitting on "reading config…".
fn sheep_pane_config_view() -> SheepConfigView {
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
    SheepConfigView::new(config, Vec::new(), Vec::new())
}

/// One instance of a clustered app: the row a shepherd reports for slot
/// `slot` of `name`.
///
/// Takes an explicit `uptime_ms` rather than deriving it from the id like
/// [`sheep`] does: a group row's uptime is the minimum across its
/// members, and grouping should not depend on id arithmetic.
fn instance(
    id: u32,
    name: &str,
    slot: u32,
    restarts: u32,
    cpu: f32,
    memory: u64,
    uptime_ms: u64,
) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online)
        .instance(Some(slot))
        .pid(Some(48_400 + id))
        .restarts(restarts)
        .uptime_ms(uptime_ms)
        .cpu_percent(Some(cpu))
        .memory_bytes(Some(memory))
        .fold(Some("edge".to_string()))
        .out_file(Some(format!("/home/ada/.shep/logs/{name}-{id}-out.log")))
        .err_file(Some(format!("/home/ada/.shep/logs/{name}-{id}-err.log")))
        .build()
}

/// The row `ActionAccepted`'s reply carries: `api` at id 2, restarted.
///
/// Pid 48299, not the listing's 48219: a matching pid would pass whether
/// the reply's row was upserted or silently ignored.
fn restarted_api() -> ProcessInfo {
    sheep(
        2,
        "api",
        ProcStatus::Online,
        Some(48_299),
        2,
        Some(7.1),
        Some(241 << 20),
        Some("edge"),
    )
}

/// The default six-sheep flock `scene_with`'s `_` arm builds, with id 2
/// (`api`) removed: five rows, ids 0, 1, 3, 4, 5.
fn flock_without_api() -> Vec<ProcessInfo> {
    vec![
        sheep(
            0,
            "web",
            ProcStatus::Online,
            Some(48_211),
            0,
            Some(3.4),
            Some(182 << 20),
            Some("edge"),
        ),
        sheep(
            1,
            "web",
            ProcStatus::Online,
            Some(48_212),
            0,
            Some(2.9),
            Some(178 << 20),
            Some("edge"),
        ),
        sheep(
            3,
            "billing-reconciliation-worker",
            ProcStatus::Online,
            Some(48_230),
            0,
            Some(0.8),
            Some(96 << 20),
            None,
        ),
        sheep(
            4,
            "cron",
            ProcStatus::Online,
            Some(48_233),
            0,
            Some(0.1),
            Some(8 << 20),
            None,
        ),
        sheep(
            5,
            "metrics",
            ProcStatus::Online,
            Some(48_240),
            0,
            Some(0.4),
            Some(11 << 20),
            None,
        ),
    ]
}

/// One dog process: `id` and `name` as `dog_rows`'s join key. `handshook`
/// carries the three-state signal a real listing does: `None` reads
/// `online`, `Some(false)` reads `silent`, per
/// [`crate::vocabulary::Reported::of`].
fn dog_sheep(id: u32, name: &str, source: DogSource, handshook: Option<bool>) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online)
        .pid(Some(90_000 + id))
        .dog(Some(source))
        .handshook(handshook)
        .build()
}

/// Walks the settings screen's cursor onto `field`'s row with real
/// `SelectDown` keypresses.
///
/// # Panics
///
/// If the settings screen is not open.
#[track_caller]
fn move_settings_cursor_to(app: &mut App, field: SettingField) {
    let target = app
        .settings()
        .expect("the settings screen must already be open")
        .rows()
        .iter()
        .position(|row| *row == SettingsRow::Scalar(field))
        .expect("field is one of the six scalar rows Settings::rows always carries");
    for _ in 0..target {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
}

/// A settings snapshot with `log_level`, `socket`, `max_cron_sleep` and
/// `style_level` declared in `shep.toml`, and `log_json`/`allow_control`
/// left at their compiled defaults: the mixed state
/// [`Scene::SettingsSet`] shows, reused by [`Scene::SettingsConfirm`] and
/// [`Scene::SettingsTyping`].
fn settings_snapshot_for_gallery() -> SettingsSnapshot {
    let config = |value: &str| ScalarView {
        value: value.to_string(),
        source: StyleSource::Config,
    };
    let default = |value: &str| ScalarView {
        value: value.to_string(),
        source: StyleSource::Default,
    };
    SettingsSnapshot {
        log_level: config("warn"),
        log_json: default("false"),
        socket: config("/home/ada/.shep/run/shep.sock"),
        max_cron_sleep: config("30s"),
        allow_control: default("false"),
        style_level: config("full"),
        // The document declares it, so the file and resolved value agree.
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

/// [`settings_snapshot_for_gallery`]'s scalars, with `dogs` replaced by
/// the three-way drift [`Scene::SettingsDogs`] shows: `otel` running
/// while the file disables it, `ledger` enabled and absent from the
/// flock, and `bark` enabled with `handshook: Some(false)`, so the join
/// reads it `silent`.
fn settings_snapshot_with_dog_drift() -> SettingsSnapshot {
    SettingsSnapshot {
        dogs: vec![
            // Both carry a real path: a non-built-in dog with
            // `adopted_path: None` is not a row `load_settings` can
            // produce from a document shep wrote.
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
            DogView {
                name: "bark".to_string(),
                enabled: true,
                adopted_path: None,
            },
        ],
        ..settings_snapshot_for_gallery()
    }
}

/// `api`'s config as the shepherd would answer it, for the four
/// editing-pane scenes: two env keys whose values never reach the view (the
/// pane's own `(set)` rendering is what a scene checks, not a real secret),
/// and no edits, overrides or pending fields of its own. [`Scene::EditPane`]
/// draws it fresh; [`Scene::EditPaneEdited`] files two edits on top of it.
fn edit_pane_config_view() -> SheepConfigView {
    let mut config = AppConfig {
        name: "api".to_string(),
        script: "./api/server.js".to_string(),
        args: vec!["--port".to_string(), "8080".to_string()],
        max_restarts: 32,
        instances: 1,
        ..AppConfig::default()
    };
    config
        .env
        .insert("NODE_ENV".to_string(), "production".to_string());
    config
        .env
        .insert("DATABASE_URL".to_string(), "postgres://db/api".to_string());
    SheepConfigView::new(config, Vec::new(), Vec::new())
}

/// `api`'s config for the close-dialog scenes: the same fields
/// [`edit_pane_config_view`] carries, plus `listen_timeout` already
/// parked, so `CloseDialogParked` has something to name without any edit
/// of the operator's own, and the boxed scenes show both halves of the
/// dialog's heading at once.
fn close_dialog_config_view() -> SheepConfigView {
    let mut config = AppConfig {
        name: "api".to_string(),
        script: "./api/server.js".to_string(),
        args: vec!["--port".to_string(), "8080".to_string()],
        max_restarts: 32,
        instances: 1,
        ..AppConfig::default()
    };
    config
        .env
        .insert("NODE_ENV".to_string(), "production".to_string());
    config
        .env
        .insert("DATABASE_URL".to_string(), "postgres://db/api".to_string());
    SheepConfigView::new(config, Vec::new(), vec!["listen_timeout".to_string()])
}

/// The header both gallery files open with.
///
/// Not a doc comment on the test: this text is read by a person opening
/// `docs/lookout/frames.txt` with no context at all, and it is the only
/// place that says where those frames came from.
const GALLERY_PREAMBLE: &str = "shep lookout frames
===================

These are real frames, rendered headlessly through ratatui's TestBackend by

    cargo test -p shep --lib --all-features -- --ignored write_the_gallery

Nothing here is a mockup.

frames.ansi renders all fifty-eight scenes through the same coloured
palette the pinned `.snap` tests use; read it with `less -R`. frames.txt
renders the same fifty-eight scenes through the flattened NO_COLOR palette
instead, the one an operator with $NO_COLOR set or a 16-colour terminal
actually gets. The two files are deliberately different pictures of the
same dashboard, not one file with the colour removed.

All four panes are here: the flock table (the spine), the host-usage strip,
the sheep detail pane and the bleats feed. The selected row is a painted
gutter in frames.ansi; in frames.txt it falls back to a `>` marker, since
the NO_COLOR palette has no ground to paint with. Every pane below the
table describes whatever that row is: one sheep usually, and a rollup with
no single log where the cursor sits on a group or a fold header.

The feed reads the selected sheep's log files from disk and re-reads them with
each flock listing. It is not a live subscription, and it says so on its own
header line: `out then err` because the two files are shown end to end with no
interleaving, and `re-read with each listing` because a two-second gap in this
pane is the refresh, not the sheep.

When the pane cannot show everything, the header says what went instead. Lines
it read and dropped are counted exactly; bytes below its 64 KiB window were
never read at all, so those are reported in bytes, because nothing counted the
lines in them and guessing would be worse than saying so.

Four frames show the editing pane, `e` from the dashboard, on a sheep's own
row: fresh at the 160x48 design target, the same pane with two edits filed
(one of them needing a respawn), and the same fresh pane at 120 and at 88
columns, where the explanation panel and the LANDS column trade places as
the width falls.

The last eight are the keymap overlay, `h` or `?` from any body. Boxed and
centred at the 160x48 target, at 130 where the border only just fits,
borderless at 129 one column below that floor, at 100 where DOING drops to a
bank of its own, and at 70 where two columns are all that fit. Three more
change what the bottom line says rather than the layout: a frozen dashboard,
a read-only one, and a terminal too short for the box at all.

Before those, four show the close dialog `esc` raises over the editing pane
when a change is filed the running child cannot take. Boxed and centred at the 160x48
target, the same at 90 where the border only just fits, borderless at 89 one
column below its floor, and the parked-only heading on a sheep whose fields
were written before the pane was opened.

Before those are the four sheep-pane scenes, `↵` on a sheep, then the
secrets pane, `S`, and before them the full-screen bleats pane, `b` from
the dashboard. The seven before
that are the settings screen, `s` from the dashboard. It owns the whole body
between the title and the status bar rather than sharing it
with the flock table, so a fresh $SHEP_HOME, some scalars declared, an armed
confirm, the socket editor mid-type, the dogs table's own drift, the same
screen at 45 columns and the same screen too short to hold every row each get
a frame of their own.
";

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame pins, not wire fixtures: re-accepting these after a layout
    /// change is expected, unlike the rule for shep-core's protocol
    /// snapshots.
    ///
    /// `cfg(unix)`: one fixture carries a synthetic signalled exit, and
    /// `signal_label` resolves it against the running platform's table.
    /// Windows never sets a signal on `ExitOutcome`, so this only runs
    /// against a synthetic fixture; the pinned artifacts under
    /// `docs/lookout/` are unix renderings for the same reason.
    #[cfg(unix)]
    #[test]
    fn frames_are_pinned() {
        for which in Scene::ALL {
            let (label, buffer) = scene(*which);
            insta::assert_snapshot!(label, render_text(&buffer));
        }
    }

    /// The two gallery files' text: plain, then ANSI.
    ///
    /// Separate from the writer so a non-ignored test can read it.
    fn gallery_text() -> (String, String) {
        let mut plain = String::from(GALLERY_PREAMBLE);
        let mut ansi = String::from(GALLERY_PREAMBLE);
        for which in Scene::ALL {
            let (width, height) = which.size();
            let heading = format!(
                "\n\n=== {}  ({width}x{height}) ===\n{}\n\n",
                which.label(),
                which.caption()
            );
            // Two separate renders, not one buffer read twice: `frames.txt`
            // is what a `NO_COLOR` operator sees, and that palette has no
            // painted ground for the gutter to fall back from, so the
            // buffer itself differs, not just how it prints.
            let plain_buffer = scene_with(*which, Duration::from_secs(600), no_color_palette());
            plain.push_str(&heading);
            plain.push_str(&render_text(&plain_buffer));
            let ansi_buffer = scene_with(*which, Duration::from_secs(600), coloured_palette());
            ansi.push_str(&heading);
            ansi.push_str(&render_ansi(&ansi_buffer));
        }
        (plain, ansi)
    }

    /// The committed gallery matches what the generator would write today.
    ///
    /// `write_the_gallery` is `#[ignore]`d, so nothing in the ordinary
    /// suite catches a scene that landed without also running it: its own
    /// doc names the exact failure, both files still saying fifty scenes
    /// and holding none of the eight keymap scenes that shipped alongside
    /// them. Diffing against fresh output, rather than only counting
    /// headings, catches a mismatch a count would miss too, a caption
    /// edited without a corresponding regeneration.
    ///
    /// `cfg(unix)`: same reason as `frames_are_pinned` above. `signal_label`
    /// resolves a fixture's signal against the running platform's own
    /// table, and the committed artifacts under `docs/lookout/` are unix
    /// renderings, so a Windows-built binary regenerates the `cron` row
    /// reading a bare `15` where the committed file reads `SIGTERM`.
    #[cfg(unix)]
    #[test]
    fn the_committed_gallery_matches_what_the_generator_would_write() {
        let dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/lookout"));
        let (plain, ansi) = gallery_text();
        for (name, want) in [("frames.txt", &plain), ("frames.ansi", &ansi)] {
            let have = std::fs::read_to_string(dir.join(name)).unwrap_or_else(|e| {
                panic!(
                    "{name} is missing or unreadable ({e}); run \
                     cargo test -p shep --lib --all-features -- --ignored write_the_gallery"
                )
            });
            assert_eq!(
                &have, want,
                "{name} is stale against the current scenes; run \
                 cargo test -p shep --lib --all-features -- --ignored write_the_gallery"
            );
        }
    }

    #[test]
    fn the_gallery_carries_no_dashes() {
        let (plain, ansi) = gallery_text();
        for (file, text) in [("frames.txt", &plain), ("frames.ansi", &ansi)] {
            assert!(!text.contains('\u{2014}'), "em dash in {file}");
            assert!(!text.contains('\u{2013}'), "en dash in {file}");
        }
    }

    /// Writes `docs/lookout/frames.txt` and `docs/lookout/frames.ansi`.
    ///
    /// `#[ignore]`: writes into the repository, so it only runs on request.
    ///
    /// ```text
    /// cargo test -p shep --lib --all-features -- --ignored write_the_gallery
    /// ```
    ///
    /// A layout change cannot rot these files unnoticed: they render the
    /// same `Scene::ALL` the pinned snapshots read, so the ordinary suite
    /// reddens first and whoever fixes it comes back here.
    ///
    /// **Adding a scene without running this one is caught too, now.**
    /// `the_committed_gallery_matches_what_the_generator_would_write`
    /// diffs the committed files against fresh output, catching a scene
    /// missing from both while their own preamble still carries the old
    /// count. Run this anyway, in the same commit that adds a scene: a
    /// missing regeneration failing there is a one-line fix, failing in
    /// the other test means reading a full-file diff to find it.
    #[test]
    #[ignore = "writes into docs/lookout; run it deliberately"]
    fn write_the_gallery() {
        // Absolute, derived from the manifest, so it lands in the same
        // place whatever directory the run started in.
        let dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/lookout"));
        std::fs::create_dir_all(dir).unwrap();

        let (plain, ansi) = gallery_text();
        std::fs::write(dir.join("frames.txt"), &plain).unwrap();
        std::fs::write(dir.join("frames.ansi"), &ansi).unwrap();

        // A live assertion, not a `timeout`: this function is synchronous,
        // so a `tokio::time::timeout` around it would complete on its first
        // poll and bound nothing at all. What can actually go wrong here is
        // a scene rendering empty, and that is what these two check.
        assert!(
            plain.lines().count() > 100,
            "every scene in the gallery is more than a hundred lines together"
        );
        assert_eq!(plain.matches("=== ").count(), Scene::ALL.len());
    }
}
