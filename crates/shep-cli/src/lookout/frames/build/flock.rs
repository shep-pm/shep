//! The flock each scene shows: which sheep, dogs, instances and folds
//! exist before anything is drawn.

use std::time::{Duration, Instant};

use shep_core::{
    protocol::{DogSource, ProcessInfo},
    status::ProcStatus,
};

use crate::lookout::{
    app::{App, Msg},
    frames::{
        fixtures::{dog_sheep, instance, poll_twice, sheep, sheep_with_ceiling},
        scene::Scene,
    },
};

/// The rows `which` opens with, already polled where the scene needs a
/// differenced reading.
pub(super) fn build_flock(app: &mut App, which: Scene, t0: Instant) -> Vec<ProcessInfo> {
    match which {
        Scene::Empty => Vec::new(),
        // The only flock in the gallery whose rows carry a slot, and so the
        // only one that draws a group header at all. `api` is here so the
        // frame shows a grouped app beside an ungrouped one rather than
        // implying every app gets a header.
        Scene::Grouped => poll_twice(
            app,
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
            app,
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
            app,
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
        // absent" means in the settings snapshot `super::armed` applies.
        Scene::SettingsDogs | Scene::SettingsNarrow | Scene::SettingsShort => vec![
            dog_sheep(90, "otel", DogSource::BuiltIn, None),
            dog_sheep(91, "bark", DogSource::BuiltIn, Some(false)),
        ],
        // A flock's two sections at once: three sheep, a healthy built-in
        // dog and a silent adopted one.
        Scene::WithDogs => poll_twice(
            app,
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
            app,
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
                app,
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
    }
}

/// Hands `flock` to `app` the way a first listing would.
pub(super) fn initialize_flock(app: &mut App, which: Scene, flock: Vec<ProcessInfo>, t0: Instant) {
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
}

#[cfg(test)]
mod tests {
    use crate::lookout::app::{Control, RowKey};
    use crate::lookout::frames::build::probe::{
        marked_row_name_starts_with, row_for, selected_line, selected_row_count,
    };
    use crate::lookout::frames::coloured_palette;
    use crate::lookout::frames::fixtures::select_id;
    use crate::lookout::frames::render::{render_ansi, render_text};
    use crate::lookout::frames::scene;

    use super::*;

    /// Every scene built around who is in the flock shows the rows it is
    /// named for: the three panes, the group rollup, the dogs section, the
    /// memory ceiling and the config drift.
    #[test]
    #[cfg(unix)] // inherited, not measured per test: see the `build` module's docs
    fn every_flock_scene_shows_the_rows_it_is_named_for() {
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
    }

    /// The errored scene parks on the errored sheep and spells out the
    /// exit, the signal and the restart count behind it.
    #[test]
    #[cfg(unix)] // inherited, not measured per test: see the `build` module's docs
    fn the_errored_scene_shows_the_exit_it_is_named_for() {
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
}
