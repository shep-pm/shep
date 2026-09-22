//! The dog-specific rows: `DogRows`, and the `DogRow` reply
//! `shep enable`/`disable`/`adopt`/`rehome` render.

use serde::Serialize;
use shep_core::protocol::{DogSource, ProcessInfo};

use crate::output::Render;
use crate::style::Presentation;

use super::toolkit::{dog_action_paint, exit_cell, paint, process_info_paint, reported};

const EMPTY: &str = "-";

/// The dogs half of a flock listing: the `ProcessInfo`s whose `dog` marker
/// is set.
///
/// Every column the two tables share sits in the same order; each table's own
/// columns come last:
///
/// ```text
/// common:  ID  NAME  STATUS  PID  RESTARTS  EXIT  CPU  MEM  UPTIME
/// sheep:   ... + FOLD  SMIT
/// dogs:    ... + SOURCE
/// ```
///
/// `FOLD` and `SMIT` are impossible for a dog rather than empty: a dog
/// belongs to no fold, and a smit is a mark a dog paints on a sheep.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct DogRows(pub Vec<ProcessInfo>);

impl Render for DogRows {
    fn headers() -> &'static [&'static str] {
        &[
            "ID", "NAME", "STATUS", "PID", "RESTARTS", "EXIT", "CPU", "MEM", "UPTIME", "SOURCE",
        ]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|p| {
                vec![
                    p.id.to_string(),
                    p.name.clone(),
                    // A dog whose process is up and which has never
                    // answered this shepherd reads `silent`, not `online`.
                    reported(p).word(),
                    p.pid
                        .map_or_else(|| EMPTY.to_string(), |pid| pid.to_string()),
                    p.restarts.to_string(),
                    exit_cell(p.pid, p.last_exit),
                    p.cpu_percent
                        .map_or_else(|| EMPTY.to_string(), |cpu| format!("{cpu:.1}%")),
                    p.memory_bytes
                        .map_or_else(|| EMPTY.to_string(), crate::output::human_bytes),
                    crate::output::human_duration(p.uptime_ms),
                    // Never the adopted path: too wide for a column. `None`
                    // is unreachable, since callers filter on
                    // `dog.is_some()`.
                    p.dog
                        .as_ref()
                        .map_or(EMPTY, |source| source.into())
                        .to_string(),
                ]
            })
            .collect()
    }

    /// [`process_info_paint`], the same function `FlockRows` uses; SOURCE is
    /// the one column not shared with that table.
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
            "STATUS" => "status",
            "PID" => "pid",
            "RESTARTS" => "restarts",
            "EXIT" => "last_exit",
            "CPU" => "cpu_percent",
            "MEM" => "memory_bytes",
            "UPTIME" => "uptime_ms",
            "SOURCE" => "dog",
            other => panic!("DogRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[
        // A sheep concept: a dog is supervised, never grouped by fold.
        "fold",
        // Absolute paths, wider than the rest of the table together.
        "out_file",
        "err_file",
        // Always `null`: only `Describe` walks for lambs.
        "lambs",
        // A dog paints smits; nothing paints one on a dog.
        "smit",
        // It decides what STATUS says, so a column would say it twice.
        // `status` alone still reads `online` for a silent dog.
        "handshook",
        // Not derivable from `handshook`: a dog spawned a moment ago and one
        // this shepherd has given up on are both `handshook: false`.
        "dog_stale",
        // Always `Some(0)`: a dog is never stocked to N instances.
        "instance",
        // `Actor::apply_one` refuses a config entry naming a dog, so a load
        // can neither park nor override one.
        "pending",
        "overridden",
        // A sheep concept: a dog is never staged behind a start order.
        "depends_on",
        // A sheep concept: a dog has no `AppConfig` and so no ceiling to
        // report; always `null` here.
        "max_memory",
        // CPU already reports the percentage; the raw counter behind it is
        // for a client differencing its own polls, not this table.
        "cpu_ms",
    ];

    // Parallel to `headers()`. The nine shared columns carry the numbers
    // `FlockRows` gives them, so both tables narrow in the same order.
    // SOURCE takes FOLD's `7`.
    const PRIORITIES: &'static [u8] = &[0, 0, 0, 2, 4, 6, 5, 3, 1, 7];
}

