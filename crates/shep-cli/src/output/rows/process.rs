//! `ProcessInfo`-shaped listing rows for a sheep: `FlockRows`, `LambRows`
//! and `FlushedRows`. Dog-specific rows live in [`super::dogs`]; the
//! paint/cell toolkit both render through lives in [`super::toolkit`].

use serde::Serialize;
use shep_core::protocol::{Lamb, ProcessInfo};

use crate::output::Render;
use crate::style::Presentation;

use super::toolkit::{
    GroupTotals, group_paint, group_row, group_totals, name_groups, paint, plain_row,
    process_info_paint, slot_row,
};

/// `Vec<ProcessInfo>` for every verb whose reply carries one: `flock`,
/// `describe`, `fold`, `start`, `stop`, `restart`, `reopen`, `flush`.
///
/// A newtype for the orphan rule; `transparent`, so the JSON is a plain
/// array of `ProcessInfo`.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct FlockRows(pub Vec<ProcessInfo>);

impl Render for FlockRows {
    fn headers() -> &'static [&'static str] {
        &[
            "ID", "NAME", "STATUS", "PID", "RESTARTS", "EXIT", "CFG", "CPU", "MEM", "UPTIME",
            "FOLD", "SMIT",
        ]
    }

    /// One row per process, and the path `Bare` takes: `table_of` calls this
    /// directly when [`crate::style::StyleLevel::boxes`] is false, so the
    /// `web:0` suffix lives in [`plain_row`] as well as in
    /// [`Self::rows_for`].
    fn rows(&self) -> Vec<Vec<String>> {
        name_groups(&self.0)
            .flat_map(|group| {
                let slotted = group.len() > 1 && group.iter().all(|p| p.instance.is_some());
                group.iter().map(move |p| plain_row(p, slotted))
            })
            .collect()
    }

    /// [`Self::rows`], each cell painted by [`process_info_paint`]'s rule for
    /// its column, or [`group_paint`]'s for a header row.
    ///
    /// An app with several instances groups under one header row when
    /// [`crate::style::StyleLevel::boxes`] is true. `sort_flock` orders the
    /// listing by (name, instance, id), so instances are adjacent and one
    /// pass groups them. `Bare` never reaches this method; see [`Self::rows`].
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        /// Which payload a rendered row came from, for [`paint`]'s rule.
        enum RowSource<'a> {
            /// A slot row or a plain (ungrouped) row.
            Sheep(&'a ProcessInfo),
            /// A group's header row, plus its summed totals.
            Group(&'a [ProcessInfo], GroupTotals),
        }

        let mut out = Vec::with_capacity(self.0.len());
        let mut sources: Vec<RowSource<'_>> = Vec::with_capacity(self.0.len());
        for group in name_groups(&self.0) {
            // A slot nobody reported cannot be grouped or suffixed.
            let slotted = group.len() > 1 && group.iter().all(|p| p.instance.is_some());
            if slotted && presentation.level.boxes() {
                let totals = group_totals(group);
                out.push(group_row(group, &totals));
                sources.push(RowSource::Group(group, totals));
                for p in group {
                    out.push(slot_row(p));
                    sources.push(RowSource::Sheep(p));
                }
            } else {
                for p in group {
                    out.push(plain_row(p, slotted));
                    sources.push(RowSource::Sheep(p));
                }
            }
        }

        paint(
            out,
            Self::headers(),
            presentation,
            status_word,
            |header, _cell, index| match &sources[index] {
                RowSource::Sheep(p) => process_info_paint(header, p),
                RowSource::Group(g, totals) => group_paint(header, g, totals),
            },
        )
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "ID" => "id",
            "NAME" => "name",
            "STATUS" => "status",
            "PID" => "pid",
            "RESTARTS" => "restarts",
            "EXIT" => "last_exit",
            // `cfg_cell` folds two fields into one cell; `overridden` rides
            // in `JSON_ONLY`.
            "CFG" => "pending",
            "CPU" => "cpu_percent",
            "MEM" => "memory_bytes",
            "UPTIME" => "uptime_ms",
            "FOLD" => "fold",
            "SMIT" => "smit",
            other => panic!("FlockRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[
        // Absolute paths, wider than the rest of the table together.
        "out_file",
        "err_file",
        // Always `null`: every row here is a sheep.
        "dog",
        // Always `null`: only `Describe` walks for lambs.
        "lambs",
        // A handshake is a fact about a dog, and every row here is a sheep.
        "handshook",
        // A dog fact too: a shepherd gives up only on dogs.
        "dog_stale",
        // The table labels each slot instead; the JSON stays flat.
        "instance",
        // CFG's header maps to `pending`, so `overridden` rides here.
        "overridden",
        // No new `shep flock` column for this: it names other sheep, not
        // this row's own status, and the table already drops columns under
        // pressure.
        "depends_on",
        // MEM already reports the raw reading; a gauge against this ceiling
        // is lookout's, not this table's.
        "max_memory",
        // CPU already reports the percentage; the raw counter behind it is
        // for a client differencing its own polls, not this table.
        "cpu_ms",
        // A reload's own answer, absent from every other listing this type
        // renders. Milliseconds are for the client waiting the swap out,
        // not for a column an operator reads.
        "reload_deadline_ms",
    ];

    // Parallel to `headers()`. The rest survive in ascending order. CFG ties
    // with EXIT at `6` and yields first: `render_boxed_ex`'s `max_by_key`
    // takes the last of an equal pair, and CFG sits later in `headers()`.
    const PRIORITIES: &'static [u8] = &[0, 0, 0, 2, 4, 6, 6, 5, 3, 1, 7, 8];
}
/// One sheep's lamb tree, as `describe`'s second table.
///
/// Not `#[serde(transparent)]`: this type's JSON is never read, since
/// `describe --format json` serializes the listing as [`FlockRows`] with its
/// own `lambs`. It exists to reach
/// [`render_table`](crate::output::render_table).
#[derive(Debug, Serialize)]
pub struct LambRows(pub Vec<Lamb>);

