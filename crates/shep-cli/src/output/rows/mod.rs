//! Every rendered payload type in the binary, and the [`Render`] impl that
//! makes each one's table and JSON renderings one source of truth.
//!
//! They live here rather than under `commands/` because nothing here carries
//! a `cfg`, so a test on the Windows leg can name every one. Split by the
//! payload each group renders: [`process`] for anything shaped like a
//! `ProcessInfo` or a `Lamb`, [`lifecycle`] for one-shot verb results,
//! [`replies`] for a daemon action's reply, [`secrets`] for the KV store and
//! `shep secret`.

mod lifecycle;
mod process;
mod replies;
mod secrets;

pub use lifecycle::*;
pub use process::*;
pub use replies::*;
pub use secrets::*;

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeSet;

    use shep_core::barks::{Bark, SinkOutcome};
    use shep_core::protocol::{
        ActionOutcome, ActionReply, DogSource, ExitInfo, Lamb, LineOutcome, LineReply, ProcessInfo,
        SignalOutcome, SignalReply,
    };
    use shep_core::status::ProcStatus;

    use crate::dog_index::AvailableDog;
    use crate::style::Presentation;
    use crate::vocabulary::Role;

    use super::super::Render;
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
    fn assert_no_drift<T: Render>(
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

    /// A pending field an operator cannot see is a silent divergence.
    #[test]
    fn the_cfg_cell_marks_a_sheep_with_pending_config() {
        let mut info = sample_info(1, "web", 60_000);
        info.pending = Some(vec!["env".to_string()]);
        assert_eq!(
            cfg_cell(info.pending.as_deref(), info.overridden.as_deref()),
            "!1"
        );

        let clean = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
        assert_eq!(
            cfg_cell(clean.pending.as_deref(), clean.overridden.as_deref()),
            "-"
        );
    }

    #[test]
    fn lamb_rows_do_not_drift() {
        assert_no_drift(
            &LambRows(vec![Lamb::new(4243, "node"), Lamb::new(4244, "sh")]),
            |j| &j[0],
            &[],
        );
    }

    /// A path is wider than every other column combined and would push
    /// UPTIME off a terminal. It is still one `--format json` away.
    #[test]
    fn the_source_column_names_a_kind_and_leaves_the_path_to_json() {
        let rows = DogRows(vec![
            dog_info("metrics", DogSource::BuiltIn),
            dog_info(
                "otel",
                DogSource::Adopted {
                    path: "/usr/local/bin/shep-otel".to_string(),
                },
            ),
        ]);
        let headers = DogRows::headers();
        let at = |cells: &[String], h: &str| {
            cells[headers.iter().position(|x| *x == h).unwrap()].clone()
        };
        assert_eq!(at(&rows.rows()[0], "SOURCE"), "built-in");
        assert_eq!(at(&rows.rows()[1], "SOURCE"), "adopted");

        let json = serde_json::to_value(&rows).unwrap();
        assert_eq!(json[1]["dog"]["path"], "/usr/local/bin/shep-otel");
    }

    /// `SOURCE`'s JSON value is the tagged `DogSource` object; `EXIT`'s is
    /// nested too.
    #[test]
    fn dog_rows_do_not_drift() {
        assert_no_drift(
            &DogRows(vec![dog_info("metrics", DogSource::BuiltIn)]),
            |j| &j[0],
            &["UPTIME", "CPU", "MEM", "SOURCE", "EXIT"],
        );
    }

    /// `SOURCE` is `formatted` for the reason `dog_rows_do_not_drift` gives.
    #[test]
    fn dog_enabled_row_does_not_drift() {
        assert_no_drift(
            &DogEnabledRow {
                name: "metrics".to_string(),
                source: DogSource::BuiltIn,
                shepherd_acted: true,
                status: "online".to_string(),
            },
            |j| j,
            &["SOURCE"],
        );
    }

    /// The `disable` sibling of `dog_enabled_row_does_not_drift`.
    #[test]
    fn dog_disabled_row_does_not_drift() {
        assert_no_drift(
            &DogDisabledRow {
                name: "metrics".to_string(),
                source: DogSource::BuiltIn,
                shepherd_acted: false,
                status: "not running; will not start with the next shepherd".to_string(),
            },
            |j| j,
            &["SOURCE"],
        );
    }

    /// The `adopt` sibling of `dog_enabled_row_does_not_drift`.
    #[test]
    fn dog_adopted_row_does_not_drift() {
        assert_no_drift(
            &DogAdoptedRow {
                name: "otel".to_string(),
                source: DogSource::Adopted {
                    path: "/usr/local/bin/shep-otel".to_string(),
                },
                shepherd_acted: true,
                status: "online".to_string(),
            },
            |j| j,
            &["SOURCE"],
        );
    }

    /// The `rehome` sibling, once with a recorded source and once with
    /// `None`, which passes through `assert_no_drift`'s `Value::Null` branch.
    #[test]
    fn dog_rehomed_row_does_not_drift_with_or_without_a_source() {
        assert_no_drift(
            &DogRehomedRow {
                name: "otel".to_string(),
                source: Some(DogSource::Adopted {
                    path: "/usr/local/bin/shep-otel".to_string(),
                }),
                shepherd_acted: true,
                status: "stopped".to_string(),
            },
            |j| j,
            &["SOURCE"],
        );
        assert_no_drift(
            &DogRehomedRow {
                name: "ghost".to_string(),
                source: None,
                shepherd_acted: false,
                status: "not running; will not start with the next shepherd".to_string(),
            },
            |j| j,
            &["SOURCE"],
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
    fn full_presentation() -> Presentation {
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

    /// The two rollups are not shared code, so cells are compared across the
    /// surfaces. Both are anchored at one instant, so the lookout's live
    /// uptime is the reported one.
    #[test]
    fn the_flock_table_and_the_lookout_roll_a_group_up_the_same_way() {
        use std::time::Instant;

        use crate::lookout::app::{App, Control, Msg, RowKey};
        use crate::lookout::theme::Palette;
        use crate::lookout::view::flock::{Column, columns_for, key_line};

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

    #[test]
    fn emptied_files_do_not_drift() {
        assert_no_drift(
            &EmptiedFiles(vec![
                EmptiedFile {
                    stream: "stdout",
                    file: "/home/x/.shep/logs/shepd.out.log".to_string(),
                    result: "emptied",
                },
                EmptiedFile {
                    stream: "stderr",
                    file: "/home/x/.shep/logs/shepd.err.log".to_string(),
                    result: "absent",
                },
            ]),
            |j| &j[0],
            &[],
        );
    }

    #[test]
    fn kill_row_does_not_drift() {
        assert_no_drift(
            &KillRow {
                pid: 4242,
                socket_removed: true,
            },
            |j| j,
            &[],
        );
    }

    #[test]
    fn saved_roll_row_does_not_drift() {
        let row = SavedRollRow {
            file: "/home/ada/.shep/flock.json".to_string(),
            apps: 9,
        };
        assert_no_drift(&row, |json| json, &[]);
    }

    #[test]
    fn import_rows_do_not_drift() {
        assert_no_drift(
            &ImportRows(vec![
                ImportRow {
                    name: "api".to_string(),
                    script: "/srv/api/dist/server.js".to_string(),
                    instances: 2,
                    reuse_port: true,
                },
                ImportRow {
                    name: "worker".to_string(),
                    script: "/srv/worker/dist/worker.js".to_string(),
                    instances: 1,
                    reuse_port: false,
                },
            ]),
            |j| &j[0],
            &[],
        );
    }

    /// Both stores, so the `-` slot an env key carries is covered too.
    #[test]
    fn import_env_rows_do_not_drift() {
        assert_no_drift(
            &ImportEnvRows(vec![
                ImportEnvRow {
                    key: "DB_PASSWORD".to_string(),
                    store: "secret".to_string(),
                    slot: "production".to_string(),
                    bytes: 7,
                },
                ImportEnvRow {
                    key: "PORT".to_string(),
                    store: "env".to_string(),
                    slot: "-".to_string(),
                    bytes: 4,
                },
            ]),
            |j| &j[0],
            &[],
        );
    }

    /// fails if a row ever grows a value: this payload is rendered from a
    /// `.env` and every cell but `bytes` is a name (IR-41).
    #[test]
    fn import_env_rows_print_a_length_and_never_a_value() {
        let rows = ImportEnvRows(vec![ImportEnvRow {
            key: "DB_PASSWORD".to_string(),
            store: "secret".to_string(),
            slot: "production".to_string(),
            bytes: "hunter2".len(),
        }]);
        let rendered = format!(
            "{:?}{:?}",
            rows.rows(),
            serde_json::to_value(&rows).unwrap()
        );
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains('7'), "{rendered}");
    }

    /// The two rows cover both shapes the payload carries: a file that was
    /// written, and a command that was run and failed.
    #[test]
    fn startup_steps_do_not_drift() {
        assert_no_drift(
            &StartupSteps(vec![
                StartupStep {
                    action: "wrote",
                    target: "/etc/systemd/system/shep-deploy.service".to_string(),
                    result: "ok".to_string(),
                },
                StartupStep {
                    action: "ran",
                    target: "systemctl enable --now shep-deploy.service".to_string(),
                    result: "Failed to enable unit: Unit file is masked.".to_string(),
                },
            ]),
            |j| &j[0],
            &[],
        );
    }

    /// `DeletedIds` serializes as a bare array, so `assert_no_drift` has no
    /// object keys to compare. This is its drift coverage instead.
    #[test]
    fn deleted_ids_rows_match_their_own_json_values() {
        let ids = DeletedIds(vec![10, 20, 30]);
        let json = serde_json::to_value(&ids).unwrap();
        let array = json.as_array().unwrap();
        let rows = ids.rows();

        assert_eq!(rows.len(), array.len());
        for (row, value) in rows.iter().zip(array) {
            assert_eq!(row.len(), 1, "DeletedIds::headers() has exactly one column");
            assert_eq!(row[0], value.to_string());
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

    fn sample_replies() -> TriggeredRows {
        TriggeredRows(vec![
            ActionReply {
                id: 1,
                name: "web".to_string(),
                outcome: ActionOutcome::Replied {
                    body: "pong".to_string(),
                },
            },
            ActionReply {
                id: 2,
                name: "worker".to_string(),
                outcome: ActionOutcome::NoChannel,
            },
        ])
    }

    /// OUTCOME and DETAIL both derive from `outcome`, a nested object, so
    /// both sit in `assert_no_drift`'s `formatted` list. Its key and
    /// cell-count checks still run.
    #[test]
    fn triggered_rows_do_not_drift() {
        assert_no_drift(&sample_replies(), |j| &j[0], &["OUTCOME", "DETAIL"]);
    }

    #[test]
    fn triggered_rows_render_id_name_and_outcome_kind() {
        let rows = sample_replies().rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "1");
        assert_eq!(rows[0][1], "web");
        assert_eq!(rows[0][2], "replied");
        assert_eq!(rows[1][0], "2");
        assert_eq!(rows[1][1], "worker");
        assert_eq!(rows[1][2], "no_channel");
    }

    /// An operator reading a `no_channel` row must find the config field
    /// that would have avoided it in the row itself, not only in `--help`.
    #[test]
    fn a_no_channel_detail_names_the_config_field() {
        let rows = sample_replies().rows();
        let detail = &rows[1][3];
        assert!(
            detail.contains("channel = true"),
            "a no_channel row must name the field that opens one: {detail}"
        );
        assert!(
            detail.contains("wait_ready") && detail.contains("shutdown_with_message"),
            "and the two fields that imply it: {detail}"
        );
    }

    #[test]
    fn skipped_and_timed_out_details_say_why() {
        let skipped = describe_outcome(&ActionOutcome::Skipped).1;
        assert!(skipped.to_lowercase().contains("reload"), "{skipped}");

        let timed_out = describe_outcome(&ActionOutcome::TimedOut).1;
        assert!(
            timed_out.to_lowercase().contains("action_timeout"),
            "{timed_out}"
        );
    }

    #[test]
    fn a_short_single_line_body_previews_unchanged() {
        assert_eq!(preview_body("pong"), "pong");
    }

    /// [`preview_body`]'s `seen == TRIGGER_BODY_PREVIEW_CHARS` check fires
    /// one character late, so only a body past the cap is truncated.
    #[test]
    fn a_body_exactly_at_the_cap_is_not_truncated() {
        let exact = "x".repeat(TRIGGER_BODY_PREVIEW_CHARS);
        assert_eq!(preview_body(&exact), exact);
    }

    #[test]
    fn a_body_past_the_cap_is_truncated_with_a_trailing_marker() {
        let over = "x".repeat(TRIGGER_BODY_PREVIEW_CHARS + 1);
        let preview = preview_body(&over);
        let expected = "x".repeat(TRIGGER_BODY_PREVIEW_CHARS) + "...";
        assert_eq!(preview, expected);
    }

    /// A multi-line body would otherwise split a table row across output
    /// lines (`TriggeredRows::rows`).
    #[test]
    fn embedded_newlines_and_carriage_returns_are_escaped_not_literal() {
        let preview = preview_body("line one\nline two\r\nline three");
        assert!(!preview.contains('\n'));
        assert!(!preview.contains('\r'));
        assert!(preview.contains("\\n"));
        assert!(preview.contains("\\r"));
    }

    /// Fails if truncation or escaping leaks into `Serialize` instead of
    /// staying in [`TriggeredRows::rows`].
    #[test]
    fn json_carries_the_real_body_the_table_cannot() {
        let long_body = format!(
            "{}\nsecond line",
            "x".repeat(TRIGGER_BODY_PREVIEW_CHARS * 2)
        );
        let replies = TriggeredRows(vec![ActionReply {
            id: 1,
            name: "web".to_string(),
            outcome: ActionOutcome::Replied {
                body: long_body.clone(),
            },
        }]);
        let json = serde_json::to_value(&replies).unwrap();
        assert_eq!(json[0]["outcome"]["body"], long_body);

        let table_cell = &replies.rows()[0][3];
        assert_ne!(
            *table_cell, long_body,
            "the table cell must be the collapsed preview, not the real body"
        );
    }

    fn sample_signal_replies() -> SignalledRows {
        SignalledRows(vec![
            SignalReply {
                id: 1,
                name: "web".to_string(),
                outcome: SignalOutcome::Delivered,
            },
            SignalReply {
                id: 2,
                name: "worker".to_string(),
                outcome: SignalOutcome::NotRunning,
            },
        ])
    }

    /// OUTCOME and DETAIL both derive from `outcome`, a nested JSON object
    /// rather than a scalar, as in `triggered_rows_do_not_drift`.
    #[test]
    fn signalled_rows_do_not_drift() {
        assert_no_drift(&sample_signal_replies(), |j| &j[0], &["OUTCOME", "DETAIL"]);
    }

    #[test]
    fn signalled_rows_render_id_name_and_outcome_kind() {
        let rows = sample_signal_replies().rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "1");
        assert_eq!(rows[0][1], "web");
        assert_eq!(rows[0][2], "delivered");
        assert_eq!(rows[1][0], "2");
        assert_eq!(rows[1][1], "worker");
        assert_eq!(rows[1][2], "not_running");
    }

    #[test]
    fn a_failed_signal_details_the_kernels_reason() {
        let rows = SignalledRows(vec![SignalReply {
            id: 1,
            name: "web".to_string(),
            outcome: SignalOutcome::Failed {
                reason: "No such process".to_string(),
            },
        }])
        .rows();
        assert_eq!(rows[0][2], "failed");
        assert_eq!(rows[0][3], "No such process");
    }

    fn sample_line_replies() -> SentLineRows {
        SentLineRows(vec![
            LineReply {
                id: 1,
                name: "repl".to_string(),
                outcome: LineOutcome::Sent,
            },
            LineReply {
                id: 2,
                name: "worker".to_string(),
                outcome: LineOutcome::NoStdin,
            },
        ])
    }

    /// OUTCOME and DETAIL both derive from `outcome`, a nested JSON object
    /// rather than a scalar, as in `triggered_rows_do_not_drift`.
    #[test]
    fn sent_line_rows_do_not_drift() {
        assert_no_drift(&sample_line_replies(), |j| &j[0], &["OUTCOME", "DETAIL"]);
    }

    #[test]
    fn sent_line_rows_render_id_name_and_outcome_kind() {
        let rows = sample_line_replies().rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "1");
        assert_eq!(rows[0][1], "repl");
        assert_eq!(rows[0][2], "sent");
        assert_eq!(rows[1][0], "2");
        assert_eq!(rows[1][1], "worker");
        assert_eq!(rows[1][2], "no_stdin");
    }

    /// The `whisper` sibling of `a_no_channel_detail_names_the_config_field`.
    #[test]
    fn a_no_stdin_detail_names_the_config_field() {
        let rows = sample_line_replies().rows();
        let detail = &rows[1][3];
        assert!(
            detail.contains("stdin = true"),
            "a no_stdin row must name the field that opens one: {detail}"
        );
    }

    #[test]
    fn a_not_written_line_details_the_reason() {
        let rows = SentLineRows(vec![LineReply {
            id: 1,
            name: "repl".to_string(),
            outcome: LineOutcome::NotWritten {
                reason: "pipe is full".to_string(),
            },
        }])
        .rows();
        assert_eq!(rows[0][2], "not_written");
        assert_eq!(rows[0][3], "pipe is full");
    }

    /// One bark delivered to a live sink and one the shepherd wrote itself
    /// with no sinks, shared by every test below.
    fn sample_barks() -> BarkRows {
        BarkRows(vec![
            Bark {
                at_ms: 1_700_000_000_000,
                rule: "restart-storm".to_string(),
                subject: "web".to_string(),
                message: "3 restarts in 60s".to_string(),
                sinks: vec![SinkOutcome {
                    sink: "ops".to_string(),
                    error: None,
                }],
            },
            Bark {
                at_ms: 1_700_000_060_000,
                rule: "daemon".to_string(),
                subject: "worker".to_string(),
                message: "restart budget exhausted".to_string(),
                sinks: vec![],
            },
        ])
    }

    /// `WHEN` and `SINKS` are both human renderings of their own JSON field,
    /// so both sit in `formatted`.
    #[test]
    fn bark_rows_do_not_drift() {
        assert_no_drift(&sample_barks(), |j| &j[0], &["WHEN", "SINKS"]);
    }

    /// `sinks_cell`'s coverage: delivered, refused, and a shepherd-authored
    /// bark with no sinks at all.
    #[test]
    fn sinks_render_delivered_failed_and_empty() {
        let delivered = Bark {
            sinks: vec![SinkOutcome {
                sink: "ops".to_string(),
                error: None,
            }],
            ..sample_barks().0[0].clone()
        };
        assert_eq!(sinks_cell(&delivered.sinks), "ops");

        let failed = Bark {
            sinks: vec![SinkOutcome {
                sink: "ops".to_string(),
                error: Some("connection refused".to_string()),
            }],
            ..sample_barks().0[0].clone()
        };
        assert_eq!(sinks_cell(&failed.sinks), "ops(failed)");

        assert_eq!(sinks_cell(&[]), "-");
    }

    /// A comma-separated list, each sink carrying its own label.
    #[test]
    fn multiple_sinks_each_carry_their_own_outcome() {
        let sinks = vec![
            SinkOutcome {
                sink: "ops".to_string(),
                error: None,
            },
            SinkOutcome {
                sink: "oncall".to_string(),
                error: Some("timed out".to_string()),
            },
        ];
        assert_eq!(sinks_cell(&sinks), "ops, oncall(failed)");
    }

    /// The cell carries no more than the sink's name plus `(failed)`, never
    /// the error string, which can quote a webhook's HTTP response.
    #[test]
    fn a_failed_sinks_error_text_never_reaches_the_cell() {
        let sinks = vec![SinkOutcome {
            sink: "ops".to_string(),
            error: Some("HTTP 401 from discord.com/api/webhooks/...".to_string()),
        }];
        let cell = sinks_cell(&sinks);
        assert_eq!(cell, "ops(failed)");
        assert!(
            !cell.contains("401") && !cell.contains("discord"),
            "the error text must stay out of the table cell: {cell}"
        );
    }

    /// `shep barks` is newest-last, matching the file on disk.
    #[test]
    fn bark_rows_stay_in_the_order_they_were_given() {
        let rows = sample_barks().rows();
        assert_eq!(rows[0][2], "web", "the older bark stays first");
        assert_eq!(rows[1][2], "worker", "the newer bark stays last");
    }

    /// Neither column is a rendering of anything else, so `formatted` is
    /// empty.
    #[test]
    fn kv_rows_do_not_drift() {
        let rows = KvRows(vec![KvEntry {
            key: "bark.cooldown".to_string(),
            value: "30s".to_string(),
        }]);
        assert_no_drift(&rows, |j| &j[0], &[]);
    }

    #[test]
    fn kv_unset_row_does_not_drift() {
        assert_no_drift(&KvUnsetRow { removed: 2 }, |j| j, &[]);
    }

    /// ENVIRONMENTS is a joined rendering of a JSON array, so it is
    /// `formatted` rather than compared cell against field.
    #[test]
    fn secret_key_rows_do_not_drift() {
        let rows = SecretKeyRows(vec![SecretKeyRow {
            key: "DB_PASSWORD".to_string(),
            environments: vec!["all".to_string(), "staging".to_string()],
        }]);
        assert_no_drift(&rows, |j| &j[0], &["ENVIRONMENTS"]);
    }

    #[test]
    fn secret_slot_row_does_not_drift() {
        let row = SecretSlotRow {
            key: "DB_PASSWORD".to_string(),
            environment: "staging".to_string(),
        };
        assert_no_drift(&row, |j| j, &[]);
    }

    #[test]
    fn secret_value_row_does_not_drift() {
        let row = SecretValueRow {
            key: "DB_PASSWORD".to_string(),
            value: "hunter2".to_string(),
        };
        assert_no_drift(&row, |j| j, &[]);
    }

    /// fails if `SecretKeyRows`/`SecretSlotRow` grow a field that carries
    /// the value itself, or if `SecretValueRow` stops redacting the one
    /// value it does carry. All three are rendered to a terminal and to
    /// `--format json`, so a value landing in the wrong place is a
    /// credential in a log or a pipeline.
    #[test]
    fn only_secret_value_row_carries_a_value_and_its_debug_is_redacted() {
        assert!(!SecretKeyRows::headers().contains(&"VALUE"));
        assert!(!SecretSlotRow::headers().contains(&"VALUE"));
        let json = serde_json::to_string(&SecretSlotRow {
            key: "K".to_string(),
            environment: "all".to_string(),
        })
        .unwrap();
        assert!(!json.contains("value"), "{json}");

        let row = SecretValueRow {
            key: "K".to_string(),
            value: "hunter2".to_string(),
        };
        let rendered = format!("{row:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        // Exact string pinned so a lazy derive(Debug) refactor fails here,
        // matching `secrets::SecretFile`'s own redacted `Debug`.
        assert_eq!(
            rendered,
            r#"SecretValueRow { key: "K", value: "<redacted>" }"#
        );
    }

    /// The live index's single entry (`web/public/dogs.json`).
    fn sample_available_dog() -> AvailableDog {
        AvailableDog {
            name: "Spot".to_string(),
            package: "shep-log-rotate".to_string(),
            adopt_as: "log-rotate".to_string(),
            description: "Rotates grown log files and asks the shepherd to reopen them."
                .to_string(),
            repo: "https://github.com/shep-pm/shep-log-rotate".to_string(),
            license: "MIT OR Apache-2.0".to_string(),
            category: "logs".to_string(),
            source: crate::dog_index::DogSourceKind::CargoGit {
                url: "https://github.com/shep-pm/shep-log-rotate".to_string(),
            },
        }
    }

    /// `adopt_as`/`repo`/`license`/`source` serialize but are covered by
    /// `JSON_ONLY` rather than a column. The four real columns are plain
    /// strings, so `formatted` is empty.
    #[test]
    fn available_dog_rows_do_not_drift() {
        assert_no_drift(
            &AvailableDogRows(vec![sample_available_dog()]),
            |j| &j[0],
            &[],
        );
    }

    // --- PRIORITIES ------------------------------------------------------

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
        assert_priorities_match_headers::<DogEnabledRow>(&["NAME", "STATUS"]);
        assert_priorities_match_headers::<DogDisabledRow>(&["NAME", "STATUS"]);
        assert_priorities_match_headers::<DogAdoptedRow>(&["NAME", "STATUS"]);
        assert_priorities_match_headers::<DogRehomedRow>(&["NAME", "STATUS"]);
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

    /// The boundary is inclusive on the `Butter` side.
    #[test]
    fn mem_role_ramps_at_its_documented_boundary() {
        assert_eq!(mem_role(None), Role::Ink3);
        assert_eq!(mem_role(Some(MEM_ELEVATED_BYTES - 1)), Role::Meadow);
        assert_eq!(mem_role(Some(MEM_ELEVATED_BYTES)), Role::Butter);
        // A light app and a heavy one must land on opposite sides.
        assert_eq!(mem_role(Some(3_800_000)), Role::Meadow, "3.8M is light");
        assert_eq!(mem_role(Some(800_000_000)), Role::Butter, "800M is heavy");
    }

    /// Idle (`0.0%`) stays `Ink3`; the boundary is inclusive on the `Butter`
    /// side.
    #[test]
    fn cpu_role_ramps_at_its_documented_boundary() {
        assert_eq!(cpu_role(None), Role::Ink3);
        assert_eq!(cpu_role(Some(0.0)), Role::Ink3);
        assert_eq!(cpu_role(Some(0.1)), Role::Meadow);
        assert_eq!(cpu_role(Some(CPU_ELEVATED_PERCENT - 0.1)), Role::Meadow);
        assert_eq!(cpu_role(Some(CPU_ELEVATED_PERCENT)), Role::Butter);
        assert_eq!(cpu_role(Some(99.0)), Role::Butter);
    }

    #[test]
    fn restarts_role_is_ink3_only_at_exactly_zero() {
        assert_eq!(restarts_role(0), Role::Ink3);
        assert_eq!(restarts_role(1), Role::Butter);
        assert_eq!(restarts_role(u32::MAX), Role::Butter);
    }

    /// A still-running sheep, a clean `0` and an uncharacterised exit are all
    /// `Ink3`.
    #[test]
    fn exit_role_is_bark_only_for_a_genuine_failure() {
        // Still running, over a `last_exit` that is still recorded.
        assert_eq!(
            exit_role(
                Some(1234),
                Some(ExitInfo {
                    code: Some(1),
                    signal: None
                })
            ),
            Role::Ink3
        );
        // Not running, no exit ever recorded.
        assert_eq!(exit_role(None, None), Role::Ink3);
        // Not running, a clean exit.
        assert_eq!(
            exit_role(
                None,
                Some(ExitInfo {
                    code: Some(0),
                    signal: None
                })
            ),
            Role::Ink3
        );
        // Not running, the daemon could not characterize the exit.
        assert_eq!(
            exit_role(
                None,
                Some(ExitInfo {
                    code: None,
                    signal: None
                })
            ),
            Role::Ink3
        );
        // Not running, a genuine nonzero exit code.
        assert_eq!(
            exit_role(
                None,
                Some(ExitInfo {
                    code: Some(1),
                    signal: None
                })
            ),
            Role::Bark
        );
        // Not running, killed by a signal.
        assert_eq!(
            exit_role(
                None,
                Some(ExitInfo {
                    code: None,
                    signal: Some(9)
                })
            ),
            Role::Bark
        );
    }

    // --- Colour: the seven tables that are not the flock listing ---------
    //
    // Each assertion compares the exact painted string: a check for the mere
    // presence of an escape byte passes on a cell painted the wrong role.

    /// The 256-colour presentation these cases render at.
    fn coloured() -> Presentation {
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
    fn painted(text: &str, role: Role) -> String {
        let mut cell = text.to_string();
        colour_cell(&mut cell, role, coloured());
        cell
    }

    /// One dog whose readings land on a known side of every ramp: four
    /// restarts, idle CPU, and 3 MiB, below the MEM boundary.
    fn sample_dog(status: ProcStatus, pid: Option<u32>) -> ProcessInfo {
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

    /// Driven through [`paint`] over a reversed header list, so every column
    /// sits somewhere it never sits in life.
    #[test]
    fn a_columns_colour_follows_its_name_and_not_its_position() {
        let dog = sample_dog(ProcStatus::Online, Some(14_110));
        let forwards = DogRows::headers();
        let backwards: Vec<&'static str> = forwards.iter().copied().rev().collect();

        let mut cells: Vec<String> = DogRows(vec![dog.clone()]).rows().remove(0);
        cells.reverse();
        let painted_rows = paint(vec![cells], &backwards, coloured(), true, |header, _, _| {
            process_info_paint(header, &dog)
        });

        let at = |name: &str| backwards.iter().position(|h| *h == name).unwrap();
        // Every one of these indices differs from the column's real one.
        assert_eq!(painted_rows[0][at("ID")], painted("9", Role::Ink3));
        assert_eq!(painted_rows[0][at("RESTARTS")], painted("4", Role::Butter));
        assert_eq!(painted_rows[0][at("MEM")], painted("3.0M", Role::Meadow));
        assert_eq!(painted_rows[0][at("CPU")], painted("0.0%", Role::Ink3));
        assert_eq!(
            painted_rows[0][at("SOURCE")],
            painted("adopted", Role::Butter)
        );
        assert_eq!(
            painted_rows[0][at("STATUS")],
            painted("(o.o) online", Role::Meadow)
        );
        assert_eq!(painted_rows[0][at("NAME")], "log-rotate", "still plain");
        assert_eq!(painted_rows[0][at("UPTIME")], "41s", "still plain");
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
        let adopted = DogAdoptedRow {
            name: "log-rotate".to_string(),
            source: DogSource::Adopted {
                path: "/usr/local/bin/shep-log-rotate".to_string(),
            },
            shepherd_acted: true,
            status: "online".to_string(),
        };
        let cells = reversed::<DogAdoptedRow>(adopted.rows().remove(0), dog_action_paint);
        let h = DogAdoptedRow::headers();
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

    /// `unknown` is `Butter` and never `Bark`: a client older than its daemon
    /// usually has a perfectly healthy dog.
    #[test]
    fn source_draws_the_trust_line_and_never_paints_a_working_dog_red() {
        assert_eq!(source_role(&DogSource::BuiltIn), Role::Ink3);
        assert_eq!(
            source_role(&DogSource::Adopted {
                path: "/usr/local/bin/shep-log-rotate".to_string()
            }),
            Role::Butter
        );
        assert_ne!(
            source_role(&DogSource::BuiltIn),
            source_role(&DogSource::Adopted {
                path: "/x".to_string()
            }),
            "shep's own code and a third-party binary must not look the same"
        );
    }

    /// All eleven kinds the three verbs produce.
    #[test]
    fn an_outcome_lands_in_the_tier_its_kind_calls_for() {
        for worked in ["replied", "delivered", "sent"] {
            assert_eq!(outcome_role(worked), Role::Meadow, "{worked}");
        }
        for quiet in ["skipped", "not_running"] {
            assert_eq!(outcome_role(quiet), Role::Ink3, "{quiet}");
        }
        for failed in ["timed_out", "failed", "not_written"] {
            assert_eq!(outcome_role(failed), Role::Bark, "{failed}");
        }
        for gap in ["no_channel", "no_stdin"] {
            assert_eq!(outcome_role(gap), Role::Butter, "{gap}");
        }
        assert_eq!(
            outcome_role("unknown"),
            Role::Butter,
            "a kind this client predates is a version gap, not a fault"
        );
    }

    /// Driven through a real `TriggeredRows`, so it covers the wiring as well
    /// as the tiers.
    #[test]
    fn a_reply_table_colours_its_outcome_and_leaves_its_detail_alone() {
        let rows = TriggeredRows(vec![
            ActionReply {
                id: 0,
                name: "web".to_string(),
                outcome: ActionOutcome::Replied {
                    body: "swept 3".to_string(),
                },
            },
            ActionReply {
                id: 1,
                name: "api".to_string(),
                outcome: ActionOutcome::TimedOut,
            },
        ])
        .rows_for(coloured(), true);

        assert_eq!(rows[0][0], painted("0", Role::Ink3), "ID is chrome");
        assert_eq!(rows[0][1], "web", "NAME is plain");
        assert_eq!(rows[0][2], painted("replied", Role::Meadow));
        assert_eq!(rows[0][3], "swept 3", "DETAIL carries no colour");
        assert_eq!(rows[1][2], painted("timed_out", Role::Bark));
        assert_eq!(
            rows[1][3], "no reply within the app's own action_timeout",
            "and neither does a failure's DETAIL"
        );
    }

    /// fails if the `-` placeholder rule stops reaching a column whose own
    /// rule declined to paint it: `BarkRows` returns [`Paint::Default`] for a
    /// SINKS cell holding `-`, and `Paint::Default` carries the rule.
    #[test]
    fn a_placeholder_falls_back_to_the_shared_rule() {
        let rows = BarkRows(vec![Bark {
            at_ms: 0,
            rule: "restart-storm".to_string(),
            subject: "web".to_string(),
            message: "restarted 5 times".to_string(),
            sinks: Vec::new(),
        }])
        .rows_for(coloured(), true);
        assert_eq!(rows[0][4], painted("-", Role::Ink3), "no sinks reads as -");
    }

    #[test]
    fn a_bark_whose_sink_refused_is_marked() {
        let bark = |error: Option<String>| Bark {
            at_ms: 0,
            rule: "restart-storm".to_string(),
            subject: "web".to_string(),
            message: "restarted 5 times".to_string(),
            sinks: vec![SinkOutcome {
                sink: "ops".to_string(),
                error,
            }],
        };
        let delivered = BarkRows(vec![bark(None)]).rows_for(coloured(), true);
        assert_eq!(delivered[0][4], painted("ops", Role::Meadow));

        let refused =
            BarkRows(vec![bark(Some("connection refused".to_string()))]).rows_for(coloured(), true);
        assert_eq!(refused[0][4], painted("ops(failed)", Role::Bark));
    }

    #[test]
    fn the_dogs_table_is_coloured_by_the_flock_tables_own_rules() {
        let rows =
            DogRows(vec![sample_dog(ProcStatus::Online, Some(14_110))]).rows_for(coloured(), true);
        let row = &rows[0];

        // Cell by cell, in the sheep table's order.
        assert_eq!(row[0], painted("9", Role::Ink3), "ID is chrome");
        assert_eq!(row[1], "log-rotate", "NAME is plain, as in the flock table");
        assert_eq!(
            row[2],
            painted("(o.o) online", Role::Meadow),
            "STATUS takes the face and the role, from vocabulary.rs"
        );
        assert_eq!(row[3], "14110", "a real PID is left plain");
        assert_eq!(row[4], painted("4", Role::Butter), "RESTARTS above zero");
        assert_eq!(row[5], painted("-", Role::Ink3), "EXIT: still running");
        assert_eq!(row[6], painted("0.0%", Role::Ink3), "idle CPU is not news");
        assert_eq!(row[7], painted("3.0M", Role::Meadow), "MEM below the ramp");
        assert_eq!(row[8], "41s", "UPTIME is plain, as in the flock table");
        assert_eq!(
            row[9],
            painted("adopted", Role::Butter),
            "SOURCE carries the trust distinction, and sits last"
        );
    }

    /// The case above cannot reach the placeholder branch: a running dog has
    /// a real PID, CPU and MEM.
    #[test]
    fn a_stopped_dogs_placeholders_are_muted() {
        let rows = DogRows(vec![sample_dog(ProcStatus::Stopped, None)]).rows_for(coloured(), true);
        let row = &rows[0];

        assert_eq!(row[2], painted("(-.-) stopped", Role::Ink3));
        assert_eq!(row[3], painted("-", Role::Ink3), "PID");
        assert_eq!(row[6], painted("-", Role::Ink3), "CPU");
        assert_eq!(row[7], painted("-", Role::Ink3), "MEM");
    }

    /// `DogEnabledRow::status` can carry a sentence in place of a status
    /// rendering.
    #[test]
    fn a_dog_action_row_colours_a_status_and_never_a_sentence() {
        let acted = DogEnabledRow {
            name: "log-rotate".to_string(),
            source: DogSource::Adopted {
                path: "/usr/local/bin/shep-log-rotate".to_string(),
            },
            shepherd_acted: true,
            status: "online".to_string(),
        };
        let row = &acted.rows_for(coloured(), true)[0];
        assert_eq!(
            row[1],
            painted("adopted", Role::Butter),
            "SOURCE says this is not shep's own code"
        );
        assert_eq!(row[3], painted("(o.o) online", Role::Meadow));

        let sentence = "no shepherd running; the config was written";
        let unacted = DogEnabledRow {
            name: "log-rotate".to_string(),
            source: DogSource::BuiltIn,
            shepherd_acted: false,
            status: sentence.to_string(),
        };
        let row = &unacted.rows_for(coloured(), true)[0];
        assert_eq!(row[3], sentence, "a sentence is left exactly as it was");
    }

    /// `false` is worth knowing, but the STATUS cell beside it already says
    /// so in a whole sentence.
    #[test]
    fn a_dog_action_row_leaves_the_name_and_the_shepherd_column_plain() {
        let row = &DogDisabledRow {
            name: "log-rotate".to_string(),
            source: DogSource::BuiltIn,
            shepherd_acted: false,
            status: "no shepherd running".to_string(),
        }
        .rows_for(coloured(), true)[0];
        assert_eq!(row[0], "log-rotate");
        assert_eq!(row[2], "false");
    }

    /// `rehome` is the only one of the four whose SOURCE can be absent.
    #[test]
    fn a_rehomed_row_with_nothing_to_forget_still_mutes_its_source() {
        let row = &DogRehomedRow {
            name: "metrics".to_string(),
            source: None,
            shepherd_acted: true,
            status: "stopped".to_string(),
        }
        .rows_for(coloured(), true)[0];
        assert_eq!(row[1], painted("-", Role::Ink3));
        assert_eq!(row[3], painted("(-.-) stopped", Role::Ink3));
    }

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

    /// A variant missing from `status_named_by`'s list renders plain instead
    /// of failing to compile. Driven off `Display`, so it also fails if the
    /// two disagree.
    #[test]
    fn every_status_is_recognised_by_its_own_rendering() {
        for status in [
            ProcStatus::Starting,
            ProcStatus::Online,
            ProcStatus::Stopping,
            ProcStatus::Stopped,
            ProcStatus::Errored,
            ProcStatus::WaitingRestart,
        ] {
            assert_eq!(
                status_named_by(&status.to_string()),
                Some(status),
                "{status} is not recognised by its own rendering"
            );
        }
        assert_eq!(
            status_named_by("no shepherd running"),
            None,
            "and a sentence is not mistaken for one"
        );
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

    /// The `Presentation` for one style, at a width nothing drops at.
    fn styled(level: crate::style::StyleLevel) -> Presentation {
        Presentation::new(
            level,
            None,
            Some(std::ffi::OsStr::new("xterm-256color")),
            None,
            200,
        )
    }

    /// `sample_dog`, plus what this shepherd knows about its handshake.
    fn dog_with_contact(handshook: Option<bool>) -> ProcessInfo {
        let mut dog = sample_dog(ProcStatus::Online, Some(208_341));
        dog.handshook = handshook;
        dog
    }

    /// The cell under `header` in `T`'s only row.
    fn cell_of<T: Render>(row: &[String], header: &str) -> String {
        row[T::headers().iter().position(|h| *h == header).unwrap()].clone()
    }

    /// The process is alive, so `status` is not wrong; it answers a different
    /// question than the operator's.
    #[test]
    fn a_dog_that_has_never_answered_the_shepherd_does_not_read_as_online() {
        let rows = DogRows(vec![dog_with_contact(Some(false))]).rows();
        assert_eq!(cell_of::<DogRows>(&rows[0], "STATUS"), "silent");
    }

    /// `full` carries its own face, and `bare` never reaches `rows_for`, so
    /// its cell carries no escape.
    #[test]
    fn a_silent_dog_reads_the_same_in_all_three_styles() {
        use crate::style::StyleLevel;
        let dogs = DogRows(vec![dog_with_contact(Some(false))]);

        let full = dogs.rows_for(styled(StyleLevel::Full), true);
        assert_eq!(
            cell_of::<DogRows>(&full[0], "STATUS"),
            painted("(?_?) silent", Role::Butter)
        );

        let plain = dogs.rows_for(styled(StyleLevel::Plain), true);
        assert_eq!(
            cell_of::<DogRows>(&plain[0], "STATUS"),
            painted("silent", Role::Butter)
        );

        let bare = dogs.rows();
        let cell = cell_of::<DogRows>(&bare[0], "STATUS");
        assert_eq!(cell, "silent");
        assert!(!cell.contains('\u{1b}'), "bare carries no escape: {cell:?}");
    }

    /// The whole row is compared: a guard keyed on the wrong field could
    /// leave STATUS right and move something else.
    #[test]
    fn a_dog_that_has_answered_renders_exactly_as_before() {
        use crate::style::StyleLevel;
        let mut before = sample_dog(ProcStatus::Online, Some(208_341));
        before.handshook = None;
        let talking = dog_with_contact(Some(true));

        assert_eq!(
            DogRows(vec![talking.clone()]).rows(),
            DogRows(vec![before.clone()]).rows()
        );
        assert_eq!(
            DogRows(vec![talking]).rows_for(styled(StyleLevel::Full), true),
            DogRows(vec![before]).rows_for(styled(StyleLevel::Full), true)
        );
    }

    /// `None` means "no handshake fact to report", never "never handshaken".
    #[test]
    fn a_dog_from_a_shepherd_predating_the_field_reads_as_it_always_did() {
        use crate::style::StyleLevel;
        let rows = DogRows(vec![dog_with_contact(None)]).rows();
        assert_eq!(cell_of::<DogRows>(&rows[0], "STATUS"), "online");

        let full = DogRows(vec![dog_with_contact(None)]).rows_for(styled(StyleLevel::Full), true);
        assert_eq!(
            cell_of::<DogRows>(&full[0], "STATUS"),
            painted("(o.o) online", Role::Meadow)
        );
    }

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

    /// `Row::reported` is the lookout's own copy, not shared code, so every
    /// axis that decides the answer is driven together: `dog`, `handshook`
    /// and every `ProcStatus`.
    #[test]
    fn the_flock_table_and_the_lookout_read_a_dogs_silence_the_same_way() {
        use crate::lookout::app::Row;

        let statuses = [
            ProcStatus::Starting,
            ProcStatus::Online,
            ProcStatus::Stopping,
            ProcStatus::Stopped,
            ProcStatus::Errored,
            ProcStatus::WaitingRestart,
        ];
        let handshooks = [None, Some(false), Some(true)];
        let dogs = [None, Some(DogSource::BuiltIn)];

        for dog in &dogs {
            for &handshook in &handshooks {
                for &status in &statuses {
                    let info = ProcessInfo::builder(9, "log-rotate", status)
                        .dog(dog.clone())
                        .handshook(handshook)
                        .build();

                    let table = reported(&info);
                    let dashboard = Row {
                        info: info.clone(),
                        anchor: std::time::Instant::now(),
                    }
                    .reported();

                    assert_eq!(
                        table, dashboard,
                        "dog={dog:?} handshook={handshook:?} status={status:?}"
                    );
                }
            }
        }
    }

    /// `online` is the one word a silence contradicts: the rest already say
    /// the relationship is not established.
    #[test]
    fn only_online_is_overridden_by_a_silence() {
        for status in [
            ProcStatus::Starting,
            ProcStatus::Stopping,
            ProcStatus::Stopped,
            ProcStatus::Errored,
            ProcStatus::WaitingRestart,
        ] {
            let mut dog = sample_dog(status, Some(208_341));
            dog.handshook = Some(false);
            let rows = DogRows(vec![dog]).rows();
            assert_eq!(
                cell_of::<DogRows>(&rows[0], "STATUS"),
                status.to_string(),
                "{status} says what it says without help"
            );
        }
    }

    /// `status` alone still reads `online` for a silent dog.
    #[test]
    fn the_json_form_carries_the_handshake_fact() {
        let json = serde_json::to_value(DogRows(vec![
            dog_with_contact(Some(false)),
            dog_with_contact(Some(true)),
            dog_with_contact(None),
        ]))
        .unwrap();
        assert_eq!(json[0]["handshook"], serde_json::json!(false));
        assert_eq!(json[0]["status"], "online");
        assert_eq!(json[1]["handshook"], serde_json::json!(true));
        assert_eq!(json[2]["handshook"], serde_json::Value::Null);
    }
}