/// `shep enable`/`disable`/`adopt`/`rehome`'s one-row reply: what the config
/// edit and, if a shepherd was reached, the resulting RPC did.
///
/// [`Self::shepherd_acted`] and [`Self::status`] are how a `--format json`
/// consumer tells the two outcomes apart.
///
/// `source` keeps the whole [`DogSource`], so JSON carries an adopted
/// binary's path; the SOURCE column renders the kind alone, which is all a
/// column has room for. `None` is `rehome`'s case alone: it reads the source
/// before the edit forgets it, and a dog that was never adopted has none.
#[derive(Debug, Serialize)]
pub struct DogRow {
    name: String,
    source: Option<DogSource>,
    shepherd_acted: bool,
    status: &'static str,
}

impl DogRow {
    /// `source` takes a [`DogSource`] or an [`Option`] of one; `status` takes
    /// a [`ProcStatus`](shep_core::status::ProcStatus) or one of the dog
    /// verbs' own sentences.
    pub fn new(
        name: impl Into<String>,
        source: impl Into<Option<DogSource>>,
        status: impl Into<&'static str>,
        shepherd_acted: bool,
    ) -> Self {
        Self {
            name: name.into(),
            source: source.into(),
            shepherd_acted,
            status: status.into(),
        }
    }
}

impl Render for DogRow {
    fn headers() -> &'static [&'static str] {
        &["NAME", "SOURCE", "SHEPHERD", "STATUS"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        vec![vec![
            self.name.clone(),
            // The kind alone, never the adopted path: too wide for a column,
            // and one `--format json` away. `-` for `None`, as `DogRows::rows`
            // renders it.
            self.source
                .as_ref()
                .map_or(EMPTY, |source| source.into())
                .to_owned(),
            self.shepherd_acted.to_string(),
            self.status.to_owned(),
        ]]
    }

    // The four dog-action rows' shared treatment; see `dog_action_paint`.
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        paint(
            self.rows(),
            Self::headers(),
            presentation,
            status_word,
            |header, cell, _index| dog_action_paint(header, cell),
        )
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "NAME" => "name",
            "SOURCE" => "source",
            "SHEPHERD" => "shepherd_acted",
            "STATUS" => "status",
            other => panic!("DogRow::headers() does not include {other:?}"),
        }
    }
    // Parallel to `headers()`. SOURCE drops before SHEPHERD, and a 4-column
    // table loses only one of the two.
    const PRIORITIES: &'static [u8] = &[0, 7, 6, 0];
    const JSON_ONLY: &'static [&'static str] = &[];
}

#[cfg(test)]
pub(crate) mod tests {
    use shep_core::{protocol::DogSource, status::ProcStatus};

    use crate::vocabulary::Role;