/// No colour: both columns are identity, and a lamb has no status, reading or
/// placeholder for one to carry.
impl Render for LambRows {
    fn headers() -> &'static [&'static str] {
        &["PID", "NAME"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|lamb| vec![lamb.pid.to_string(), lamb.name.clone()])
            .collect()
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "PID" => "pid",
            "NAME" => "name",
            other => panic!("LambRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    // Parallel to `headers()`. Two columns, both identity, so this never
    // narrows; spelled out so a later header does not inherit it by omission.
    const PRIORITIES: &'static [u8] = &[0, 0];
}

/// `Response::Flushed(Vec<ProcessInfo>)`: the sheep a `shep flush` matched,
/// rendered by the files it emptied rather than by their lifecycle.
///
/// Serializes exactly as [`FlockRows`] does, over the same
/// `Vec<ProcessInfo>`, so only the table differs. `out_file`/`err_file` are
/// free-form config taken verbatim, so a mistyped one empties something that
/// is not a log.
///
/// One row per sheep: several can share a log path, and the daemon truncates
/// each distinct path once.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct FlushedRows(pub Vec<ProcessInfo>);

impl Render for FlushedRows {
    fn headers() -> &'static [&'static str] {
        &["ID", "NAME", "OUT_FILE", "ERR_FILE"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|p| {
                vec![
                    p.id.to_string(),
                    p.name.clone(),
                    // `-`: a peer daemon predating the field, never a sheep
                    // with no log file.
                    p.out_file.clone().unwrap_or_else(|| "-".to_string()),
                    p.err_file.clone().unwrap_or_else(|| "-".to_string()),
                ]
            })
            .collect()
    }

    /// [`process_info_paint`] again: ID muted, NAME plain, and both path
    /// columns left to the dash rule. A real path is the subject of this
    /// table rather than a reading about it.
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        paint(
            self.rows(),
            Self::headers(),
            presentation,
            status_word,
            |header, _cell, index| process_info_paint(header, &self.0[index]),
        )
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "ID" => "id",
            "NAME" => "name",
            "OUT_FILE" => "out_file",
            "ERR_FILE" => "err_file",
            other => panic!("FlushedRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[
        // A sheep's lifecycle and resource use, which a flush neither reads
        // nor changes. They stay in the JSON so a consumer switching on the
        // envelope's `command` does not find the record shape switching too.
        "status",
        "pid",
        "restarts",
        "uptime_ms",
        "fold",
        "cpu_percent",
        "memory_bytes",
        // The raw counter behind `cpu_percent`, same reason.
        "cpu_ms",
        // Every row is a sheep: no `dog`, no handshake, and nothing for a
        // shepherd to give up on.
        "dog",
        "handshook",
        "dog_stale",
        // Always `null`: only `Describe` walks for lambs.
        "lambs",
        // Nothing a flush reads or changes, and a column each would push
        // OUT_FILE/ERR_FILE off the side of a terminal.
        "last_exit",
        "smit",
        "instance",
        "pending",
        "overridden",
        // Nothing a flush reads or changes.
        "depends_on",
        // A ceiling is a resource reading, the same reason `memory_bytes`
        // rides here.
        "max_memory",
    ];

    // Parallel to `headers()`. ERR_FILE survives one round longer than
    // OUT_FILE: a crash is read from stderr first.
    const PRIORITIES: &'static [u8] = &[0, 0, 7, 6];
}

