//! The dog-specific rows: `DogRows`, and the `DogActionRow` reply
//! `shep enable`/`disable`/`adopt`/`rehome` render.

use serde::Serialize;
use shep_core::protocol::{DogAccess, DogSource, ProcessInfo};

use crate::output::Render;
use crate::style::Presentation;

use super::toolkit::{dog_action_paint, exit_cell, paint, process_info_paint, reported};

const EMPTY: &str = "-";

/// The SOURCE column's cell: `source`'s kind, or `-` when there is none.
///
/// Never the adopted path, which is wider than the rest of the table
/// together and is one `--format json` away.
fn source_cell(source: Option<&DogSource>) -> String {
    source.map_or(EMPTY, |source| source.into()).to_owned()
}

/// The ACCESS column's cell: the marks `access` carries, or `-` for a dog
/// with nothing to flag.
///
/// Names stay in the JSON. The cell says which of the three states, because
/// a column has room for words and not for a list of permissions.
fn access_cell(access: Option<&DogAccess>) -> String {
    let Some(access) = access.filter(|access| !access.is_clear()) else {
        return EMPTY.to_owned();
    };
    [
        (access.undeclared, "undeclared"),
        (access.not_reviewed, "not reviewed"),
        (!access.waiting.is_empty(), "waiting"),
    ]
    .into_iter()
    .filter_map(|(on, word)| on.then_some(word))
    .collect::<Vec<_>>()
    .join(", ")
}

/// The dogs half of a flock listing: the `ProcessInfo`s whose `dog` marker
/// is set.
///
/// ```text
/// common:  ID  NAME  STATUS  PID  RESTARTS  EXIT  CPU  MEM  UPTIME
/// sheep:   ... + FOLD  SMIT
/// dogs:    ... + SOURCE  ACCESS
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
            "ACCESS",
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
                    // `None` is unreachable: callers filter on `dog.is_some()`.
                    source_cell(p.dog.as_ref()),
                    access_cell(p.access.as_ref()),
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
    fn json_key_for(header: &str) -> String {
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
            "ACCESS" => "access",
            other => panic!("DogRows::headers() does not include {other:?}"),
        }
        .to_owned()
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
    // SOURCE takes FOLD's `7`; ACCESS ties EXIT's `6` and, being later, goes
    // first of the two. The JSON always carries it.
    const PRIORITIES: &'static [u8] = &[0, 0, 0, 2, 4, 6, 5, 3, 1, 7, 6];
}

/// `DogActionRow`'s STATUS cell and the SHEPHERD cell beside it, as one
/// choice.
///
/// The two are never independent: a dog verb has a `no shepherd` sentence
/// exactly when no shepherd answered. Holding the pair in one type makes
/// the other combination a type error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DogActionStatus {
    /// No shepherd answered: the verb wrote the config and reports what the
    /// next shepherd will do. `shepherd_acted` reads `false`.
    NoShepherd(&'static str),
    /// A shepherd answered the verb's request. `shepherd_acted` reads `true`.
    Acted(&'static str),
}

impl DogActionStatus {
    /// The STATUS cell's text, which for [`Self::NoShepherd`] is a whole
    /// sentence rather than a status rendering.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::NoShepherd(text) | Self::Acted(text) => text,
        }
    }

    /// Whether a shepherd acted, which is what the SHEPHERD column and the
    /// `shepherd_acted` JSON key report.
    #[must_use]
    pub const fn shepherd_acted(self) -> bool {
        matches!(self, Self::Acted(..))
    }
}

/// `shep enable`/`disable`/`adopt`, and `shep delete`/`rehome` on a dog's,
/// one-row reply: what the config edit and, if a shepherd was reached, the
/// resulting RPC did.
///
/// `source` keeps the whole [`DogSource`] so JSON carries an adopted
/// binary's path; the SOURCE column renders the kind alone. `None` is a
/// removal's case alone: a dog that was never adopted has none.
#[derive(Debug, Serialize)]
pub struct DogActionRow {
    name: String,
    source: Option<DogSource>,
    shepherd_acted: bool,
    status: &'static str,
}