    use super::super::tests::{assert_no_drift, coloured, dog_info, painted, sample_dog};
    use super::*;

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
            &DogRow::new("metrics", DogSource::BuiltIn, "online", true),
            |j| j,
            &["SOURCE"],
        );
    }

    /// The `disable` sibling of `dog_enabled_row_does_not_drift`.
    #[test]
    fn dog_disabled_row_does_not_drift() {
        assert_no_drift(
            &DogRow::new(
                "metrics",
                DogSource::BuiltIn,
                "not running; will not start with the next shepherd",
                false,
            ),
            |j| j,
            &["SOURCE"],
        );
    }

    /// The `adopt` sibling of `dog_enabled_row_does_not_drift`.
    #[test]
    fn dog_adopted_row_does_not_drift() {
        assert_no_drift(
            &DogRow::new(
                "otel",
                DogSource::Adopted {
                    path: "/usr/local/bin/shep-otel".to_string(),
                },
                "online",
                true,
            ),
            |j| j,
            &["SOURCE"],
        );
    }

    /// The `rehome` sibling, once with a recorded source and once with
    /// `None`, which passes through `assert_no_drift`'s `Value::Null` branch.
    #[test]
    fn dog_rehomed_row_does_not_drift_with_or_without_a_source() {
        assert_no_drift(
            &DogRow::new(
                "otel",
                DogSource::Adopted {
                    path: "/usr/local/bin/shep-otel".to_string(),
                },
                "stopped",
                true,
            ),
            |j| j,
            &["SOURCE"],
        );
        assert_no_drift(
            &DogRow::new(
                "ghost",
                None::<DogSource>,
                "not running; will not start with the next shepherd",
                false,
            ),
            |j| j,
            &["SOURCE"],
        );
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

    /// `DogRow`'s status can carry a sentence in place of a status
    /// rendering.
    #[test]
    fn a_dog_action_row_colours_a_status_and_never_a_sentence() {
        let acted = DogRow::new(
            "log-rotate",
            DogSource::Adopted {
                path: "/usr/local/bin/shep-log-rotate".to_string(),
            },
            "online",
            true,
        );
        let row = &acted.rows_for(coloured(), true)[0];
        assert_eq!(
            row[1],
            painted("adopted", Role::Butter),
            "SOURCE says this is not shep's own code"
        );
        assert_eq!(row[3], painted("(o.o) online", Role::Meadow));

        let sentence = "no shepherd running; the config was written";
        let unacted = DogRow::new("log-rotate", DogSource::BuiltIn, sentence, false);
        let row = &unacted.rows_for(coloured(), true)[0];
        assert_eq!(row[3], sentence, "a sentence is left exactly as it was");
    }

    /// `false` is worth knowing, but the STATUS cell beside it already says
    /// so in a whole sentence.
    #[test]
    fn a_dog_action_row_leaves_the_name_and_the_shepherd_column_plain() {
        let row = &DogRow::new(
            "log-rotate",
            DogSource::BuiltIn,
            "no shepherd running",
            false,
        )
        .rows_for(coloured(), true)[0];
        assert_eq!(row[0], "log-rotate");
        assert_eq!(row[2], "false");
    }

    /// `rehome` is the only one of the four whose SOURCE can be absent.
    #[test]
    fn a_rehomed_row_with_nothing_to_forget_still_mutes_its_source() {
        let row = &DogRow::new("metrics", None::<DogSource>, "stopped", true)
            .rows_for(coloured(), true)[0];
        assert_eq!(row[1], painted("-", Role::Ink3));
        assert_eq!(row[3], painted("(-.-) stopped", Role::Ink3));
    }

    /// The SOURCE column renders a kind and the JSON keeps the whole
    /// `DogSource`, so an adopted binary's path survives a flattening that
    /// only the table needs. Nothing else pins this payload, and collapsing
    /// the four dog-action rows into one is exactly where it goes missing.
    #[test]
    fn the_dog_action_json_keeps_the_path_the_source_column_drops() {
        let adopted = DogRow::new(
            "otel",
            DogSource::Adopted {
                path: "/usr/local/bin/shep-otel".to_string(),
            },
            "online",
            true,
        );
        assert_eq!(adopted.rows()[0][1], "adopted", "the column names a kind");

        let json = serde_json::to_value(&adopted).unwrap();
        assert_eq!(json["source"]["kind"], "adopted");
        assert_eq!(json["source"]["path"], "/usr/local/bin/shep-otel");
        assert_eq!(json["status"], "online");
        assert_eq!(json["shepherd_acted"], serde_json::json!(true));

        // `rehome` alone reaches this: `null`, never the column's own `-`.
        let forgotten = DogRow::new("metrics", None::<DogSource>, "stopped", false);
        assert_eq!(forgotten.rows()[0][1], "-");
        assert_eq!(
            serde_json::to_value(&forgotten).unwrap()["source"],
            serde_json::Value::Null
        );
    }

    /// The `Presentation` for one style, at a width nothing drops at.
    pub(crate) fn styled(level: crate::style::StyleLevel) -> Presentation {
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
    pub(crate) fn cell_of<T: Render>(row: &[String], header: &str) -> String {
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