#[cfg(test)]
pub(crate) mod tests {
    use shep_core::protocol::ExitInfo;
    use shep_core::status::ProcStatus;

    use crate::vocabulary::Role;

    use super::super::dogs::tests::{cell_of, styled};
    use super::super::tests::{assert_no_drift, coloured, painted, sample_flock, sample_info};
    use super::*;

    #[test]
    fn flock_rows_do_not_drift() {
        // `reload_deadline_ms` on top of `sample_flock`, not in it: this
        // type renders a reload's acceptance as well as a listing, and the
        // gate can only see a key a fixture serializes. It stays off the
        // shared fixture because no other payload built from one can carry
        // it, and `skip_serializing_if` means an unset field has no key.
        let FlockRows(mut rows) = sample_flock();
        for row in &mut rows {
            row.reload_deadline_ms = Some(15_000);
        }
        // UPTIME/CPU/MEM are formatted, EXIT's JSON value is a nested object,
        // and CFG is a summary of two fields.
        assert_no_drift(
            &FlockRows(rows),
            |j| &j[0],
            &["UPTIME", "CPU", "MEM", "EXIT", "CFG"],
        );
    }

    /// `sample_flock` cannot exercise this: every row there is running, so
    /// its cell is always `-`.
    #[test]
    fn the_exit_column_shows_the_last_exit_only_for_a_sheep_that_is_not_running() {
        let headers = FlockRows::headers();
        let at = |cells: &[String], h: &str| {
            cells[headers.iter().position(|x| *x == h).unwrap()].clone()
        };

        // Never exited: no pid, no `last_exit`.
        let never_run = ProcessInfo::builder(1, "fresh", ProcStatus::Stopped).build();
        // Exited with a code: no pid, `last_exit` carries one.
        let crashed = ProcessInfo::builder(2, "crashed", ProcStatus::Errored)
            .last_exit(Some(ExitInfo {
                code: Some(1),
                signal: None,
            }))
            .build();
        // Killed by a signal: no pid, `last_exit` carries one.
        let killed = ProcessInfo::builder(3, "killed", ProcStatus::Stopped)
            .last_exit(Some(ExitInfo {
                code: None,
                signal: Some(9),
            }))
            .build();
        // Running again after a past exit: `last_exit` is sticky across a
        // respawn, but a live pid leaves this column nothing to say.
        let running_again = ProcessInfo::builder(4, "recovered", ProcStatus::Online)
            .pid(Some(4242))
            .last_exit(Some(ExitInfo {
                code: Some(1),
                signal: None,
            }))
            .build();

        let rows = FlockRows(vec![never_run, crashed, killed, running_again]).rows();
        assert_eq!(at(&rows[0], "EXIT"), "-");
        assert_eq!(at(&rows[1], "EXIT"), "1");
        #[cfg(unix)]
        assert_eq!(at(&rows[2], "EXIT"), "SIGKILL");
        // `signal_label`'s Windows arm is a bare number.
        #[cfg(not(unix))]
        assert_eq!(at(&rows[2], "EXIT"), "9");
        assert_eq!(at(&rows[3], "EXIT"), "-");
    }

    #[test]
    fn lamb_rows_do_not_drift() {
        assert_no_drift(
            &LambRows(vec![Lamb::new(4243, "node"), Lamb::new(4244, "sh")]),
            |j| &j[0],
            &[],
        );
    }