impl DogActionRow {
    /// `source` takes a [`DogSource`] or an [`Option`] of one; `status` says
    /// at once what the STATUS cell reads and whether a shepherd acted.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        source: impl Into<Option<DogSource>>,
        status: DogActionStatus,
    ) -> Self {
        Self {
            name: name.into(),
            source: source.into(),
            shepherd_acted: status.shepherd_acted(),
            status: status.text(),
        }
    }
}

impl Render for DogActionRow {
    fn headers() -> &'static [&'static str] {
        &["NAME", "SOURCE", "SHEPHERD", "STATUS"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        vec![vec![
            self.name.clone(),
            source_cell(self.source.as_ref()),
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
    fn json_key_for(header: &str) -> String {
        match header {
            "NAME" => "name",
            "SOURCE" => "source",
            "SHEPHERD" => "shepherd_acted",
            "STATUS" => "status",
            other => panic!("DogActionRow::headers() does not include {other:?}"),
        }
        .to_owned()
    }
    // Parallel to `headers()`. SOURCE drops before SHEPHERD, and a 4-column
    // table loses only one of the two.
    const PRIORITIES: &'static [u8] = &[0, 7, 6, 0];
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
            dog_info("metrics", DogSource::BuiltIn { grants: None }),
            dog_info(
                "otel",
                DogSource::Adopted {
                    grants: None,
                    path: "/usr/local/bin/shep-otel".to_string(),
                    channel: false,
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
            &DogRows(vec![dog_info(
                "metrics",
                DogSource::BuiltIn { grants: None },
            )]),
            |j| &j[0],
            &["UPTIME", "CPU", "MEM", "SOURCE", "EXIT", "ACCESS"],
        );
    }

    /// `SOURCE` is `formatted` for the reason `dog_rows_do_not_drift` gives.
    #[test]
    fn dog_enabled_row_does_not_drift() {
        assert_no_drift(
            &DogActionRow::new(
                "metrics",
                DogSource::BuiltIn { grants: None },
                DogActionStatus::Acted("online"),
            ),
            |j| j,
            &["SOURCE"],
        );
    }

    /// The `disable` sibling of `dog_enabled_row_does_not_drift`.
    #[test]
    fn dog_disabled_row_does_not_drift() {
        assert_no_drift(
            &DogActionRow::new(
                "metrics",
                DogSource::BuiltIn { grants: None },
                DogActionStatus::NoShepherd("not running; will not start with the next shepherd"),
            ),
            |j| j,
            &["SOURCE"],
        );
    }

    /// The `adopt` sibling of `dog_enabled_row_does_not_drift`.
    #[test]
    fn dog_adopted_row_does_not_drift() {
        assert_no_drift(
            &DogActionRow::new(
                "otel",
                DogSource::Adopted {
                    grants: None,
                    path: "/usr/local/bin/shep-otel".to_string(),
                    channel: false,
                },
                DogActionStatus::Acted("online"),
            ),
            |j| j,
            &["SOURCE"],
        );
    }

    /// The SHEPHERD cell and the `shepherd_acted` key are read off which
    /// variant the status came in, so a verb cannot report that a shepherd
    /// acted while its own STATUS says none did.
    #[test]
    fn the_shepherd_cell_and_key_come_from_the_status_variant() {
        let acted = DogActionRow::new(
            "metrics",
            DogSource::BuiltIn { grants: None },
            DogActionStatus::Acted("online"),
        );
        assert_eq!(acted.rows()[0][2], "true");
        assert_eq!(acted.rows()[0][3], "online");
        assert_eq!(
            serde_json::to_value(&acted).unwrap()["shepherd_acted"],
            serde_json::json!(true)
        );

        let absent = DogActionRow::new(
            "metrics",
            DogSource::BuiltIn { grants: None },
            DogActionStatus::NoShepherd("no shepherd running; the config was written"),
        );
        assert_eq!(absent.rows()[0][2], "false");
        assert_eq!(
            absent.rows()[0][3],
            "no shepherd running; the config was written"
        );
        assert_eq!(
            serde_json::to_value(&absent).unwrap()["shepherd_acted"],
            serde_json::json!(false)
        );
    }

    /// The `rehome` sibling, once with a recorded source and once with
    /// `None`, which passes through `assert_no_drift`'s `Value::Null` branch.
    #[test]
    fn dog_rehomed_row_does_not_drift_with_or_without_a_source() {
        assert_no_drift(
            &DogActionRow::new(
                "otel",
                DogSource::Adopted {
                    grants: None,
                    path: "/usr/local/bin/shep-otel".to_string(),
                    channel: false,
                },
                DogActionStatus::Acted("stopped"),
            ),
            |j| j,
            &["SOURCE"],
        );
        assert_no_drift(
            &DogActionRow::new(
                "ghost",
                None::<DogSource>,
                DogActionStatus::NoShepherd("not running; will not start with the next shepherd"),
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

    /// `DogActionRow`'s status can carry a sentence in place of a status
    /// rendering.
    #[test]
    fn a_dog_action_row_colours_a_status_and_never_a_sentence() {
        let acted = DogActionRow::new(
            "log-rotate",
            DogSource::Adopted {
                grants: None,
                path: "/usr/local/bin/shep-log-rotate".to_string(),
                channel: false,
            },
            DogActionStatus::Acted("online"),
        );
        let row = &acted.rows_for(coloured(), true)[0];
        assert_eq!(
            row[1],
            painted("adopted", Role::Butter),
            "SOURCE says this is not shep's own code"
        );
        assert_eq!(row[3], painted("(o.o) online", Role::Meadow));

        let sentence = "no shepherd running; the config was written";
        let unacted = DogActionRow::new(
            "log-rotate",
            DogSource::BuiltIn { grants: None },
            DogActionStatus::NoShepherd(sentence),
        );
        let row = &unacted.rows_for(coloured(), true)[0];
        assert_eq!(row[3], sentence, "a sentence is left exactly as it was");
    }

    /// `false` is worth knowing, but the STATUS cell beside it already says
    /// so in a whole sentence.
    #[test]
    fn a_dog_action_row_leaves_the_name_and_the_shepherd_column_plain() {
        let row = &DogActionRow::new(
            "log-rotate",
            DogSource::BuiltIn { grants: None },
            DogActionStatus::NoShepherd("no shepherd running"),
        )
        .rows_for(coloured(), true)[0];
        assert_eq!(row[0], "log-rotate");
        assert_eq!(row[2], "false");
    }

    /// `rehome` is the only one of the four whose SOURCE can be absent.
    #[test]
    fn a_rehomed_row_with_nothing_to_forget_still_mutes_its_source() {
        let row = &DogActionRow::new(
            "metrics",
            None::<DogSource>,
            DogActionStatus::Acted("stopped"),
        )
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
        let adopted = DogActionRow::new(
            "otel",
            DogSource::Adopted {
                grants: None,
                path: "/usr/local/bin/shep-otel".to_string(),
                channel: false,
            },
            DogActionStatus::Acted("online"),
        );
        assert_eq!(adopted.rows()[0][1], "adopted", "the column names a kind");

        let json = serde_json::to_value(&adopted).unwrap();
        assert_eq!(json["source"]["kind"], "adopted");
        assert_eq!(json["source"]["path"], "/usr/local/bin/shep-otel");
        assert_eq!(json["status"], "online");
        assert_eq!(json["shepherd_acted"], serde_json::json!(true));

        // `rehome` alone reaches this: `null`, never the column's own `-`.
        let forgotten = DogActionRow::new(
            "metrics",
            None::<DogSource>,
            DogActionStatus::NoShepherd("not running; will not start with the next shepherd"),
        );
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
