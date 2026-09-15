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

mod render;
mod scene;

// Re-exported so `crate::lookout::frames::render_text` keeps resolving for
// its callers elsewhere in `lookout` (dashboard snapshot tests, mostly)
// after this module split `render_text` out into its own file.
use render::render_ansi;
pub use render::render_text;
use scene::Scene;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use shep_client::RequestError;
use shep_core::config::AppConfig;
use shep_core::protocol::{
    DogSource, ExitInfo, Lamb, ProcessInfo, Response, RpcError, RpcErrorCode, SheepConfigView,
};
use shep_core::status::ProcStatus;

use super::app::{
    ActionVerb, App, Control, KeyPress, Msg, RevealedValue, RowKey, Sent, SettingsRow,
};
use super::secrets::{SecretRow, SecretsModel, Source};
use super::source::HostSample;
use super::tail::{Stream, Tail, TailLine};
use super::theme::Palette;
use super::view::fixtures::select_field;
use super::view::{body_rows, draw};
use crate::commands::settings::{
    DogView, ScalarView, SettingField, SettingsSnapshot, load_settings,
};
use crate::commands::shep_toml::ShepToml;
use crate::secret_readers::Reader;
use crate::style::{StyleLevel, StyleSource};

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

/// One scene, `age` after its opening snapshot, drawn through `palette`.
///
/// Deterministic: a forced palette, an explicit `Instant` advanced by exact
/// `Duration`s, and a literal frozen timestamp, so the gallery never
/// depends on this machine's clock or environment.
///
/// `palette` exists so the gallery can render the same scene twice: once
/// through [`coloured_palette`] for `docs/lookout/frames.ansi` and the
/// pinned snapshots, and once through [`no_color_palette`] for
/// `docs/lookout/frames.txt`.
///
/// `age` exists for
/// `the_frozen_frame_does_not_move_however_long_the_link_stays_gone`,
/// which renders the frozen scene at two ages and checks for identical
/// frames.
#[must_use]
fn scene_with(which: Scene, age: Duration, palette: Palette) -> Buffer {
    let t0 = Instant::now();
    let mut app = App::new(palette, which.control(), "/home/ada/.shep".to_string(), t0);

    let flock = match which {
        Scene::Empty => Vec::new(),
        // The only flock in the gallery whose rows carry a slot, and so the
        // only one that draws a group header at all. `api` is here so the
        // frame shows a grouped app beside an ungrouped one rather than
        // implying every app gets a header.
        Scene::Grouped => poll_twice(
            &mut app,
            t0,
            vec![
                instance(0, "web", 0, 0, 3.4, 182 << 20, 4_512_000),
                // The youngest of the three, and the one carrying restarts:
                // the group row's uptime is a minimum and its restarts are a
                // sum.
                instance(1, "web", 1, 2, 2.9, 178 << 20, 300_000),
                instance(2, "web", 2, 1, 3.1, 180 << 20, 9_000_000),
                sheep(
                    3,
                    "api",
                    ProcStatus::Online,
                    Some(48_219),
                    1,
                    Some(7.1),
                    Some(241 << 20),
                    Some("edge"),
                ),
            ],
            &[(0, 60, 130), (1, 40, 98), (2, 50, 112), (3, 100, 242)],
        ),
        // Two folds of differing size (`batch` at 32MiB, `core` at 96MiB),
        // the biggest fold (`edge`, at 781MiB) carrying a grouped app beside
        // a standalone one, two sheep in no fold at all, and a dog. Every
        // memory figure is a round number of MiB precisely so the SHARE
        // gauge and the NOTES percentage can be checked by hand rather than
        // trusted on sight: batch's 32 of 928 total MiB is 3%, core's 96 is
        // 10%, edge's 781 is 84%.
        Scene::Folds => poll_twice(
            &mut app,
            t0,
            vec![
                sheep(
                    20,
                    "reindexer",
                    ProcStatus::Online,
                    Some(48_500),
                    0,
                    Some(1.2),
                    Some(20 << 20),
                    Some("batch"),
                ),
                sheep(
                    21,
                    "backfill",
                    ProcStatus::Online,
                    Some(48_501),
                    3,
                    Some(0.4),
                    Some(12 << 20),
                    Some("batch"),
                ),
                sheep(
                    22,
                    "worker",
                    ProcStatus::Online,
                    Some(48_510),
                    0,
                    Some(2.0),
                    Some(96 << 20),
                    Some("core"),
                ),
                // `instance` always sets fold to `edge`, which is exactly
                // the fold this scene wants its one grouped app in.
                instance(23, "web", 0, 0, 3.4, 182 << 20, 4_512_000),
                instance(24, "web", 1, 2, 2.9, 178 << 20, 300_000),
                instance(25, "web", 2, 1, 3.1, 180 << 20, 9_000_000),
                sheep(
                    26,
                    "api",
                    ProcStatus::Online,
                    Some(48_219),
                    1,
                    Some(7.1),
                    Some(241 << 20),
                    Some("edge"),
                ),
                sheep(
                    27,
                    "cron",
                    ProcStatus::Online,
                    Some(48_233),
                    0,
                    Some(0.1),
                    Some(8 << 20),
                    None,
                ),
                sheep(
                    28,
                    "metrics",
                    ProcStatus::Online,
                    Some(48_240),
                    0,
                    Some(0.4),
                    Some(11 << 20),
                    None,
                ),
                dog_sheep(90, "bark", DogSource::BuiltIn, None),
            ],
            &[
                (20, 30, 54),
                (21, 10, 18),
                (22, 60, 100),
                (23, 90, 158),
                (24, 70, 128),
                (25, 80, 142),
                (26, 150, 292),
                (27, 5, 7),
                (28, 10, 18),
            ],
        ),
        Scene::Errored | Scene::Frozen | Scene::LambsUnknown => poll_twice(
            &mut app,
            t0,
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
                    2,
                    "api",
                    ProcStatus::Errored,
                    None,
                    14,
                    None,
                    None,
                    Some("edge"),
                ),
                sheep(
                    3,
                    "billing-reconciliation-worker",
                    ProcStatus::WaitingRestart,
                    None,
                    3,
                    None,
                    None,
                    None,
                ),
                sheep(4, "cron", ProcStatus::Stopped, None, 0, None, None, None),
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
            ],
            &[(0, 90, 158), (1, 70, 128), (5, 10, 18)],
        ),
        // Two dog processes: `otel` up and healthy, `bark` up but never
        // handshook. `ledger` has no row here, which is what "enabled and
        // absent" means in the settings snapshot below.
        Scene::SettingsDogs | Scene::SettingsNarrow | Scene::SettingsShort => vec![
            dog_sheep(90, "otel", DogSource::BuiltIn, None),
            dog_sheep(91, "bark", DogSource::BuiltIn, Some(false)),
        ],
        // A flock's two sections at once: three sheep, a healthy built-in
        // dog and a silent adopted one.
        Scene::WithDogs => poll_twice(
            &mut app,
            t0,
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
                    "api",
                    ProcStatus::Online,
                    Some(48_219),
                    1,
                    Some(7.1),
                    Some(241 << 20),
                    Some("edge"),
                ),
                sheep(
                    2,
                    "cron",
                    ProcStatus::Online,
                    Some(48_233),
                    0,
                    Some(0.1),
                    Some(8 << 20),
                    None,
                ),
                dog_sheep(90, "bark", DogSource::BuiltIn, None),
                dog_sheep(
                    91,
                    "log-rotate",
                    DogSource::Adopted {
                        path: "/usr/local/bin/shep-log-rotate".to_string(),
                    },
                    Some(false),
                ),
            ],
            &[(0, 90, 158), (1, 150, 292), (2, 5, 7)],
        ),
        // Decision 7's three MEM/CEIL states, which no other scene's
        // fixtures exercise: every other call to `sheep` leaves
        // `max_memory` at the wire default, `None`.
        Scene::MemCeiling => poll_twice(
            &mut app,
            t0,
            vec![
                sheep_with_ceiling(0, "web-headroom", 128 << 20, 512 << 20),
                sheep_with_ceiling(1, "web-hot", 480 << 20, 512 << 20),
                sheep(
                    2,
                    "batch-worker",
                    ProcStatus::Online,
                    Some(48_303),
                    0,
                    Some(1.0),
                    Some(64 << 20),
                    None,
                ),
            ],
            &[(0, 40, 74), (1, 90, 168), (2, 10, 30)],
        ),
        // The CFG column's two markers, `!N` and `*N`, plus a CPU history
        // long enough that `CpuSpark` draws a shape rather than one bar.
        // Neither has a fixture anywhere else in the gallery: no other
        // scene sets `pending` or `overridden`, and every other scene's
        // flock is built by one `Msg::Snapshot`, giving each sheep exactly
        // one CPU sample.
        Scene::CfgDrift => {
            let mut pending_row = sheep(
                10,
                "web",
                ProcStatus::Online,
                Some(48_410),
                0,
                Some(4.0),
                Some(64 << 20),
                Some("edge"),
            );
            pending_row.pending = Some(vec!["env".to_string(), "port".to_string()]);
            let mut overridden_row = sheep(
                11,
                "api",
                ProcStatus::Online,
                Some(48_411),
                0,
                Some(3.0),
                Some(96 << 20),
                Some("edge"),
            );
            overridden_row.overridden = Some(vec!["instances".to_string()]);
            let plain_row = sheep(
                12,
                "cron",
                ProcStatus::Online,
                Some(48_412),
                0,
                Some(0.5),
                Some(8 << 20),
                None,
            );
            // Nine snapshots ahead of the shared one below, each two seconds
            // apart and each moving `web`'s CPU counter by a distinct
            // delta, so the sparkline differences into a varying shape
            // rather than a single bar padded with blanks. The first of
            // the nine only records a baseline (see `Self::record_samples`),
            // so the history holds eight differenced samples plus the
            // trailing one below, which carries the loop's final reading.
            let mut cpu_ms = 0_u64;
            let mut at = t0;
            for delta_ms in [20_u64, 120, 50, 160, 70, 140, 40, 100, 130] {
                cpu_ms += delta_ms;
                at += Duration::from_secs(2);
                let mut warming_up = pending_row.clone();
                warming_up.cpu_ms = Some(cpu_ms);
                app.update(Msg::Snapshot {
                    rows: vec![warming_up, overridden_row.clone(), plain_row.clone()],
                    at,
                });
            }
            // The returned row has to carry the same reading the loop's
            // last snapshot sent, not the pre-loop clone: this used to
            // mutate `warming_up` only, leaving `pending_row.cpu_ms` at
            // `None` forever.
            pending_row.cpu_ms = Some(cpu_ms);
            vec![pending_row, overridden_row, plain_row]
        }
        // The one sheep every sheep-pane scene draws, `web`, run through
        // six real polls two seconds apart, cpu_ms and memory_bytes both
        // rising by varying deltas: task 4's counter differencing needs a
        // poll to differ against, so a scene built from a single snapshot
        // would draw an idle-looking chart while looking fine, the same
        // trap `Scene::CfgDrift`'s own fixture above exists to avoid.
        Scene::SheepPane
        | Scene::SheepPaneCpuOnly
        | Scene::SheepPaneSparklines
        | Scene::SheepPaneShort => {
            let mut at = t0;
            let mut row = sheep(
                20,
                "web",
                ProcStatus::Online,
                Some(48_500),
                0,
                Some(0.0),
                Some(10 << 20),
                Some("edge"),
            );
            row.max_memory = Some(64 << 20);
            for (cpu_ms, memory) in [
                (400_u64, 14 << 20),
                (900, 20 << 20),
                (1_300, 28 << 20),
                (2_000, 34 << 20),
                (2_400, 30 << 20),
            ] {
                at += Duration::from_secs(2);
                row.cpu_ms = Some(cpu_ms);
                row.memory_bytes = Some(memory);
                app.update(Msg::Snapshot {
                    rows: vec![row.clone()],
                    at,
                });
            }
            vec![row]
        }
        _ => {
            let mut rows = vec![
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
                    2,
                    "api",
                    ProcStatus::Online,
                    Some(48_219),
                    1,
                    Some(7.1),
                    Some(241 << 20),
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
            ];
            // `log_row`'s on-disk size had never rendered anywhere in the
            // gallery, because every fixture's
            // `out_file`/`err_file` name a path (`/home/ada/.shep/logs/...`)
            // that never exists on the machine running the test, so
            // `fs::metadata` always failed silently. `HealthyWide`'s
            // selected sheep, `api`, points at two real files instead,
            // committed under `crates/shep-cli/tests/fixtures/gallery-logs/`,
            // fixed at 1024 bytes each so the rendered size (`2.0K`) is the
            // same on every machine and every run.
            //
            // The path is relative rather than the fictional absolute shape
            // every other fixture uses: cargo sets a test binary's cwd to
            // its own crate's manifest directory on every platform, so
            // `fs::metadata` resolves this same string against the same
            // real file wherever the gallery is regenerated. An absolute
            // path would either stay fictional (the `/home/ada/...` shape,
            // never real) or, made real, would have to embed either a
            // random tempdir name (breaking `write_the_gallery`'s
            // idempotency: it must diff clean run twice) or the actual
            // checkout's home directory, which must never land in a file
            // this repository commits.
            //
            // Applied here, before `poll_twice` runs, rather than after: the
            // two polls below are this scene's only delivery into
            // `self.flock` now, so a patch applied afterward would never
            // reach it.
            if which == Scene::HealthyWide
                && let Some(api) = rows.iter_mut().find(|sheep| sheep.id == 2)
            {
                api.out_file = Some("tests/fixtures/gallery-logs/api-out.log".to_string());
                api.err_file = Some("tests/fixtures/gallery-logs/api-err.log".to_string());
            }
            poll_twice(
                &mut app,
                t0,
                rows,
                &[
                    (0, 90, 158),
                    (1, 70, 128),
                    (2, 150, 292),
                    (3, 10, 26),
                    (4, 5, 7),
                    (5, 10, 18),
                ],
            )
        }
    };

    // `Empty` and the settings-dogs trio never call `App::update` while
    // building `flock` above: `Empty`'s flock is empty and the dogs table
    // carries no CPU column, so neither needs `poll_twice`'s two-poll
    // shape, and this is the only snapshot either of them gets. Every other
    // scene already delivered its own final state through `poll_twice` (or,
    // for `CfgDrift` and the sheep-pane group, its own inline loop);
    // resending the same rows here at `t0`, older than that already-applied
    // poll, would difference against a zero or negative window and corrupt
    // the very last sample `poll_twice` just recorded back to `0.0` or
    // `None`: the bug this round exists to fix, not reintroduce.
    if matches!(
        which,
        Scene::Empty | Scene::SettingsDogs | Scene::SettingsNarrow | Scene::SettingsShort
    ) {
        app.update(Msg::Snapshot {
            rows: flock,
            at: t0,
        });
    }
    app.update(Msg::Tick {
        now: t0 + Duration::from_secs(7),
    });

    // Every `Keymap*` scene uses the healthy flock fixture above (the
    // default arm of the match) and raises the overlay right after: `h`
    // toggles `App::keymap_open` and nothing else, so it does not matter
    // that this runs ahead of the selection, the host sample or (for
    // `KeymapFrozen`) the freeze below.
    if which.is_keymap() {
        app.update(Msg::Key(KeyPress::Help));
    }

    // Selects `api` (id 2) so the panes below describe a fixed sheep,
    // walked by id since the table sorts by name. Skipped where there is
    // no flock, no pane below the table, or the cursor belongs elsewhere
    // (`Grouped`, `Folds`, and the three settings scenes with no id 2).
    if !matches!(
        which,
        Scene::Empty
            | Scene::Narrow
            | Scene::TooNarrow
            | Scene::TableOnly
            | Scene::Grouped
            | Scene::Folds
            | Scene::WithDogs
            | Scene::MemCeiling
            | Scene::CfgDrift
            | Scene::SettingsDogs
            | Scene::SettingsNarrow
            | Scene::SettingsShort
            | Scene::SheepPane
            | Scene::SheepPaneCpuOnly
            | Scene::SheepPaneSparklines
            | Scene::SheepPaneShort
    ) {
        select_id(&mut app, 2);
    }

    // Onto `web`'s group header, which is the one row in the gallery that is
    // not a sheep. Every pane below the table renders its own group state
    // from it: the rollup line, the per-instance lamb sentence, and the
    // feed's refusal to pick an instance to tail.
    if which == Scene::Grouped {
        select_group(&mut app, "web");
    }

    // `Folds` presses `F` to switch the table into the fold view, collapses
    // `batch` (the smallest fold, so `z`'s effect is visible without hiding
    // much), and parks the cursor on `edge`'s own header, the fold that
    // carries the grouped app.
    if which == Scene::Folds {
        app.update(Msg::Key(KeyPress::FoldView));
        select_fold(&mut app, "batch");
        app.update(Msg::Key(KeyPress::Collapse));
        select_fold(&mut app, "edge");
    }

    // `LambsUnknown` wants `cron`, id 4, instead.
    if which == Scene::LambsUnknown {
        select_id(&mut app, 4);
    }

    // `WithDogs` parks on the silent adopted dog, id 91, the row this
    // scene exists to show.
    if which == Scene::WithDogs {
        select_id(&mut app, 91);
    }

    // `MemCeiling` parks on `web-hot`, id 1, the row at 94% of its
    // ceiling: the butter warning this scene exists to show.
    if which == Scene::MemCeiling {
        select_id(&mut app, 1);
    }

    // `CfgDrift` parks on `web`, id 10, the row carrying the pending
    // fields: the detail pane's own `cfg !2 pending` cell only renders
    // for the selected sheep.
    if which == Scene::CfgDrift {
        select_id(&mut app, 10);
    }

    // Every sheep-pane scene parks on `web`, id 20 (the only row in its
    // own flock), opens the pane the same way the event loop does (`Enter`
    // on the selected row), and answers the config request it fires so the
    // config column has real fields to draw rather than "reading config…".
    if matches!(
        which,
        Scene::SheepPane
            | Scene::SheepPaneCpuOnly
            | Scene::SheepPaneSparklines
            | Scene::SheepPaneShort
    ) {
        select_id(&mut app, 20);
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(sheep_pane_config_view()))),
        });
    }

    match which {
        Scene::FilterEditing | Scene::FilterNoMatch | Scene::FilterActive => {
            app.update(Msg::Key(KeyPress::FilterStart));
            let query = if which == Scene::FilterNoMatch {
                "zzz"
            } else {
                "web"
            };
            for typed in query.chars() {
                app.update(Msg::Key(KeyPress::TextChar(typed)));
            }
            if which == Scene::FilterActive {
                app.update(Msg::Key(KeyPress::TextApply));
            }
        }
        _ => {}
    }

    // Every live scene gets a host sample, frozen included: without a
    // baseline sample the strip would read "not read yet" regardless of
    // the freeze guard. `Scene::Frozen` below sends a second, older
    // sample that exercises the guard itself.
    if which == Scene::HostUnknown {
        app.update(Msg::Host { sample: None });
    } else {
        app.update(Msg::Host {
            sample: Some(HostSample {
                load: (2.31, 4.10, 3.88),
                cores: Some(10),
                memory_total_bytes: 32 << 30,
                memory_used_bytes: 12 * (1 << 30) + (410 << 20),
                uptime_seconds: 6 * 86_400 + 3 * 3_600,
            }),
        });
    }

    app.update(Msg::Bleats {
        tail: feed_for(which),
    });

    // Opens the pane on `api` (already selected above) and stacks all
    // three filter axes: `o` once for `out`, `m` four times for `None ->
    // Trace -> Debug -> Info -> Warn`, then a regex typed into the match
    // box. `w` last, since it toggles independently of the filters and
    // this is the one fixture line worth wrapping.
    if which == Scene::Bleats {
        app.update(Msg::Key(KeyPress::Bleats));
        app.update(Msg::Key(KeyPress::StreamCycle));
        for _ in 0..4 {
            app.update(Msg::Key(KeyPress::LevelCycle));
        }
        app.update(Msg::Key(KeyPress::FilterStart));
        for typed in "/retry|jitter/".chars() {
            app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
        app.update(Msg::Key(KeyPress::WrapToggle));
    }

    // Applied while the link is still `Live`: `on_lambs` refuses once it
    // is `Lost`, the same guard `Msg::Bleats` carries.
    if matches!(which, Scene::Lambs | Scene::Frozen) {
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 2 },
            result: Ok(Response::Described(vec![
                ProcessInfo::builder(2, "api", ProcStatus::Online)
                    .pid(Some(48_219))
                    .lambs(Some(vec![
                        Lamb::new(48_220, "node"),
                        Lamb::new(48_221, "node"),
                        Lamb::new(48_222, "node"),
                    ]))
                    .build(),
            ])),
        });
    }

    // `cron` (id 4) has no pid, so the shepherd's walk never ran and
    // `lambs_for(4)` stays `None`.
    if which == Scene::LambsUnknown {
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 4 },
            result: Ok(Response::Described(vec![
                ProcessInfo::builder(4, "cron", ProcStatus::Stopped)
                    .lambs(None)
                    .build(),
            ])),
        });
    }

    // `Msg::Host` and the `SelectDown`s run before `Msg::Frozen`: the
    // reducer refuses both once frozen.
    match which {
        Scene::Retrying => {
            app.update(Msg::Retrying { attempt: 3 });
        }
        Scene::Frozen | Scene::KeymapFrozen => {
            app.update(Msg::Frozen {
                at_local: "2026-08-14 14:32:07".to_string(),
                why: super::view::fixtures::FROZEN_WHY.to_string(),
            });
            // Sent after `Msg::Frozen`, with a load average that varies
            // with `age`: the guard's refusal keeps
            // `the_frozen_frame_does_not_move_however_long_the_link_stays_gone`
            // byte-identical across ages.
            app.update(Msg::Host {
                sample: Some(HostSample {
                    load: (2.31 + age.as_secs_f64(), 4.10, 3.88),
                    cores: Some(10),
                    memory_total_bytes: 32 << 30,
                    memory_used_bytes: 12 * (1 << 30) + (410 << 20),
                    uptime_seconds: 6 * 86_400 + 3 * 3_600,
                }),
            });
        }
        Scene::Refused => {
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        }
        _ => {}
    }

    // The last tick, `age` after the opening snapshot: advances the
    // uptime column on a live scene, and does nothing on the frozen one,
    // since the reducer stops accepting `now` once the link is lost.
    app.update(Msg::Tick { now: t0 + age });

    // Applied after the last tick: `scene()` renders at `age` = 600s past
    // `CONFIRM_EXPIRY` (10s), so an armed confirm built before the tick
    // would already show expired.
    match which {
        Scene::ActionRefusedOffline => {
            // The link must stop being live before the key is pressed, or
            // `arm` would accept it.
            app.update(Msg::Retrying { attempt: 3 });
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        }
        Scene::Confirm | Scene::Acting | Scene::ActionRefused | Scene::ActionAccepted => {
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
            if which != Scene::Confirm {
                app.update(Msg::Key(KeyPress::Confirm));
            }
            if which == Scene::ActionAccepted {
                app.update(Msg::Replied {
                    sent: Sent::Action {
                        verb: ActionVerb::Restart,
                        target: RowKey::Sheep(2),
                        name: "api".to_string(),
                    },
                    result: Ok(Response::Restarted {
                        accepted: vec![restarted_api()],
                        refused: Vec::new(),
                    }),
                });
            }
            if which == Scene::ActionRefused {
                // The sheep leaves the flock while the request is out, which
                // is what makes the daemon's own sentence the true one.
                app.update(Msg::Snapshot {
                    rows: flock_without_api(),
                    at: t0,
                });
                app.update(Msg::Replied {
                    sent: Sent::Action {
                        verb: ActionVerb::Restart,
                        target: RowKey::Sheep(2),
                        name: "api".to_string(),
                    },
                    result: Err(RequestError::Rpc(RpcError {
                        code: RpcErrorCode::NotFound,
                        message: "selector matched no registered sheep".to_string(),
                        daemon_version: None,
                    })),
                });
            }
        }
        _ => {}
    }

    // Applied last, for the same reason: `SettingsConfirm` and `Secrets`
    // each arm a candidate that expires (on `CONFIRM_EXPIRY` or
    // `REVEAL_HOLDS`), so both must be armed after the tick at `age`.
    match which {
        Scene::Secrets => {
            // Opens the secrets pane on `production`: an operator row
            // revealed, a provider group, and a row with a slot
            // elsewhere but not here. `Msg::Revealed` is handed the
            // value directly; the reducer only checks the gate below.
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
                            byte_len: Some("hunter2-not-really".len()),
                            readers: vec![Reader {
                                name: "catcher".to_string(),
                                environment: "production".to_string(),
                                online: true,
                            }],
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
                            key: "vercel/API_TOKEN".to_string(),
                            source: Source::Namespace("vercel".to_string()),
                            in_force: Some("production".to_string()),
                            set_in: vec!["production".to_string()],
                            byte_len: Some(6),
                            readers: Vec::new(),
                        },
                    ],
                    // Every row's `readers` comes off the muster roll, and
                    // so does this: a model carrying one without the other
                    // is a state `secrets::model` cannot produce, and the
                    // frame would name a reader under a line saying no roll
                    // was ever written.
                    roll_age: Some(Duration::from_secs(184)),
                    allow_read: true,
                    ..SecretsModel::default()
                })),
            });
            app.update(Msg::Key(KeyPress::Reveal));
            app.update(Msg::Revealed {
                key: "DB_PASSWORD".to_string(),
                environment: "production".to_string(),
                value: Some(RevealedValue("hunter2-not-really".to_string())),
            });
        }
        Scene::SettingsFresh => {
            // A fresh document, not a hand-edited snapshot: first run
            // leaves only `[interpreters]`, and `load_settings` is the
            // same reader `run_ui` calls.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("shep.toml");
            ShepToml::edit(&path, ShepToml::write_starter_interpreters).unwrap();
            let snapshot = load_settings(
                &path,
                std::path::Path::new("/home/ada/.shep/run/shep.sock"),
                (StyleLevel::Full, StyleSource::Default),
            )
            .unwrap();
            app.update(Msg::Key(KeyPress::Settings));
            app.update(Msg::Settings {
                result: Ok(snapshot),
            });
        }
        Scene::SettingsSet => {
            app.update(Msg::Key(KeyPress::Settings));
            app.update(Msg::Settings {
                result: Ok(settings_snapshot_for_gallery()),
            });
        }
        Scene::SettingsConfirm => {
            app.update(Msg::Key(KeyPress::Settings));
            app.update(Msg::Settings {
                result: Ok(settings_snapshot_for_gallery()),
            });
            // The cursor already sits on `log_level`, `Settings::rows`'s
            // first row, so arming it needs no `SelectDown` at all.
            app.update(Msg::Key(KeyPress::Cycle));
        }
        Scene::SettingsTyping => {
            app.update(Msg::Key(KeyPress::Settings));
            app.update(Msg::Settings {
                result: Ok(settings_snapshot_for_gallery()),
            });
            move_settings_cursor_to(&mut app, SettingField::Socket);
            // Opens the editor, seeded with the on-disk value.
            app.update(Msg::Key(KeyPress::Confirm));
            // Trims the seeded value back to a partial path, so the frame
            // shows the editor genuinely mid-type rather than holding the
            // whole, untouched value it opened with.
            for _ in 0..8 {
                app.update(Msg::Key(KeyPress::TextBackspace));
            }
        }
        Scene::SettingsDogs | Scene::SettingsNarrow => {
            app.update(Msg::Key(KeyPress::Settings));
            app.update(Msg::Settings {
                result: Ok(settings_snapshot_with_dog_drift()),
            });
        }
        Scene::SettingsShort => {
            app.update(Msg::Key(KeyPress::Settings));
            app.update(Msg::Settings {
                result: Ok(settings_snapshot_with_dog_drift()),
            });
            // Onto the last row, which is the one a body this short cannot
            // reach without scrolling.
            app.update(Msg::Key(KeyPress::SelectLast));
        }
        Scene::EditPane
        | Scene::EditPaneEdited
        | Scene::EditPaneSqueezed
        | Scene::EditPaneNarrow => {
            // `e` on the selected row (`api`, id 2), the way `ask_for_config`
            // reaches it, then the shepherd's own reply.
            app.update(Msg::Key(KeyPress::Edit));
            app.update(Msg::Replied {
                sent: Sent::SheepConfig {
                    name: "api".to_string(),
                },
                result: Ok(Response::SheepConfig(Box::new(edit_pane_config_view()))),
            });
            // Two edits, driven by real key presses the way
            // `select_field` always is: `cwd`, which needs a respawn, and
            // `max_memory`, which lands at once, so the pending section
            // and the title's own count both have something to show.
            if which == Scene::EditPaneEdited {
                for (key, typed) in [("cwd", "/srv/api"), ("max_memory", "256")] {
                    select_field(&mut app, key);
                    app.update(Msg::Key(KeyPress::Confirm));
                    for character in typed.chars() {
                        app.update(Msg::Key(KeyPress::TextChar(character)));
                    }
                    app.update(Msg::Key(KeyPress::TextApply));
                }
            }
        }
        Scene::CloseDialog
        | Scene::CloseDialogFloor
        | Scene::CloseDialogNarrow
        | Scene::CloseDialogParked => {
            // The same `e` on `api` (id 2) that opens every edit-pane
            // scene, but replied with a view that already carries one
            // parked field, `listen_timeout`, so the close dialog has
            // something to say about the parked half without any edit of
            // the operator's own.
            app.update(Msg::Key(KeyPress::Edit));
            app.update(Msg::Replied {
                sent: Sent::SheepConfig {
                    name: "api".to_string(),
                },
                result: Ok(Response::SheepConfig(Box::new(close_dialog_config_view()))),
            });
            // `cwd` and `err_file` both need a respawn, so the dialog's
            // heading names an unsent count alongside the parked one.
            // `CloseDialogParked` files neither, relying on the fixture's
            // own parked field alone.
            if which != Scene::CloseDialogParked {
                for (key, typed) in [("cwd", "/srv/api"), ("err_file", "/var/log/api/err.log")] {
                    select_field(&mut app, key);
                    app.update(Msg::Key(KeyPress::Confirm));
                    for character in typed.chars() {
                        app.update(Msg::Key(KeyPress::TextChar(character)));
                    }
                    app.update(Msg::Key(KeyPress::TextApply));
                }
            }
            // Raises the dialog: `esc` stops writing on its own and asks
            // first, which is the whole point of this frame.
            app.update(Msg::Key(KeyPress::Escape));
        }
        _ => {}
    }

    let (width, height) = which.size();
    // The same call `run_ui` makes before every draw. Without it
    // `Viewport::rows` stays zero, which means unlimited, so a guard on a
    // scrolled screen never triggers.
    app.note_body_rows(body_rows(Rect::new(0, 0, width, height)));
    app.note_body_width(width);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(&app, frame)).unwrap();
    terminal.backend().buffer().clone()
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
    use super::super::keymap::Group;
    use super::*;

    /// The table row for `name`, or `None` if the table does not draw one.
    ///
    /// Strips the leading `>` selection marker first, so a marked and
    /// unmarked row share the same token index.
    ///
    /// The numeric-id guard on token 0 is load bearing: the status bar's
    /// own lines also open `{verb} {name} (id {id})`, so without it this
    /// could match the bar line instead of a table row.
    #[cfg_attr(windows, allow(dead_code))]
    fn row_for<'a>(frame: &'a str, name: &str) -> Option<&'a str> {
        frame.lines().find(|line| {
            let mut tokens = line.trim_start_matches('>').split_whitespace();
            tokens.next().is_some_and(|id| id.parse::<u32>().is_ok()) && tokens.next() == Some(name)
        })
    }

    /// [`coloured_palette`] always paints a ground, so every pinned
    /// snapshot's selected row is a painted gutter rather than a `>` glyph
    /// ([`super::super::view::flock::gutter`]).
    fn gallery_ground() -> ratatui::style::Color {
        coloured_palette()
            .ground()
            .bg
            .expect("the gallery's coloured palette always paints a ground")
    }

    /// Whether row `y` of `buffer` is the selected one: its gutter cell
    /// (column 0) carries [`gallery_ground`].
    fn row_is_selected(buffer: &Buffer, y: u16) -> bool {
        buffer
            .cell((0, y))
            .is_some_and(|cell| cell.bg == gallery_ground())
    }

    /// The rendered text of whichever row of `buffer` is selected, or
    /// `None` if none is (a section header, for instance, is never
    /// selected).
    #[cfg_attr(windows, allow(dead_code))]
    fn selected_line<'a>(text: &'a str, buffer: &Buffer) -> Option<&'a str> {
        (0..buffer.area.height)
            .find(|&y| row_is_selected(buffer, y))
            .and_then(|y| text.lines().nth(usize::from(y)))
    }

    /// How many rows of `buffer`, EXCLUDING the status bar, are painted as
    /// selected: a scene invariant is exactly one, the same invariant a `>`
    /// glyph count used to check.
    ///
    /// The status bar is always the buffer's last row and now carries the
    /// same [`gallery_ground`] the selected row's gutter does
    /// (`Palette::ground`, painted by `view::status` in this task); it is
    /// chrome, not a candidate row, so it is excluded rather than counted.
    fn selected_row_count(buffer: &Buffer) -> usize {
        (0..buffer.area.height.saturating_sub(1))
            .filter(|&y| row_is_selected(buffer, y))
            .count()
    }

    /// The dogs table's own row lookup: unlike [`row_for`], a dog row opens
    /// with `mark` and a name, never a numeric id, so the same "first two
    /// tokens" shape does not apply.
    fn dog_row_for<'a>(frame: &'a str, name: &str) -> Option<&'a str> {
        frame
            .lines()
            .find(|line| line.trim_start_matches('>').split_whitespace().next() == Some(name))
    }

    /// Whether the selected row's name starts with `prefix`.
    ///
    /// Handles truncation: the NAME column's truncated string depends on
    /// terminal width, so `prefix` only needs to fit the eight-column
    /// floor `name_width` never shrinks below. Selection is read off
    /// `buffer`'s own painted gutter ([`row_is_selected`]), not a `>`
    /// glyph the gallery's palette no longer draws.
    #[cfg_attr(windows, allow(dead_code))]
    fn marked_row_name_starts_with(text: &str, buffer: &Buffer, prefix: &str) -> bool {
        selected_line(text, buffer).is_some_and(|line| {
            line.trim_start_matches('>')
                .split_whitespace()
                .nth(1)
                .is_some_and(|name| name.starts_with(prefix))
        })
    }

    #[test]
    /// Every [`Scene`] gets a block here except two, checked by walking
    /// `Scene::ALL` against this function's own body rather than trusted
    /// from memory: `Folds`, which has no assertion anywhere pinning what
    /// its caption claims beyond the `.snap` file, and `Secrets`, which
    /// does but in its own dedicated test,
    /// `the_secrets_scene_shows_a_revealed_row_not_a_mask`, below, rather
    /// than duplicated here.
    ///
    /// `cfg(unix)`: one fixture carries a synthetic signalled exit, and
    /// `signal_label` resolves it against the running platform's table.
    /// Windows never sets a signal on `ExitOutcome`, so this arm only
    /// runs against a synthetic fixture like this one; the pinned
    /// artifacts under `docs/lookout/` are unix renderings for the same
    /// reason.
    #[cfg(unix)]
    #[allow(clippy::too_many_lines)] // one assertion block per scene covered, each pinning its own caption clause
    fn every_scene_shows_the_thing_it_is_named_for() {
        // HealthyWide: all three panes at 120x30.
        let wide_buffer = scene(Scene::HealthyWide).1;
        let wide = render_text(&wide_buffer);
        assert!(
            wide.contains("FOLD") && wide.contains("EXIT"),
            "every column fits at 120 columns"
        );
        assert!(
            wide.contains("host  load  ██░░░░░░░░ 2.31 4.10 3.88 / 10 cores"),
            "the host strip"
        );
        assert!(
            wide.contains("SHEEP 2  api"),
            "the detail pane, on the selected sheep, behind its own chip"
        );
        assert!(wide.contains("BLEATS api"), "and the feed, on the same one");
        assert!(
            wide.contains("2.0K on disk"),
            "api's log row points at real, fixed-size fixture files, so the size renders: {wide:?}"
        );
        // `coloured_palette` paints a real ground, so the selected row's
        // gutter is a painted space rather than a `>` glyph
        // (`view::flock::gutter`); checked on the buffer's own background
        // rather than the rendered text, which cannot see one. Two rows
        // carry it, not one: the selected row's gutter, and the status bar
        // painted at the foot of the pane.
        let ground = coloured_palette()
            .ground()
            .bg
            .expect("the gallery's coloured palette always paints a ground");
        let painted_rows = (0..wide_buffer.area.height)
            .filter(|&y| {
                wide_buffer
                    .cell((0, y))
                    .is_some_and(|cell| cell.bg == ground)
            })
            .count();
        assert_eq!(
            painted_rows, 2,
            "the selected row's gutter and the status bar"
        );

        // Grouped: cursor on the group header, which rolls up restarts,
        // CPU and memory and takes the shortest uptime.
        let grouped_buffer = scene(Scene::Grouped).1;
        let grouped = render_text(&grouped_buffer);
        assert!(
            grouped.contains("web \u{d7}3"),
            "the group header names the app and how many instances it has: {grouped:?}"
        );
        assert_eq!(
            grouped
                .lines()
                .filter(|line| line.contains("web \u{d7}3"))
                .count(),
            2,
            "one in the table, one in the detail pane, and nowhere else"
        );
        assert!(
            selected_line(&grouped, &grouped_buffer)
                .is_some_and(|line| line.contains("web \u{d7}3")),
            "the cursor is on the header, not on one of its slots: {grouped:?}"
        );
        assert!(
            row_for(&grouped, "api").is_some(),
            "an ungrouped app sits beside the grouped one: {grouped:?}"
        );
        let rollup = grouped
            .lines()
            .find(|line| line.starts_with("app web "))
            .expect("the detail pane's rollup line");
        // Summed restarts, CPU and memory; uptime is the minimum (300s
        // plus the 600s this frame renders at), not the oldest member's.
        //
        // 9.5% is the three instances' differenced `cpu_ms` readings
        // (3.5 + 2.9 + 3.1) summed, not `ProcessInfo::cpu_percent`: see
        // `poll_twice`'s own doc for why this scene has anything to
        // difference at all.
        assert!(rollup.contains("restarts 3"), "summed restarts: {rollup:?}");
        assert!(rollup.contains("cpu 9.5%"), "summed cpu: {rollup:?}");
        assert!(rollup.contains("mem 540.0M"), "summed memory: {rollup:?}");
        assert!(rollup.contains("uptime 15m"), "the shortest: {rollup:?}");
        assert!(
            !rollup.contains("2h 40m"),
            "not the longest, which is what a max or a first-member read would show: {rollup:?}"
        );
        assert!(
            grouped.contains("lambs  not shown for a group; select one instance"),
            "the detail pane says lambs are per-instance: {grouped:?}"
        );
        assert!(
            grouped.contains("BLEATS web  follows one instance; select one to see its log"),
            "and the feed will not guess which instance to tail: {grouped:?}"
        );
        assert!(
            !grouped.contains("GET /healthz 200 3ms"),
            "with no instance's lines under that sentence: {grouped:?}"
        );

        // WithDogs: the flock table's two sections, sheep then dogs.
        let with_dogs_buffer = scene(Scene::WithDogs).1;
        let with_dogs = render_text(&with_dogs_buffer);
        assert!(
            with_dogs.contains("FLOCK") && with_dogs.contains("DOGS"),
            "both section bands are drawn: {with_dogs:?}"
        );
        assert!(
            row_for(&with_dogs, "bark").is_some_and(|row| row.contains("online")),
            "the built-in dog is healthy: {with_dogs:?}"
        );
        assert!(
            row_for(&with_dogs, "log-rotate").is_some_and(|row| row.contains("silent")),
            "the adopted dog has never handshaken, so it reads silent: {with_dogs:?}"
        );
        assert!(
            marked_row_name_starts_with(&with_dogs, &with_dogs_buffer, "log-rotate"),
            "the cursor is parked on the silent dog: {with_dogs:?}"
        );
        assert!(
            with_dogs.contains("dog adopted"),
            "the detail pane names it adopted, not built-in: {with_dogs:?}"
        );

        // MemCeiling: the MEM/CEIL gauge's three states in one frame.
        let ceiling_buffer = scene(Scene::MemCeiling).1;
        let ceiling = render_text(&ceiling_buffer);
        assert!(
            row_for(&ceiling, "batch-wor…").is_some_and(|row| row.contains("░░░░░░░░░░")),
            "no ceiling configured draws an empty bar: {ceiling:?}"
        );
        assert!(
            row_for(&ceiling, "web-headr…").is_some_and(|row| row.contains("███░░░░░░░")),
            "a quarter of the ceiling fills three of ten cells: {ceiling:?}"
        );
        assert!(
            row_for(&ceiling, "web-hot").is_some_and(|row| row.contains("█████████░")),
            "94 percent of the ceiling fills nine of ten cells: {ceiling:?}"
        );
        assert!(
            marked_row_name_starts_with(&ceiling, &ceiling_buffer, "web-hot"),
            "the cursor is parked on the row at 94 percent: {ceiling:?}"
        );

        // CfgDrift: the CFG column's two markers, plus a CPU history long
        // enough to draw a shape rather than one bar.
        let drift_buffer = scene(Scene::CfgDrift).1;
        let drift = render_text(&drift_buffer);
        assert!(
            row_for(&drift, "web").is_some_and(|row| row.contains("!2")),
            "two fields parked for the next spawn: {drift:?}"
        );
        assert!(
            row_for(&drift, "api").is_some_and(|row| row.contains("*1")),
            "one field an operator set outside the Flockfile: {drift:?}"
        );
        assert!(
            row_for(&drift, "cron").is_some_and(|row| row.contains(" - ")),
            "neither pending nor overridden: {drift:?}"
        );
        let web_row = row_for(&drift, "web").expect("web's own row");
        let spark: std::collections::HashSet<char> = web_row
            .chars()
            .filter(|glyph| ('\u{2581}'..='\u{2588}').contains(glyph))
            .collect();
        assert!(
            spark.len() > 1,
            "ten distinct CPU samples draw more than one sparkline step, not a single repeated bar: {web_row:?}"
        );
        assert!(
            drift.contains("cfg !2 pending"),
            "the detail pane's own cell, for the selected sheep: {drift:?}"
        );

        // Empty: each of the three panes gives its own reason.
        let empty = render_text(&scene(Scene::Empty).1);
        assert!(
            empty.contains("the flock is empty"),
            "the table's own sentence"
        );
        assert!(
            empty.contains("no sheep selected: the flock is empty"),
            "the detail pane's"
        );
        assert!(empty.contains("BLEATS no sheep is selected"), "the feed's");
        // The summary sits after both host readings now, which puts it past
        // the cut at this scene's 100 columns. Asserted as absent rather
        // than dropped: this is the visible cost of grouping the machine's
        // two numbers together, and a later reorder that brings it back
        // should have to come through here and say so. The empty-flock
        // behaviour it used to check is pinned at the unit level, in
        // `host::tests::a_flock_with_no_readings_shows_a_dash_and_not_a_zero`.
        assert!(
            !empty.contains("errored"),
            "the summary is past a 100-column cut once host memory precedes it"
        );

        // Narrow: 51 columns drops FOLD, EXIT, RESTARTS, PID and MEM but
        // keeps CPU and UPTIME.
        let narrow = render_text(&scene(Scene::Narrow).1);
        assert!(narrow.contains("CPU") && narrow.contains("UPTIME"));
        for gone in ["FOLD", "EXIT", "RESTARTS", "PID", "MEM"] {
            assert!(!narrow.contains(gone), "the narrow tier dropped {gone}");
        }
        assert!(narrow.contains("host  load"), "the strip is up at 14 rows");
        assert!(!narrow.contains("BLEATS"), "the feed is not");
        assert!(
            !narrow.contains("SHEEP 0  "),
            "and neither is the detail pane"
        );

        // TooNarrow: below the floor, refuses rather than overlapping.
        let too_narrow = render_text(&scene(Scene::TooNarrow).1);
        let mut lines = too_narrow.lines();
        assert_eq!(lines.next().unwrap().trim_end(), "too small");
        assert_eq!(lines.next().unwrap().trim_end(), "need 33x6");

        // FeedGap: dropped lines and never-read bytes, counted separately.
        let gap = render_text(&scene(Scene::FeedGap).1);
        assert!(
            gap.contains("earlier lines not shown"),
            "the lines it dropped"
        );
        assert!(gap.contains("3.8M"), "the exact figure, not a vague one");
        assert!(gap.contains("never read"), "and what it never looked at");
        assert!(
            !gap.contains("re-read with each listing"),
            "the gap replaces the header"
        );

        // FeedMissing: names the cause rather than sitting blank.
        let missing = render_text(&scene(Scene::FeedMissing).1);
        assert!(missing.contains("has not written a log in this $SHEP_HOME"));

        // NoDetail: the detail pane is the first to go at 20 rows.
        let no_detail = render_text(&scene(Scene::NoDetail).1);
        assert!(
            no_detail.contains("BLEATS api"),
            "the feed stayed, on the selection"
        );
        assert!(no_detail.contains("host  load"), "and so did the strip");
        // The log-path prefix is the detail pane's alone: the feed's body
        // lines are tagged `out  ` too, but carry log text, not a path.
        assert!(
            !no_detail.contains("out  /home/ada/.shep/logs/"),
            "the detail pane went"
        );

        // TableOnly: 12 rows, no optional panes.
        let table_only = render_text(&scene(Scene::TableOnly).1);
        assert!(!table_only.contains("host  load"));
        assert!(!table_only.contains("BLEATS"));
        assert!(table_only.contains("STATUS"), "the table is still there");

        // Cramped: 33 columns, the narrowest terminal that draws.
        let cramped = render_text(&scene(Scene::Cramped).1);
        assert!(cramped.contains('…'), "something truncated, visibly");
        // Not a row-width check, which `render_text` satisfies trivially:
        // "nothing overlaps" means each pane's marker appears exactly once.
        // `contains`, not `starts_with`: the `BLEATS` chip now leads that
        // row.
        for marker in ["host  ", "BLEATS"] {
            assert_eq!(
                cramped.lines().filter(|line| line.contains(marker)).count(),
                1,
                "{marker:?} appears once at 33 columns"
            );
        }
        // The detail pane's merged log row carries no chip of its own, so
        // `starts_with` still works, but the path itself can truncate away
        // at 33 columns; the divider is what survives to identify the row
        // instead.
        assert_eq!(
            cramped
                .lines()
                .filter(|line| line.starts_with("out  ") && line.contains('\u{2502}'))
                .count(),
            1,
            "the detail pane's merged log row appears once at 33 columns"
        );
        assert!(
            cramped.lines().last().unwrap().contains("control enabled"),
            "and the status bar is still the last row"
        );

        // Retrying: every pane keeps describing the last listing.
        let retrying = render_text(&scene(Scene::Retrying).1);
        assert!(retrying.contains("reconnecting"));
        assert!(
            retrying.contains("SHEEP 2  api"),
            "the detail pane is still up"
        );
        assert!(retrying.contains("host  load"), "and so is the strip");

        // Frozen: last known values stay, nothing keeps ticking, and the
        // panel that replaced the two lower panes says what happened.
        let frozen = render_text(&scene(Scene::Frozen).1);
        assert!(frozen.contains("THE SHEPHERD HAS DIED"));
        assert!(
            frozen.contains("host  load  ██░░░░░░░░ 2.31 4.10 3.88 / 10 cores"),
            "the strip kept its LAST values rather than blanking"
        );
        assert!(
            frozen.contains("FROZEN"),
            "UPTIME renamed over a column that stopped advancing"
        );
        assert!(
            frozen.contains(
                "refused   the shepherd did not answer: could not connect to `/home/ada/.shep/run/shep.sock`: Connection refused (os error 61)"
            ),
            "the link panel quotes the last dial's own error, whole"
        );
        assert!(
            frozen.contains("█ 250ms  █ 500ms  █ 1s  █ 2s  █ 4s"),
            "and draws every rung the ladder climbed"
        );
        // The design's own copy offers `r` in both places. `r` is refused
        // once the link is lost, and `run_link` has already returned, so
        // the frame must not name it anywhere.
        assert!(
            frozen.contains("shep muster, from another shell"),
            "the panel sends the operator somewhere that works"
        );
        assert!(
            !frozen.contains("dials again") && !frozen.contains("retry the link"),
            "and offers no key a freeze has already refused"
        );

        // Errored: selection parked on the errored sheep.
        let errored_buffer = scene(Scene::Errored).1;
        let errored = render_text(&errored_buffer);
        assert!(errored.contains("errored"));
        assert!(
            errored.contains("SHEEP 2  api"),
            "the selection is on the errored sheep"
        );
        assert_eq!(
            selected_row_count(&errored_buffer),
            1,
            "exactly one row's gutter is painted, on that row"
        );
        // Only the ANSI rendering carries colour to check STATUS against.
        //
        // Asserted per row, not on the whole frame: `errored.contains("1")`
        // would pass on any digit anywhere, blank column included.
        let row_of = |name: &str| {
            errored
                .lines()
                .find(|line| line.contains(name))
                .unwrap_or_else(|| panic!("no row for {name}:\n{errored}"))
                .to_string()
        };
        for (name, want) in [
            ("api", "1"),
            // NAME truncates at this width, landing one syllable earlier.
            ("billing-r", "1"),
            ("cron", "SIGTERM"),
        ] {
            let row = row_of(name);
            assert!(
                row.contains(want),
                "{name}'s EXIT cell must read {want}, not a dash: {row}"
            );
        }
        assert!(
            row_of("metrics").contains(" -   "),
            "a running sheep has no exit to report: {}",
            row_of("metrics")
        );

        let errored_ansi = render_ansi(&scene(Scene::Errored).1);
        assert!(
            errored_ansi.contains("\u{1b}[38;5;29monline"),
            "online's STATUS cell gets meadow"
        );
        assert!(
            errored_ansi.contains("\u{1b}[38;5;166merrored"),
            "errored's STATUS cell gets bark"
        );
        assert!(
            errored_ansi.contains("\u{1b}[38;5;221mwaiting-restart"),
            "waiting-restart's STATUS cell gets butter"
        );
        assert!(
            errored_ansi.contains("\u{1b}[38;5;245mID"),
            "the header row is muted grey, the same token stopped's STATUS uses"
        );
        assert!(
            errored_ansi.contains("\u{1b}[38;5;245mstopped"),
            "stopped's STATUS cell shares the chrome's muted grey rather than standing out"
        );

        // Refused: `x` with actions gated off.
        let refused = render_text(&scene(Scene::Refused).1);
        assert!(refused.contains("--read-only"), "{refused}");
        assert!(
            refused.contains("BLEATS api"),
            "a refusal does not blank the screen"
        );

        // FilterEditing: mid-type, table already narrowed.
        let editing = render_text(&scene(Scene::FilterEditing).1);
        assert_eq!(
            editing
                .lines()
                .filter(|line| line.contains("  web  "))
                .count(),
            2,
            "two rows survived the query"
        );
        assert!(!editing.contains("billing"), "and the rest did not");
        assert!(editing.contains("2 of 6 in the flock"), "got {editing:?}");
        assert!(
            editing.contains("filter  web\u{258f}"),
            "the query and the cursor"
        );
        for named in ["enter applies", "esc cancels", "ctrl-c quits"] {
            assert!(editing.contains(named), "the box names {named}");
        }

        // FilterActive: the same query, box closed.
        let active = render_text(&scene(Scene::FilterActive).1);
        assert!(active.contains("filter \"web\""), "the box is closed");
        assert!(!active.contains("enter applies"), "and its keys are gone");
        assert!(active.contains("2 of 6 in the flock"), "still narrowed");
        assert!(active.contains("/ edit") && active.contains("esc clear"));

        // FilterNoMatch: names the query rather than claiming empty.
        let none = render_text(&scene(Scene::FilterNoMatch).1);
        assert!(none.contains("no sheep's name contains \"zzz\""));
        assert!(!none.contains("the flock is empty"));
        assert!(none.contains("0 of 6 in the flock"));

        // HostUnknown: the strip keeps the flock's own totals.
        let unknown = render_text(&scene(Scene::HostUnknown).1);
        assert!(unknown.contains("host  usage is not available on this platform"));
        assert!(
            unknown.contains("flock cpu"),
            "the half lookout can compute survives"
        );

        // Lambs: the stamp sits before the list.
        let lambs = render_text(&scene(Scene::Lambs).1);
        assert!(lambs.contains("lambs  3 parent-pid descendants, read "));
        assert!(lambs.contains("48220 node"), "each lamb's pid and name");
        let line = lambs
            .lines()
            .find(|line| line.starts_with("lambs  "))
            .expect("the lamb line");
        assert!(
            line.find("read ").unwrap() < line.find("48220").unwrap(),
            "the stamp comes before the list"
        );

        // LambsUnknown: no pid to walk from, unset rather than empty.
        let unknown = render_text(&scene(Scene::LambsUnknown).1);
        assert!(unknown.contains("lambs  this sheep is not running, so there is no tree to walk"));
        assert!(
            !unknown.contains("none found"),
            "which is the other sentence"
        );
        assert!(unknown.contains("SHEEP 4  cron"), "on the stopped sheep");

        // Confirm: `R` pressed, nothing sent yet.
        let confirm = render_text(&scene(Scene::Confirm).1);
        assert!(confirm.contains("restart api (id 2)? enter confirms, any other key cancels"));
        assert!(confirm.contains("control enabled"), "the gate is open");
        assert!(
            row_for(&confirm, "api").is_some_and(|row| row.contains("online")),
            "nothing was sent, so api is still online: {confirm:?}"
        );

        // Acting: request out, table unchanged.
        let acting_buffer = scene(Scene::Acting).1;
        let acting = render_text(&acting_buffer);
        assert!(acting.contains("restart api (id 2): sent, waiting for the shepherd"));
        assert!(
            selected_line(&acting, &acting_buffer).is_some_and(|line| line.contains("api")),
            "the table is untouched: the selection is still on api"
        );
        assert!(
            row_for(&acting, "api").is_some_and(|row| row.contains("online")),
            "and the row still says what the shepherd last said"
        );

        // ActionAccepted: the reply's own row reaches the table at once.
        let accepted = render_text(&scene(Scene::ActionAccepted).1);
        assert!(accepted.contains("restart api (id 2): the shepherd restarted it"));
        assert!(
            row_for(&accepted, "api").is_some_and(|row| row.contains("48299")),
            "the reply's own row reached the table without waiting for a poll"
        );

        // ActionRefused: the shepherd's own sentence is forwarded as is.
        let action_refused_buffer = scene(Scene::ActionRefused).1;
        let refused = render_text(&action_refused_buffer);
        assert!(refused.contains("restart api (id 2): selector matched no registered sheep"));
        assert!(
            !refused.contains("NotFound"),
            "no Rust identifiers on the bar"
        );
        assert!(refused.contains("5 in the flock"), "one row shorter");
        assert!(
            row_for(&refused, "api").is_none(),
            "api is the row that went"
        );
        assert!(
            marked_row_name_starts_with(&refused, &action_refused_buffer, "billing"),
            "and the cursor has moved to the row below: {refused:?}"
        );

        // ActionRefusedOffline: names the same reconnect attempt as the
        // banner above it, not the exhausted-ladder sentence.
        let offline = render_text(&scene(Scene::ActionRefusedOffline).1);
        assert_eq!(
            offline.matches("reconnecting (attempt 3)").count(),
            2,
            "the banner and the refusal under it agree, rather than one \
             saying reconnecting and the other saying gone: {offline:?}"
        );
        assert!(
            !offline.contains("nothing left to ask"),
            "the ladder has not run out yet, so the refusal must not claim it has: {offline:?}"
        );

        // The left bar slot is empty here, same as on every ordinary
        // dashboard scene, so this bar carries the plain control hint.
        let lambs_bar = render_text(&scene(Scene::Lambs).1);
        for key in ["x stop", "R restart", "L reload"] {
            assert!(lambs_bar.contains(key), "the control hint names {key}");
        }

        // SettingsFresh: a bare `shep.toml`, every scalar reads the default.
        let fresh = render_text(&scene(Scene::SettingsFresh).1);
        assert_eq!(
            fresh.matches("the default").count(),
            6,
            "all six scalars read the default: {fresh:?}"
        );
        assert!(
            !fresh.contains("shep.toml  "),
            "a fresh home has declared nothing: {fresh:?}"
        );

        // SettingsSet: `shep.toml` and the default sit side by side.
        let set = render_text(&scene(Scene::SettingsSet).1);
        assert!(set.contains("shep.toml"), "some scalars are declared");
        assert!(set.contains("the default"), "and some are not: {set:?}");

        // SettingsConfirm: names the env var and flag it cannot see.
        let confirm = render_text(&scene(Scene::SettingsConfirm).1);
        assert!(confirm.contains("shep daemon reload"), "got: {confirm:?}");
        assert!(confirm.contains("SHEP_LOG_LEVEL"), "got: {confirm:?}");
        assert!(confirm.contains("--log-level"), "got: {confirm:?}");

        // SettingsTyping: names the field being typed, not the filter box.
        let typing = render_text(&scene(Scene::SettingsTyping).1);
        assert!(
            typing.contains("editing socket"),
            "names the field being typed: {typing:?}"
        );
        assert!(
            !typing.contains("filter "),
            "must not read as the dashboard's own filter box: {typing:?}"
        );

        // SettingsDogs: the drift the table exists to reveal.
        let dogs = render_text(&scene(Scene::SettingsDogs).1);
        assert!(
            dog_row_for(&dogs, "otel")
                .is_some_and(|row| row.contains("no") && row.contains("online")),
            "otel: disabled in the file, running: {dogs:?}"
        );
        assert!(
            dog_row_for(&dogs, "ledger")
                .is_some_and(|row| row.contains("yes") && row.contains("not running")),
            "ledger: enabled, absent from the flock: {dogs:?}"
        );
        assert!(
            dog_row_for(&dogs, "bark")
                .is_some_and(|row| row.contains("yes") && row.contains("silent")),
            "bark: enabled, running, never handshook: {dogs:?}"
        );

        // SettingsNarrow: both tables drop a column rather than clip.
        let narrow = render_text(&scene(Scene::SettingsNarrow).1);
        assert!(
            narrow.contains("shep.toml"),
            "the scalar rows keep SOURCE: {narrow:?}"
        );
        assert!(
            !narrow.contains("needs shep daemon reload"),
            "and lose the apply cost: {narrow:?}"
        );
        assert!(
            dog_row_for(&narrow, "otel").is_some_and(|row| row.contains("online")),
            "the dogs table keeps RUNNING: {narrow:?}"
        );
        assert!(!narrow.contains("built-in"), "and loses SOURCE: {narrow:?}");

        // "The same screen at 14 rows, which is fewer than it has to draw.
        //  The cursor is on the last dog, so the view has scrolled to reach
        //  it and `... 5 above` says how much is off the top. The scroll is
        //  counted in lines rather than in rows: a section header and the
        //  dogs caption cost the same height a row does."
        let short = render_text(&scene(Scene::SettingsShort).1);
        assert!(
            short.contains("... 5 above"),
            "the marker names how many rows are off the top: {short:?}"
        );
        assert!(
            short
                .lines()
                .any(|line| line.starts_with("> bark")
                    || line.starts_with('>') && line.contains("bark")),
            "the cursor's own row is drawn: {short:?}"
        );
        assert!(
            !short.contains("log_level"),
            "and the rows above it are the ones that went: {short:?}"
        );
        assert!(
            short.contains("[style]") && short.contains("[dogs]"),
            "what survives is whole sections, headers and all: {short:?}"
        );

        // Bleats: all three axes stacked, wrapping on.
        let bleats = render_text(&scene(Scene::Bleats).1);
        for chip in ["stream out", "level ≥ warn", "match /retry|jitter/ (regex)"] {
            assert!(
                bleats.contains(chip),
                "the filter row names {chip}: {bleats:?}"
            );
        }
        assert!(
            // The sentence itself is longer than this 100-column frame, so
            // it fits and truncates with an ellipsis: this is the prefix
            // that survives the cut.
            bleats.contains("1 of 16 lines in the window: all three m…"),
            "the survivor count and the start of the composition sentence: {bleats:?}"
        );
        assert!(
            bleats.contains("out  WARN retrying upstream payment gateway"),
            "the one surviving line, tagged by its stream: {bleats:?}"
        );
        assert!(
            bleats
                .lines()
                .any(|line| line.starts_with("     ith jitter")),
            "wrapping is on, so the line's tail lands on its own row rather than truncating: {bleats:?}"
        );
        assert!(
            bleats.contains("esc back") && bleats.contains("\u{2588} following"),
            "the full key line, and the pane is still following the tail: {bleats:?}"
        );

        // SheepPane: both charts, the config column, and the feed, all at
        // 160x48.
        let sheep_pane = render_text(&scene(Scene::SheepPane).1);
        assert!(
            sheep_pane.contains("\u{2588}\u{2588} CPU")
                && sheep_pane.contains("\u{2588}\u{2588} MEM"),
            "both charts draw: {sheep_pane:?}"
        );
        assert!(
            sheep_pane.contains("\u{2588}\u{2588} CONFIG & ENV") && sheep_pane.contains("script"),
            "the config column lists web's own fields: {sheep_pane:?}"
        );
        assert!(
            sheep_pane.contains("\u{2588}\u{2588} BLEATS"),
            "the embedded feed carries its own header: {sheep_pane:?}"
        );

        // SheepPaneCpuOnly: 139 columns, the CPU chart alone plus a
        // one-line memory summary.
        let cpu_only = render_text(&scene(Scene::SheepPaneCpuOnly).1);
        assert!(
            cpu_only.contains("\u{2588}\u{2588} CPU"),
            "the CPU chart still draws: {cpu_only:?}"
        );
        assert!(
            !cpu_only.contains("\u{2588}\u{2588} MEM"),
            "the memory chart is gone: {cpu_only:?}"
        );
        assert!(
            cpu_only.contains("rss "),
            "replaced by a one-line rss summary: {cpu_only:?}"
        );

        // SheepPaneSparklines: 99 columns, 1a's own pair on one row.
        let sparklines = render_text(&scene(Scene::SheepPaneSparklines).1);
        assert!(
            !sparklines.contains("\u{2588}\u{2588} CPU")
                && !sparklines.contains("\u{2588}\u{2588} MEM"),
            "both charts are gone: {sparklines:?}"
        );
        assert!(
            sparklines.contains("CPU 20s") && sparklines.contains("MEM/CEIL"),
            "1a's own pair draws instead: {sparklines:?}"
        );

        // SheepPaneShort: 160x25, the memory chart gone, the CPU chart and
        // the config column still up.
        let short_pane = render_text(&scene(Scene::SheepPaneShort).1);
        assert!(
            short_pane.contains("\u{2588}\u{2588} CPU"),
            "the CPU chart still draws: {short_pane:?}"
        );
        assert!(
            !short_pane.contains("\u{2588}\u{2588} MEM"),
            "the memory chart is gone under 26 rows: {short_pane:?}"
        );
        assert!(
            short_pane.contains("\u{2588}\u{2588} CONFIG & ENV"),
            "the config column, which gives ground last, is still up: {short_pane:?}"
        );

        // EditPane: fresh, no edits filed, 160x48. LANDS draws in the
        // header and the explanation panel is on screen, and the title
        // band carries no edit count.
        let edit_pane = render_text(&scene(Scene::EditPane).1);
        let title = edit_pane
            .lines()
            .find(|line| line.contains("(sheep config)"))
            .expect("the title band");
        assert!(
            !title.contains("edit"),
            "fresh, so the title names no edit count: {title:?}"
        );
        let header = edit_pane
            .lines()
            .find(|line| line.contains("FIELD") && line.contains("VALUE"))
            .expect("the field list header");
        assert!(
            header.contains("LANDS"),
            "wide enough for the cost column: {header:?}"
        );
        assert!(
            edit_pane.contains("FOCUSED"),
            "the explanation panel names the focused field: {edit_pane:?}"
        );

        // EditPaneEdited: the same pane with cwd and max_memory filed. The
        // title counts them and the pending section lists both.
        let edit_pane_edited = render_text(&scene(Scene::EditPaneEdited).1);
        let edited_title = edit_pane_edited
            .lines()
            .find(|line| line.contains("(sheep config)"))
            .expect("the title band");
        assert!(
            edited_title.contains("2 edits"),
            "two edits filed: {edited_title:?}"
        );
        let pending_section = edit_pane_edited
            .lines()
            .skip_while(|line| !line.trim_start().starts_with("pending edits"))
            .take(3)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            pending_section.contains("cwd") && pending_section.contains("max_memory"),
            "both filed edits are named under the pending edits section: {pending_section:?}"
        );

        // EditPaneSqueezed: 120 columns. The panel still draws; LANDS
        // gives way to it, so the header carries FIELD and VALUE but not
        // LANDS.
        let squeezed = render_text(&scene(Scene::EditPaneSqueezed).1);
        let squeezed_header = squeezed
            .lines()
            .find(|line| line.contains("FIELD") && line.contains("VALUE"))
            .expect("the field list header");
        assert!(
            !squeezed_header.contains("LANDS"),
            "LANDS gives way to the panel at 120 columns: {squeezed_header:?}"
        );
        assert!(
            squeezed.contains("FOCUSED"),
            "the panel still draws at 120 columns: {squeezed:?}"
        );

        // EditPaneNarrow: 88 columns. The panel is gone, so LANDS returns.
        let narrow_edit = render_text(&scene(Scene::EditPaneNarrow).1);
        let narrow_edit_header = narrow_edit
            .lines()
            .find(|line| line.contains("FIELD") && line.contains("VALUE"))
            .expect("the field list header");
        assert!(
            narrow_edit_header.contains("LANDS"),
            "nothing else carries cost at 88 columns, so LANDS is back: {narrow_edit_header:?}"
        );
        assert!(
            !narrow_edit.contains("FOCUSED"),
            "the panel does not draw at 88 columns: {narrow_edit:?}"
        );

        // CloseDialog: 160x48, boxed. The heading, the first row inside
        // the border, names both halves: two edits needing a respawn and
        // one field already parked.
        let close_dialog = render_text(&scene(Scene::CloseDialog).1);
        let close_dialog_lines: Vec<&str> = close_dialog.lines().collect();
        let border_row = close_dialog_lines
            .iter()
            .position(|line| line.contains('▛')) // BOX_TOP_LEFT
            .expect("the box border draws at 160 columns");
        let heading = close_dialog_lines[border_row + 1];
        assert!(
            heading.contains("EDITS NEED A RESPAWN") && heading.contains("FIELD ALREADY"),
            "the heading names both the unsent and the parked half: {heading:?}"
        );

        // CloseDialogFloor: 90x48, exactly the width the border needs.
        // Read off the border's own row, the way both heading assertions
        // here do: a frame-wide `contains` passes on the glyph turning up
        // anywhere, and it is only this dialog that draws one today.
        let close_dialog_floor = render_text(&scene(Scene::CloseDialogFloor).1);
        let floor_lines: Vec<&str> = close_dialog_floor.lines().collect();
        let floor_border_row = floor_lines
            .iter()
            .position(|line| line.contains('▛')) // BOX_TOP_LEFT
            .expect("the box border draws at the floor");
        assert!(
            floor_lines[floor_border_row].contains('▜'), // BOX_TOP_RIGHT
            "the border's top row closes at the floor: {:?}",
            floor_lines[floor_border_row]
        );

        // CloseDialogNarrow: 89x48, one column under the floor. No box
        // glyph anywhere in the frame; this is the assertion a wrong
        // overlay::floor_for(BOX_WIDTH) would fail.
        let close_dialog_narrow = render_text(&scene(Scene::CloseDialogNarrow).1);
        for glyph in ['▛', '▜', '▙', '▟', '▐', '▀', '▄', '▌'] {
            assert!(
                !close_dialog_narrow.contains(glyph),
                "no border glyph {glyph:?} draws one column under the floor: {close_dialog_narrow:?}"
            );
        }

        // CloseDialogParked: 160x48, no edit of the operator's own. The
        // heading names only the parked half, with no unsent count.
        let close_dialog_parked = render_text(&scene(Scene::CloseDialogParked).1);
        let parked_lines: Vec<&str> = close_dialog_parked.lines().collect();
        let parked_border_row = parked_lines
            .iter()
            .position(|line| line.contains('▛'))
            .expect("the box border draws at 160 columns");
        let parked_heading = parked_lines[parked_border_row + 1];
        assert!(
            parked_heading.contains("FIELD ALREADY") && !parked_heading.contains("EDIT"),
            "the heading names the parked half alone, with no unsent count: {parked_heading:?}"
        );
    }

    /// Two grouped apps, four sheep, six visible rows: a `0..=flock_len()`
    /// budget only reaches index 4, short of the last row at index 5. One
    /// group would not show this, since its header lands the budget
    /// exactly on the last row.
    #[test]
    fn the_cursor_walk_budgets_by_visible_rows_not_by_sheep() {
        let t0 = Instant::now();
        let mut app = App::new(
            coloured_palette(),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                instance(0, "web", 0, 0, 3.4, 182 << 20, 4_512_000),
                instance(1, "web", 1, 0, 2.9, 178 << 20, 4_512_000),
                instance(2, "api", 0, 0, 7.1, 241 << 20, 4_512_000),
                instance(3, "api", 1, 0, 6.8, 239 << 20, 4_512_000),
            ],
            at: t0,
        });
        assert_eq!(
            app.visible_rows().len(),
            7,
            "the flock header, four sheep and two group headers"
        );

        // `web`'s second slot: the last visible row, and the one a
        // sheep-counted budget cannot reach.
        select_id(&mut app, 1);
        assert_eq!(app.selected(), Some(RowKey::Sheep(1)));
    }

    /// A pinned width whose own arithmetic no longer holds drops a column
    /// in silence, and this is the scene whose whole point is what the
    /// columns look like once the shepherd is gone.
    ///
    /// The floor is `ALL`'s own tier threshold plus the gutter, not the
    /// design's 160: the frame is drawn twelve columns wider than it has
    /// to be, and pinning the wider number would fail for a scene that was
    /// still drawing everything.
    #[test]
    fn the_frozen_scene_draws_every_column() {
        use super::super::view::flock::{GUTTER, columns_for};

        let (width, _) = Scene::Frozen.size();
        assert_eq!(
            columns_for(width - GUTTER).len(),
            columns_for(u16::MAX).len(),
            "the frozen scene is {width} columns, which drops a column from the widest set"
        );
    }

    /// Rule 3 of the design system: strip every colour and the frame still
    /// reads. Its corollary on this one frame is stronger — no cell above
    /// the link panel may carry a colour at all, since a meadow `online`
    /// two seconds after the shepherd died is the one lie this screen can
    /// tell.
    #[test]
    fn no_cell_above_the_link_panel_is_painted_live() {
        let palette = coloured_palette();
        let buffer = scene(Scene::Frozen).1;
        let muted = palette.muted().fg;
        let line = palette.line().fg;
        // Row 0 is the band, which is bark and says so; the link panel
        // below carries the ladder's own bark blocks and a butter `r`.
        let panel_at = render_text(&buffer)
            .lines()
            .position(|row| row.contains("THE LINK"))
            .expect("the link panel is on a frozen frame");
        let panel_at = u16::try_from(panel_at).expect("a row index fits");
        for y in 1..panel_at {
            for x in 0..buffer.area.width {
                let cell = &buffer[(buffer.area.x + x, buffer.area.y + y)];
                assert!(
                    cell.fg == Color::Reset || Some(cell.fg) == muted || Some(cell.fg) == line,
                    "a live-looking cell at {x},{y}: {:?} in {:?}",
                    cell.symbol(),
                    cell.fg
                );
            }
        }
    }

    /// Rendered twice at two different ages and compared to each other,
    /// not to the healthy scene, since a live-versus-frozen diff would
    /// pass either way. The live pair at the bottom catches a renderer
    /// that drops the uptime column entirely.
    ///
    /// Split at the link panel's own chip rather than at a row index. Every
    /// value above it came from the shepherd and stopped when the shepherd
    /// did; the panel below it describes the link, and how long ago the
    /// link died is a fact about now. One half must not move and the other
    /// must, so the test asserts both rather than narrowing to the half
    /// that is easier to pin.
    #[test]
    fn a_frozen_frame_moves_only_in_the_link_panel() {
        let ten_minutes = render_text(&scene_with(
            Scene::Frozen,
            Duration::from_secs(600),
            coloured_palette(),
        ));
        let sixteen_hours = render_text(&scene_with(
            Scene::Frozen,
            Duration::from_secs(60_000),
            coloured_palette(),
        ));
        let above_the_panel = |frame: &str| {
            frame
                .split("THE LINK")
                .next()
                .expect("split yields at least one part")
                .to_string()
        };
        assert_eq!(
            above_the_panel(&ten_minutes),
            above_the_panel(&sixteen_hours),
            "the frozen frame's uptime column advanced after the link was lost"
        );
        assert_ne!(
            ten_minutes, sixteen_hours,
            "the link panel's age is the one number a frozen frame still counts"
        );
        // Seven seconds short of each age, and deliberately: `scene_with`
        // ticks the clock forward by that much before it freezes anything,
        // so the freeze happens at `t0 + 7s` and the panel counts from
        // there. `9m 53s`, not `593s` — the same two-unit shape every other
        // duration on the screen uses.
        assert!(
            ten_minutes.contains("2026-08-14 14:32:07, 9m 53s ago"),
            "and it counts in the same words every other duration uses"
        );
        assert!(
            sixteen_hours.contains("2026-08-14 14:32:07, 16h 39m ago"),
            "at both ends of the sweep"
        );
        assert!(
            above_the_panel(&ten_minutes).contains("1h 18m"),
            "the frozen frame has an uptime cell for the comparison above to cover"
        );

        let live_ten = render_text(&scene_with(
            Scene::HealthyWide,
            Duration::from_secs(600),
            coloured_palette(),
        ));
        let live_sixteen = render_text(&scene_with(
            Scene::HealthyWide,
            Duration::from_secs(60_000),
            coloured_palette(),
        ));
        assert_ne!(
            live_ten, live_sixteen,
            "a LIVE frame's uptime column must advance, or the assertion above passes for the wrong reason"
        );
    }

    /// Each width scene draws the column count it exists to show, counted
    /// off the heading row's own occupied starts rather than inferred from
    /// a word being present. A test that only looked for `MOVING` would
    /// pass at every width in the table, since `MOVING` draws at every
    /// width the overlay ever renders.
    #[test]
    fn each_keymap_width_scene_draws_its_own_column_count() {
        for (which, wanted) in [
            (Scene::Keymap, 4),
            (Scene::KeymapFloor, 4),
            (Scene::KeymapBorderlessWide, 4),
            (Scene::KeymapNarrow, 3),
            (Scene::KeymapTwoColumn, 2),
        ] {
            let rendered = render_text(&scene(which).1);
            let first_bank = rendered
                .lines()
                // `Group::Moving.heading()`, not the literal "MOVING": the
                // count below derives from `heading()`, so a rename would
                // leave this `find` hunting a word nothing draws and the
                // failure would read as a missing heading row rather than as
                // a rename.
                .find(|row| row.contains(Group::Moving.heading()))
                .expect("no heading row");
            let drawn = Group::DRAWN
                .iter()
                .filter(|group| first_bank.contains(group.heading()))
                .count();
            assert_eq!(drawn, wanted, "{}: {first_bank:?}", which.label());
        }
    }

    /// Every `Keymap*` scene actually raises the overlay.
    ///
    /// `scene`'s builder raises it from a `matches!` list of variants, and
    /// the comment over that list says "every `Keymap*` scene". Nothing held
    /// that claim. The per-scene checks in this module are a hand-written
    /// list, `each_keymap_width_scene_draws_its_own_column_count` names five
    /// of the eight, and a snapshot is accepted for whatever a scene
    /// rendered, so a ninth keymap scene left out of the `matches!` list
    /// would draw the dashboard and be pinned that way.
    ///
    /// Driven off the label rather than a list, so a new scene joins this
    /// test by being named. Dropping `KeymapNarrow` from the builder's list
    /// fails it, and so does adding a scene the list does not cover.
    ///
    /// The count is the other half: it catches a keymap scene whose label
    /// does not start with `keymap`, which this test would otherwise skip in
    /// silence, and it is the same eight the gallery preamble promises.
    #[test]
    fn every_keymap_scene_raises_the_overlay() {
        let mut checked = 0;
        for which in Scene::ALL {
            if !which.label().starts_with("keymap") {
                continue;
            }
            checked += 1;
            let rendered = render_text(&scene(*which).1);
            assert!(
                rendered.contains(Group::Moving.heading()),
                "{}: the overlay did not draw",
                which.label()
            );
        }
        assert_eq!(checked, 8, "the keymap scenes, counted by label");
    }

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

    /// `Scene::Secrets` claims a revealed row in its own doc, caption and
    /// the hand-copied frame in `web/`. Assert the frame actually shows
    /// the plaintext and a countdown, not a mask: a snapshot alone would
    /// pass on an expired reveal, since nothing names what "revealed"
    /// means.
    #[test]
    fn the_secrets_scene_shows_a_revealed_row_not_a_mask() {
        let text = render_text(&scene(Scene::Secrets).1);
        let revealed = text
            .lines()
            .find(|line| line.contains("DB_PASSWORD"))
            .expect("the secrets scene draws a DB_PASSWORD row");

        assert!(
            revealed.contains("hunter2-not-really"),
            "DB_PASSWORD's row should show the revealed plaintext, not a mask: {revealed:?}"
        );
        assert!(
            !revealed.contains("bytes"),
            "the VALUE cell should show plaintext, not a masked byte count: {revealed:?}"
        );
        assert!(
            revealed.contains("visible"),
            "a revealed row should show a countdown, not the unrevealed dash: {revealed:?}"
        );
    }

    /// `secrets::model` reads `readers` and `roll_age` off the same muster
    /// roll, so a frame naming a reader and then saying no roll exists is a
    /// state the loader cannot produce. The snapshot pins the frame against
    /// its own committed copy, so the contradiction stays green there and
    /// reaches `docs/lookout/` and the published page.
    #[test]
    fn the_secrets_scene_does_not_deny_the_roll_its_readers_came_from() {
        let text = render_text(&scene(Scene::Secrets).1);
        let read_by = text
            .lines()
            .find(|line| line.contains("DB_PASSWORD"))
            .expect("the secrets scene draws a DB_PASSWORD row");

        assert!(
            read_by.contains("1 (1 online)"),
            "the scene's own row names a reader: {read_by:?}"
        );
        assert!(
            !text.contains("no muster roll yet"),
            "and so cannot also say the roll it came from was never written"
        );
        assert!(
            text.contains("READ BY as of the roll"),
            "it says how old the roll is instead"
        );
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