    /// A zero is a claim, "this sheep is using no CPU", and the daemon says
    /// `None` precisely when it cannot make that claim.
    #[test]
    fn a_sheep_with_no_reading_renders_a_dash_not_a_zero() {
        let mut info = sample_info(1, "web", 60_000);
        info.cpu_percent = None;
        info.memory_bytes = None;
        let rows = FlockRows(vec![info]);
        let cells = &rows.rows()[0];
        let headers = FlockRows::headers();
        let cpu = cells[headers.iter().position(|h| *h == "CPU").unwrap()].clone();
        let mem = cells[headers.iter().position(|h| *h == "MEM").unwrap()].clone();
        assert_eq!(cpu, "-");
        assert_eq!(mem, "-");
    }

    /// Boxes on, so `rows_for` takes the grouping branch, and `NO_COLOR` set
    /// so cells compare as literal text.
    pub(crate) fn full_presentation() -> Presentation {
        use crate::style::StyleLevel;
        Presentation::new(
            StyleLevel::Full,
            Some(std::ffi::OsStr::new("1")),
            None,
            None,
            200,
        )
    }

    /// Boxes off, so `rows_for` takes the flat, suffixed branch instead.
    fn bare_presentation() -> Presentation {
        use crate::style::StyleLevel;
        Presentation::new(StyleLevel::Bare, None, None, None, 200)
    }

    #[test]
    fn a_single_instance_app_is_untouched_by_grouping() {
        let rows = FlockRows(vec![
            ProcessInfo::builder(4, "api", ProcStatus::Online)
                .instance(Some(0))
                .build(),
        ]);
        let rendered = rows.rows_for(full_presentation(), true);
        assert_eq!(rendered.len(), 1, "no group row for one instance");
        assert_eq!(rendered[0][1], "api", "and no suffix");
    }

    #[test]
    fn a_multi_instance_app_gets_a_group_row_then_its_slots() {
        let rows = FlockRows(
            (0..3)
                .map(|slot| {
                    ProcessInfo::builder(slot + 1, "web", ProcStatus::Online)
                        .instance(Some(slot))
                        .build()
                })
                .collect(),
        );
        let rendered = rows.rows_for(full_presentation(), true);
        assert_eq!(rendered.len(), 4, "one group row plus three slots");
        assert_eq!(rendered[0][0], "", "the group row has no id");
        assert!(rendered[0][1].contains("web"), "{:?}", rendered[0]);
        assert!(
            rendered[0][1].contains('3'),
            "and the count: {:?}",
            rendered[0]
        );
        assert_eq!(rendered[1][0], "1", "slot rows keep their ids");
    }

    /// `BTreeMap` keys on the status word, so the order is alphabetical.
    #[test]
    fn a_mixed_group_says_so_rather_than_picking_a_winner() {
        let rows = FlockRows(vec![
            ProcessInfo::builder(1, "web", ProcStatus::Online)
                .instance(Some(0))
                .build(),
            ProcessInfo::builder(2, "web", ProcStatus::Stopped)
                .instance(Some(1))
                .build(),
            ProcessInfo::builder(3, "web", ProcStatus::Online)
                .instance(Some(2))
                .build(),
        ]);
        let rendered = rows.rows_for(full_presentation(), true);
        assert_eq!(rendered[0][2], "2 online, 1 stopped");
    }

    /// The slots are listed oldest first, so the shortest is last.
    #[test]
    fn a_group_uptime_is_the_shortest_of_its_slots() {
        let rows = FlockRows(
            [9_000_000_u64, 4_512_000, 300_000]
                .into_iter()
                .enumerate()
                .map(|(slot, uptime_ms)| {
                    let slot = u32::try_from(slot).unwrap();
                    ProcessInfo::builder(slot + 1, "web", ProcStatus::Online)
                        .instance(Some(slot))
                        .uptime_ms(uptime_ms)
                        .build()
                })
                .collect(),
        );
        let rendered = rows.rows_for(full_presentation(), true);
        assert_eq!(rendered[0][9], "5m", "300_000ms, the shortest of the three");
    }

