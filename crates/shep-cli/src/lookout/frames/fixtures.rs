//! Cursor-parking helpers and the data builders behind every gallery scene:
//! flock rows, dog rows, config views, settings snapshots and the bleats
//! feed.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use shep_core::config::AppConfig;
use shep_core::protocol::{DogSource, ExitInfo, ProcessInfo, SheepConfigView};
use shep_core::status::ProcStatus;

use super::super::app::{App, KeyPress, Msg, RowKey, SettingsRow};
use super::super::tail::{Stream, Tail, TailLine};
use crate::commands::settings::{DogView, ScalarView, SettingField, SettingsSnapshot};
use crate::style::StyleSource;

use super::Scene;

/// Parks the gallery's cursor on the sheep with id `id`.
///
/// Walks by name since `SelectDown` moves one visible row and the table
/// reads by name, not id.
///
/// # Panics
///
/// If `id` is not in the flock, or is hidden by a filter.
#[track_caller]
pub(super) fn select_id(app: &mut App, id: u32) {
    select_row(app, &RowKey::Sheep(id));
}

/// Parks the gallery's cursor on `name`'s group header.
///
/// # Panics
///
/// If `name` has no group header: an app with one instance, or an
/// instance that reports no slot.
#[track_caller]
pub(super) fn select_group(app: &mut App, name: &str) {
    select_row(app, &RowKey::Group(name.to_string()));
}

/// Parks the gallery's cursor on `name`'s fold header.
///
/// # Panics
///
/// If `name` has no fold header: the table is not in the fold view yet, or
/// nothing in the flock carries that fold.
#[track_caller]
pub(super) fn select_fold(app: &mut App, name: &str) {
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
pub(super) fn poll_twice(
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
pub(super) fn feed_for(which: Scene) -> Tail {
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
pub(super) fn sheep(
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
pub(super) fn sheep_with_ceiling(id: u32, name: &str, memory: u64, ceiling: u64) -> ProcessInfo {
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
pub(super) fn sheep_pane_config_view() -> SheepConfigView {
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
pub(super) fn instance(
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
pub(super) fn restarted_api() -> ProcessInfo {
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
pub(super) fn flock_without_api() -> Vec<ProcessInfo> {
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
pub(super) fn dog_sheep(
    id: u32,
    name: &str,
    source: DogSource,
    handshook: Option<bool>,
) -> ProcessInfo {
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
pub(super) fn move_settings_cursor_to(app: &mut App, field: SettingField) {
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
pub(super) fn settings_snapshot_for_gallery() -> SettingsSnapshot {
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
pub(super) fn settings_snapshot_with_dog_drift() -> SettingsSnapshot {
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
pub(super) fn edit_pane_config_view() -> SheepConfigView {
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
pub(super) fn close_dialog_config_view() -> SheepConfigView {
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
