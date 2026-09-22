//! Every rendered payload type in the binary, and the [`super::Render`] impl
//! that
//! makes each one's table and JSON renderings one source of truth.
//!
//! They live here rather than under `commands/` because nothing here carries
//! a `cfg`, so a test on the Windows leg can name every one. Split by the
//! payload each group renders: [`process`] for a sheep's own
//! `ProcessInfo`/`Lamb` rows, [`dogs`] for the dog-specific ones,
//! [`toolkit`] for the paint/cell rules both share, [`lifecycle`] for
//! one-shot verb results, [`replies`] for a daemon action's reply,
//! [`secrets`] for the KV store and `shep secret`.

mod dogs;
mod lifecycle;
mod process;
mod replies;
mod secrets;
mod toolkit;

pub use dogs::*;
pub use lifecycle::*;
pub use process::*;
pub use replies::*;
pub use secrets::*;
pub(crate) use toolkit::*;

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeSet;

    use shep_core::protocol::{ActionOutcome, ActionReply, DogSource, ExitInfo, ProcessInfo};
    use shep_core::status::ProcStatus;

    use crate::style::Presentation;
    use crate::vocabulary::Role;

    use super::super::Render;
    use super::process::tests::full_presentation;
    use super::*;

    pub(crate) fn sample_info(id: u32, name: &str, uptime_ms: u64) -> ProcessInfo {
        // Every `Option` field `Some`: `assert_no_drift` skips a `null`, so a
        // field left empty here is a column it stops watching. `dog` is the
        // exception, since every row here is a sheep.
        ProcessInfo::builder(id, name, ProcStatus::Online)
            .pid(Some(1000 + id))
            .restarts(id)
            .uptime_ms(uptime_ms)
            .fold(Some("backend".to_string()))
            .out_file(Some(format!("/logs/{name}-0-out.log")))
            .err_file(Some(format!("/logs/{name}-0-err.log")))
            // Not a round number of MiB, so `human_bytes` renders "48.1M".
            .cpu_percent(Some(12.5))
            .memory_bytes(Some(50_462_720))
            // Populated for the "every `Option` field `Some`" reason above;
            // every row here is running, so the cell reads `-` regardless.
            .last_exit(Some(ExitInfo {
                code: Some(1),
                signal: None,
            }))
            // The literal a real dog paints.
            .smit(Some("\u{25b2} main@a1b2c3".to_string()))
            // `cfg_cell` shows `pending` over `overridden` when both are set,
            // so this fixture cannot exercise `overridden`'s cell text; it is
            // `JSON_ONLY` anyway.
            .pending(Some(vec!["env".to_string()]))
            .overridden(Some(vec!["cwd".to_string()]))
            .build()
    }

    /// Three fully-populated sheep, shared by every test in this module and
    /// by `output`'s own envelope/emit tests.
    pub(crate) fn sample_flock() -> FlockRows {
        FlockRows(vec![
            sample_info(1, "web", 60_000),
            sample_info(2, "worker", 120_000),
            sample_info(3, "cron", 30_000),
        ])
    }

    pub(crate) fn info_with_uptime_ms(uptime_ms: u64) -> ProcessInfo {
        sample_info(1, "web", uptime_ms)
    }

    /// A dog-shaped `ProcessInfo`: `sample_info` with `dog` set to `source`.
    pub(crate) fn dog_info(name: &str, source: DogSource) -> ProcessInfo {
        let mut info = sample_info(1, name, 60_000);
        info.dog = Some(source);
        info
    }

    /// The anti-drift gate, once per payload type with JSON object keys
    /// (`DeletedIds` has none; see its own test below).
    ///
    /// Checks a fully-populated value's JSON keys against `headers()` after
    /// `json_key_for`, every row's cell count against `headers().len()`, and
    /// each non-`formatted` cell against its own JSON value, which is what
    /// catches two same-arity cells swapped.
    ///
    /// `formatted` lists headers whose cell is a human rendering rather than
    /// the field's raw value.
    pub(crate) fn assert_no_drift<T: Render>(
        value: &T,
        first_record: fn(&serde_json::Value) -> &serde_json::Value,
        formatted: &[&str],
    ) {
        let json = serde_json::to_value(value).unwrap();
        let record = first_record(&json);
        let keys: BTreeSet<&str> = record
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();

        let covered: BTreeSet<&str> = T::headers()
            .iter()
            .map(|h| T::json_key_for(h))
            .chain(T::JSON_ONLY.iter().copied())
            .collect();

        assert_eq!(
            keys, covered,
            "a serialized field is a column, or it is in JSON_ONLY with a reason — never neither"
        );

        let rows = value.rows();
        for row in &rows {
            assert_eq!(
                row.len(),
                T::headers().len(),
                "a row has {} cells but headers() has {} — a dropped or added cell changes no \
                 row *count*, so table_and_json_report_the_same_record_count would miss it",
                row.len(),
                T::headers().len(),
            );
        }

        let Some(row) = rows.first() else {
            return;
        };
        for (i, header) in T::headers().iter().enumerate() {
            if formatted.contains(header) {
                continue;
            }
            let key = T::json_key_for(header);
            let expected = match &record[key] {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                // A `None`-carrying fixture is skipped, not a failure.
                serde_json::Value::Null => continue,
                other => panic!(
                    "{header} ({key}) serialized to {other:?}; teach this match how to \
                     stringify it, or add {header} to `formatted`"
                ),
            };
            assert_eq!(
                row[i], expected,
                "{header} cell does not match its own JSON field {key:?} — swapped or \
                 substituted with a neighbouring column?"
            );
        }
    }

    #[test]
    fn table_and_json_report_the_same_record_count() {
        let rows = sample_flock(); // three sheep
        let json = serde_json::to_value(&rows).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 3);
        assert_eq!(
            rows.rows().len(),
            3,
            "the two renderings must never disagree on how many records exist"
        );

        let ids = DeletedIds(vec![1, 2, 3, 4]);
        assert_eq!(
            serde_json::to_value(&ids)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            4
        );
        assert_eq!(ids.rows().len(), 4);
    }

    /// One `Render` impl's own check. `headers()` and `PRIORITIES` are
    /// hand-edited parallel arrays, so a header inserted without its priority
    /// shifts every later one onto the wrong column. `floor` is this type's
    /// intended never-drop set.
    fn assert_priorities_match_headers<T: Render>(floor: &[&str]) {
        let headers = T::headers();
        let priorities = T::PRIORITIES;
        assert_eq!(
            headers.len(),
            priorities.len(),
            "{}: headers() has {} columns but PRIORITIES has {} — they must move together",
            std::any::type_name::<T>(),
            headers.len(),
            priorities.len(),
        );
        let actual_floor: Vec<&str> = headers
            .iter()
            .zip(priorities)
            .filter(|&(_, &p)| p == 0)
            .map(|(&h, _)| h)
            .collect();
        assert_eq!(
            actual_floor,
            floor,
            "{}: the columns at priority 0 do not match this type's own intended floor",
            std::any::type_name::<T>(),
        );
    }

    /// The anti-drift gate for [`Render::PRIORITIES`] across every payload
    /// type this crate defines, so a table added later without a real array
    /// fails here instead of shipping the trait's all-zero default.
    #[test]
    fn priorities_line_up_with_headers_for_every_render_impl() {
        assert_priorities_match_headers::<FlockRows>(&["ID", "NAME", "STATUS"]);
        assert_priorities_match_headers::<DogRows>(&["ID", "NAME", "STATUS"]);
        assert_priorities_match_headers::<LambRows>(&["PID", "NAME"]);
        assert_priorities_match_headers::<DogsRow>(&["NAME", "STATUS"]);
        assert_priorities_match_headers::<FlushedRows>(&["ID", "NAME"]);
        assert_priorities_match_headers::<EmptiedFiles>(&["STREAM", "RESULT"]);
        assert_priorities_match_headers::<DeletedIds>(&["ID"]);
        assert_priorities_match_headers::<KillRow>(&["PID", "SOCKET_REMOVED"]);
        assert_priorities_match_headers::<RolledSheepRows>(&["NAME", "STATUS"]);
        assert_priorities_match_headers::<SavedRollRow>(&["FILE", "APPS"]);
        assert_priorities_match_headers::<ImportRows>(&["NAME"]);
        assert_priorities_match_headers::<ImportEnvRows>(&["KEY", "STORE"]);
        assert_priorities_match_headers::<StartupSteps>(&["TARGET", "RESULT"]);
        assert_priorities_match_headers::<TriggeredRows>(&["ID", "NAME", "OUTCOME"]);
        assert_priorities_match_headers::<SignalledRows>(&["ID", "NAME", "OUTCOME"]);
        assert_priorities_match_headers::<SentLineRows>(&["ID", "NAME", "OUTCOME"]);
        assert_priorities_match_headers::<BarkRows>(&["WHEN", "RULE", "SUBJECT"]);
        assert_priorities_match_headers::<KvRows>(&["KEY", "VALUE"]);
        assert_priorities_match_headers::<KvUnsetRow>(&["REMOVED"]);
        assert_priorities_match_headers::<AvailableDogRows>(&["NAME", "PACKAGE"]);
        assert_priorities_match_headers::<SecretKeyRows>(&["KEY", "ENVIRONMENTS"]);
        assert_priorities_match_headers::<SecretSlotRow>(&["KEY", "ENVIRONMENT"]);
        assert_priorities_match_headers::<SecretValueRow>(&["KEY", "VALUE"]);
    }

    /// The 256-colour presentation these cases render at.
    pub(crate) fn coloured() -> Presentation {
        use crate::style::StyleLevel;
        Presentation::new(
            StyleLevel::Full,
            None,
            Some(std::ffi::OsStr::new("xterm-256color")),
            None,
            200,
        )
    }

    /// `text` as `colour_cell` would paint it for `role`.
    pub(crate) fn painted(text: &str, role: Role) -> String {
        let mut cell = text.to_string();
        colour_cell(&mut cell, role, coloured());
        cell
    }

    /// One dog whose readings land on a known side of every ramp: four
    /// restarts, idle CPU, and 3 MiB, below the MEM boundary.
    pub(crate) fn sample_dog(status: ProcStatus, pid: Option<u32>) -> ProcessInfo {
        ProcessInfo::builder(9, "log-rotate", status)
            .pid(pid)
            .restarts(4)
            .uptime_ms(41_000)
            .cpu_percent(pid.map(|_| 0.0))
            .memory_bytes(pid.map(|_| 3 * 1024 * 1024))
            .dog(Some(DogSource::Adopted {
                path: "/usr/local/bin/shep-log-rotate".to_string(),
            }))
            .build()
    }

    #[test]
    fn the_sheep_and_dog_tables_share_a_column_order() {
        let sheep = FlockRows::headers();
        let dogs = DogRows::headers();

        // CFG is a sheep concept: a dog is never loaded from a Flockfile a
        // config load can park or override, so it is filtered out before the
        // shared prefix is compared.
        let sheep_without_cfg: Vec<&str> = sheep.iter().copied().filter(|h| *h != "CFG").collect();
        assert_eq!(
            sheep.iter().position(|h| *h == "CFG"),
            sheep.iter().position(|h| *h == "EXIT").map(|at| at + 1),
            "CFG sits directly after EXIT in the sheep table"
        );

        let common = [
            "ID", "NAME", "STATUS", "PID", "RESTARTS", "EXIT", "CPU", "MEM", "UPTIME",
        ];
        assert_eq!(
            &sheep_without_cfg[..common.len()],
            &common,
            "the sheep table leads with them, CFG aside"
        );
        assert_eq!(&dogs[..common.len()], &common, "and so does the dogs table");

        assert_eq!(
            &sheep_without_cfg[common.len()..],
            &["FOLD", "SMIT"],
            "the sheep table's own"
        );
        assert_eq!(&dogs[common.len()..], &["SOURCE"], "the dogs table's own");

        // FOLD and SMIT are impossible for a dog rather than empty: a dog
        // belongs to no fold, and a smit is a mark a dog paints on a sheep.
        assert!(
            DogRows::JSON_ONLY.contains(&"fold") && DogRows::JSON_ONLY.contains(&"smit"),
            "both still ride the JSON, with a reason recorded beside them"
        );
    }

    /// Compares painted cells, not roles, so it also catches a table reading
    /// the right rule off the wrong column.
    #[test]
    fn a_shared_column_is_painted_the_same_in_both_tables() {
        let mut as_sheep = sample_dog(ProcStatus::Online, Some(14_110));
        as_sheep.dog = None;
        let sheep = FlockRows(vec![as_sheep]).rows_for(coloured(), true);
        let dogs =
            DogRows(vec![sample_dog(ProcStatus::Online, Some(14_110))]).rows_for(coloured(), true);

        for (index, header) in FlockRows::headers().iter().enumerate() {
            let Some(there) = DogRows::headers().iter().position(|h| h == header) else {
                continue;
            };
            assert_eq!(
                sheep[0][index], dogs[0][there],
                "{header} renders differently in the two tables"
            );
        }
    }

    /// The same reversed-header proof, pointed at every painter that is not
    /// [`process_info_paint`]: `dog_action_paint`, `reply_paint` and the four
    /// inline closures.
    #[test]
    fn every_painter_follows_the_column_name_and_not_the_position() {
        /// Paints `row` through `T`'s headers reversed, and hands the cells
        /// back in their original order.
        fn reversed<T: Render>(row: Vec<String>, paint_of: fn(&str, &str) -> Paint) -> Vec<String> {
            let backwards: Vec<&'static str> = T::headers().iter().copied().rev().collect();
            let mut cells = row;
            cells.reverse();
            let mut painted = paint(
                vec![cells],
                &backwards,
                coloured(),
                true,
                |header, cell, _index| paint_of(header, cell),
            )
            .remove(0);
            painted.reverse();
            painted
        }
        let at =
            |headers: &[&'static str], name: &str| headers.iter().position(|h| *h == name).unwrap();

        // --- the four dog-action rows, through `dog_action_paint` ---------
        let adopted = DogsRow::new(
            "log-rotate",
            DogSource::Adopted {
                path: "/usr/local/bin/shep-log-rotate".to_string(),
            },
            "online",
            true,
        );
        let cells = reversed::<DogsRow>(adopted.rows().remove(0), dog_action_paint);
        let h = DogsRow::headers();
        assert_eq!(
            cells[at(h, "SOURCE")],
            painted("adopted", Role::Butter),
            "SOURCE decided from SOURCE, wherever it sits"
        );
        assert_eq!(
            cells[at(h, "STATUS")],
            painted("(o.o) online", Role::Meadow),
            "STATUS decided from STATUS"
        );
        assert_eq!(cells[at(h, "NAME")], "log-rotate", "NAME untouched");
        assert_eq!(cells[at(h, "SHEPHERD")], "true", "SHEPHERD untouched");

        // --- the three reply tables, through `reply_paint` ----------------
        let reply = TriggeredRows(vec![ActionReply {
            id: 0,
            name: "web".to_string(),
            outcome: ActionOutcome::TimedOut,
        }]);
        let cells = reversed::<TriggeredRows>(reply.rows().remove(0), reply_paint);
        let h = TriggeredRows::headers();
        assert_eq!(cells[at(h, "ID")], painted("0", Role::Ink3));
        assert_eq!(cells[at(h, "OUTCOME")], painted("timed_out", Role::Bark));
        assert_eq!(
            cells[at(h, "DETAIL")],
            "no reply within the app's own action_timeout",
            "DETAIL untouched, and never mistaken for the OUTCOME beside it"
        );

        // --- the inline closures -----------------------------------------
        let emptied = EmptiedFiles(vec![EmptiedFile {
            stream: "stdout",
            file: "/logs/shepd.out.log".to_string(),
            result: "emptied",
        }])
        .rows_for(coloured(), true);
        assert_eq!(
            emptied[0][at(EmptiedFiles::headers(), "RESULT")],
            painted("emptied", Role::Meadow)
        );

        let steps = StartupSteps(vec![StartupStep {
            action: "ran",
            target: "launchctl load".to_string(),
            result: "permission denied".to_string(),
        }])
        .rows_for(coloured(), true);
        assert_eq!(
            steps[0][at(StartupSteps::headers(), "RESULT")],
            painted("permission denied", Role::Bark),
            "an unrecognised RESULT is the failure line"
        );
    }

    /// The two rollups are not shared code, so cells are compared across the
    /// surfaces. Both are anchored at one instant, so the lookout's live
    /// uptime is the reported one.
    #[test]
    fn the_flock_table_and_the_lookout_roll_a_group_up_the_same_way() {
        use std::time::Instant;

        use crate::lookout::app::{App, Control, Msg, RowKey};
        use crate::lookout::theme::Palette;
        use crate::lookout::view::flock::{Column, FrameFacts, columns_for, key_line};

        // Every slot differs in every summed field, so a rollup reading one
        // member cannot coincide with the sum.
        let flock: Vec<ProcessInfo> = [
            (0_u32, 0_u32, 3.4_f32, 182_u64 << 20, 4_512_000_u64),
            (1, 2, 2.9, 178 << 20, 300_000),
            (2, 1, 3.1, 180 << 20, 9_000_000),
        ]
        .into_iter()
        .map(|(slot, restarts, cpu, memory, uptime_ms)| {
            ProcessInfo::builder(slot + 1, "web", ProcStatus::Online)
                .instance(Some(slot))
                .pid(Some(48_400 + slot))
                .restarts(restarts)
                .cpu_percent(Some(cpu))
                .memory_bytes(Some(memory))
                .uptime_ms(uptime_ms)
                .build()
        })
        .collect();

        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: flock.clone(),
            at: t0,
        });

        // The summing rules themselves, ahead of any rendering.
        let table_totals = group_totals(&flock);
        let dashboard_totals = app.group_totals("web");
        assert_eq!(dashboard_totals.count, flock.len());
        assert_eq!(dashboard_totals.restarts, table_totals.restarts, "restarts");
        // CPU is the one field these two surfaces do NOT agree on, by
        // design: `shep flock` reads `ProcessInfo::cpu_percent`, the
        // shepherd's own running mean, while lookout differences its own
        // polls and has had only one here, so it honestly has nothing yet.
        // A one-shot listing has nothing to difference against, so the two
        // surfaces answering differently is correct rather than drift.
        assert_eq!(table_totals.cpu, Some(9.4), "the table keeps cpu_percent");
        assert_eq!(
            dashboard_totals.cpu, None,
            "one poll has nothing differenced to sum yet"
        );
        assert_eq!(dashboard_totals.memory, table_totals.memory, "memory");
        assert_eq!(
            dashboard_totals.uptime_ms,
            Some(table_totals.uptime_ms),
            "uptime"
        );
        assert_eq!(
            app.group_status_text("web"),
            group_status(&flock),
            "a uniform group's status word"
        );

        // The mixed case has a format to disagree about, not one word.
        let mut mixed = flock.clone();
        mixed[1].status = ProcStatus::Stopped;
        let mut mixed_app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        mixed_app.update(Msg::Snapshot {
            rows: mixed.clone(),
            at: t0,
        });
        assert_eq!(
            mixed_app.group_status_text("web"),
            group_status(&mixed),
            "a mixed group's per-state counts"
        );

        let table = FlockRows(flock).rows_for(full_presentation(), true);
        let header = &table[0];
        let dashboard_columns = columns_for(200);
        let dashboard_line = key_line(
            &app,
            &FrameFacts::new(&app),
            &RowKey::Group("web".to_string()),
            dashboard_columns,
            200,
            false,
        );
        let dashboard = dashboard_line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        // Then the rendered cells. FOLD and SMIT are per-app facts, not sums;
        // STATUS carries a face here and not in the dashboard. CPU is not
        // in this list: `dashboard_totals.cpu` above already pins the one
        // field these two surfaces deliberately disagree on.
        for column in ["NAME", "RESTARTS", "MEM", "UPTIME"] {
            let at = FlockRows::headers()
                .iter()
                .position(|header| *header == column)
                .expect("the column is in the table");
            let cell = header[at].trim();
            assert!(
                dashboard.contains(cell),
                "`shep flock` rolls {column} up to {cell:?} and `shep lookout` \
                 does not agree: {dashboard:?}"
            );
        }

        // The CPU cell specifically, not a substring search of the whole
        // line: FOLD and SMIT are blank-instance dashes too, so `" - "`
        // shows up in the rendered row whatever the CPU cell holds. `key_line`
        // pushes one span per column (a "  " separator span between them,
        // two text spans only for `Column::MemCeil`), so walking `columns_for`
        // the same way it does finds the exact span the CPU cell landed in.
        let mut cpu_span = None;
        let mut span_index = 0;
        for (index, column) in dashboard_columns.iter().enumerate() {
            if index > 0 {
                span_index += 1; // the "  " separator span
            }
            if *column == Column::Cpu {
                cpu_span = Some(span_index);
                break;
            }
            span_index += if *column == Column::MemCeil { 2 } else { 1 };
        }
        let cpu_span = cpu_span.expect("CPU is drawn at this width");
        let cpu_cell = dashboard_line.spans[cpu_span].content.as_ref().trim();
        assert_eq!(
            cpu_cell, "-",
            "the dashboard's own CPU cell reads `-` after one poll: {dashboard:?}"
        );
    }
}