    /// A zero is a claim, and the sum of nothing is no claim: the fold starts
    /// at `None` rather than `0`.
    #[test]
    fn a_group_with_no_readings_shows_a_dash_not_a_zero() {
        let rows = FlockRows(
            (0..2)
                .map(|slot| {
                    ProcessInfo::builder(slot + 1, "web", ProcStatus::Online)
                        .instance(Some(slot))
                        .cpu_percent(None)
                        .memory_bytes(None)
                        .build()
                })
                .collect(),
        );
        let rendered = rows.rows_for(full_presentation(), true);
        assert_eq!(rendered[0][7], "-", "cpu");
        assert_eq!(rendered[0][8], "-", "mem");

        // One live reading among absent ones is still a claim: the fold
        // leaves `None` only when no slot reported.
        let mixed = FlockRows(vec![
            ProcessInfo::builder(1, "web", ProcStatus::Online)
                .instance(Some(0))
                .cpu_percent(Some(2.5))
                .memory_bytes(Some(64 << 20))
                .build(),
            ProcessInfo::builder(2, "web", ProcStatus::Online)
                .instance(Some(1))
                .cpu_percent(None)
                .memory_bytes(None)
                .build(),
        ]);
        let rendered = mixed.rows_for(full_presentation(), true);
        assert_eq!(rendered[0][7], "2.5%", "cpu");
        assert_eq!(rendered[0][8], "64.0M", "mem");
    }

    #[test]
    fn a_flat_style_suffixes_the_name_instead_of_grouping() {
        let rows = FlockRows(
            (0..2)
                .map(|slot| {
                    ProcessInfo::builder(slot + 1, "web", ProcStatus::Online)
                        .instance(Some(slot))
                        .build()
                })
                .collect(),
        );
        let rendered = rows.rows_for(bare_presentation(), true);
        assert_eq!(rendered.len(), 2, "one line per process, still greppable");
        assert_eq!(rendered[0][1], "web:0");
        assert_eq!(rendered[1][1], "web:1");
    }

    /// The same suffix through `render_table`, which calls `Self::rows`
    /// directly. The test above asserts on `rows_for` and cannot reach it.
    #[test]
    fn the_bare_path_reaches_the_suffix_through_rows_not_rows_for() {
        let rows = FlockRows(
            (0..2)
                .map(|slot| {
                    ProcessInfo::builder(slot + 1, "web", ProcStatus::Online)
                        .instance(Some(slot))
                        .build()
                })
                .collect(),
        );
        let rendered = crate::output::render_table(&rows);
        assert!(rendered.contains("web:0"), "{rendered}");
        assert!(rendered.contains("web:1"), "{rendered}");
    }

    #[test]
    fn a_row_from_an_older_daemon_renders_exactly_as_it_did_before() {
        let rows = FlockRows(vec![
            ProcessInfo::builder(1, "web", ProcStatus::Online).build(),
            ProcessInfo::builder(2, "web", ProcStatus::Online).build(),
        ]);
        let rendered = rows.rows_for(full_presentation(), true);
        assert_eq!(rendered.len(), 2, "no slots, so no grouping");
        assert_eq!(rendered[0][1], "web", "and no suffix");
    }

    /// The lifecycle keys are off the table only because
    /// [`FlushedRows::JSON_ONLY`] names them.
    #[test]
    fn flushed_rows_do_not_drift() {
        assert_no_drift(&FlushedRows(sample_flock().0), |j| &j[0], &[]);
    }

    /// A `--format json` parser must not need a case keyed on the envelope's
    /// `command`.
    #[test]
    fn a_flush_serializes_the_same_record_the_other_flock_verbs_do() {
        let flock = serde_json::to_value(sample_flock()).unwrap();
        let flushed = serde_json::to_value(FlushedRows(sample_flock().0)).unwrap();
        assert_eq!(
            flock, flushed,
            "the table may differ between these two verbs; the JSON payload may not"
        );
    }

    /// The floor-set check cannot see two non-floor columns trading numbers.
    /// The flock listing's drop order is the one the spec states outright.
    #[test]
    fn the_flock_listing_drops_its_columns_in_the_documented_order() {
        let mut ranked: Vec<(&str, u8)> = FlockRows::headers()
            .iter()
            .copied()
            .zip(FlockRows::PRIORITIES.iter().copied())
            .collect();
        ranked.sort_by_key(|&(_, priority)| priority);

        let order: Vec<&str> = ranked.iter().map(|&(header, _)| header).collect();
        assert_eq!(
            order,
            vec![
                // The three that identify a sheep, and so never drop.
                "ID", "NAME", "STATUS", //
                // Then in the order they survive as the terminal narrows;
                // the give-up order is this reversed.
                "UPTIME", "PID", "MEM", "RESTARTS", "CPU", "EXIT", "CFG", "FOLD", "SMIT",
            ],
            "the flock listing's drop order changed; if that is deliberate, \
             change this test and say why in the commit"
        );
    }

    // --- Colour: MEM/CPU/RESTARTS/EXIT/ID/FOLD/placeholder roles ----------

    /// A path is this table's subject, so only the `-` a peer daemon
    /// predating the field produces is muted.
    #[test]
    fn a_flushed_row_mutes_its_id_and_its_dash_and_leaves_a_path_alone() {
        let mut without = sample_info(1, "cron", 0);
        without.out_file = None;
        without.err_file = None;
        let rows =
            FlushedRows(vec![sample_info(0, "web", 60_000), without]).rows_for(coloured(), true);

        assert_eq!(rows[0][0], painted("0", Role::Ink3), "ID is chrome");
        assert_eq!(rows[0][1], "web", "NAME is plain");
        assert_eq!(
            rows[0][2], "/logs/web-0-out.log",
            "a real path carries no colour"
        );
        assert_eq!(rows[1][2], painted("-", Role::Ink3), "the placeholder does");
        assert_eq!(rows[1][3], painted("-", Role::Ink3));
    }

    #[test]
    fn lamb_rows_carry_no_colour_at_all() {
        let rows = LambRows(vec![Lamb::new(48_302, "node")]).rows_for(coloured(), true);
        assert_eq!(rows[0], vec!["48302".to_string(), "node".to_string()]);
    }

    /// Through `rows_for`, since this is about which cells get touched rather
    /// than about a threshold.
    #[test]
    fn chrome_and_placeholder_columns_are_coloured_and_nothing_else_is() {
        use crate::style::{Presentation, StyleLevel};

        let presentation = Presentation::new(
            StyleLevel::Full,
            None,
            Some(std::ffi::OsStr::new("xterm-256color")),
            None,
            200,
        );
        // One row with a real PID and no fold, one with neither.
        let mut running = sample_info(0, "web", 60_000);
        running.fold = None;
        let mut stopped = sample_info(1, "cron", 0);
        stopped.pid = None;
        stopped.fold = None;
        let flock = FlockRows(vec![running, stopped]);

        let rows = flock.rows_for(presentation, true);

        // ID: chrome, always coloured.
        assert!(rows[0][0].contains('\u{1b}'), "{:?}", rows[0][0]);
        assert!(rows[1][0].contains('\u{1b}'), "{:?}", rows[1][0]);
        // PID: a real value is left plain; the placeholder is coloured.
        assert!(!rows[0][3].contains('\u{1b}'), "{:?}", rows[0][3]);
        assert!(rows[1][3].contains('\u{1b}'), "{:?}", rows[1][3]);
        // FOLD: chrome, always coloured, `-` here on both rows.
        assert!(rows[0][10].contains('\u{1b}'), "{:?}", rows[0][10]);
        assert!(rows[1][10].contains('\u{1b}'), "{:?}", rows[1][10]);
    }

    // --- A dog that has never answered the shepherd ----------------------
    //
    // `ProcessInfo::status` reports whether a process is alive; `handshook`
    // is the fact the STATUS column adds to it.

    /// A sheep's `handshook` is always `None`. Driven through a sheep
    /// carrying `Some(false)` too, which the daemon never sends.
    #[test]
    fn a_sheep_never_reads_as_silent() {
        use crate::style::StyleLevel;
        let sheep = sample_info(1, "web", 60_000);
        assert_eq!(sheep.handshook, None, "the daemon sends nothing here");
        let rows = FlockRows(vec![sheep.clone()]).rows();
        assert_eq!(cell_of::<FlockRows>(&rows[0], "STATUS"), "online");

        let mut impossible = sheep;
        impossible.handshook = Some(false);
        let full = FlockRows(vec![impossible]).rows_for(styled(StyleLevel::Full), true);
        assert_eq!(
            cell_of::<FlockRows>(&full[0], "STATUS"),
            painted("(o.o) online", Role::Meadow),
            "the sheep table has no dogs in it, and no silence rule either"
        );
    }
}
