//! `emit_described`'s own envelope: `describe`'s sheep table, lamb trees,
//! and per-name config/secret prose underneath it.

use std::collections::BTreeSet;
use std::io;

use serde::Serialize;
use shep_core::protocol::ProcessInfo;

use crate::cli::Format;
use crate::style::Presentation;

use super::{FlockRows, LambRows, SCHEMA_VERSION, rows, table_of};

/// The `--format json` shape [`emit_described`] writes:
/// [`super::OutputEnvelope`]'s own three fields, plus `secrets` riding
/// beside `data` rather than inside it.
///
/// A sibling field rather than a new column on [`ProcessInfo`]: `secrets`
/// is derived by the client from local files, never a fact the shepherd
/// reports, and `data` stays exactly the array it always was, so an
/// existing `data[0].name` script sees no shape change. Empty skips the
/// field entirely, matching a `fold` reply, which never computes one.
///
/// Only ever constructed by [`emit_described`]. `#[cfg_attr(windows,
/// allow(dead_code))]` for `diagnostics::NoticeEnvelope`'s reason: every
/// caller lives
/// in `commands/` or `lib.rs`'s `#[cfg(unix)]` arms.
#[derive(Serialize)]
#[cfg_attr(windows, allow(dead_code))]
struct DescribedEnvelope<'a> {
    schema_version: u32,
    command: &'a str,
    data: FlockRows,
    #[serde(skip_serializing_if = "<[rows::DescribedSecret]>::is_empty")]
    secrets: &'a [rows::DescribedSecret],
}

/// Renders one `describe` answer: the sheep table, then each sheep's lamb
/// tree beneath it when the reply walked and found any.
///
/// A silent row also gets a paragraph from
/// [`crate::vocabulary::silence_note`] (what `flock::silence_pointer` points
/// at). A "Depends on" heading follows, once per name, naming the sheep this
/// one waits for at a staged start; then Pending and Overridden headings,
/// same once-per-name rule, naming `shep reload <name>` as what promotes a
/// parked config. Never shorten the caption to "process tree": the walk
/// follows parent-pid links while the stop ladder acts on the process group,
/// and the two diverge.
///
/// `secrets` is `describe`'s own local read of this machine's secret
/// stores, keyed by sheep name; pass an empty slice for a reply (`fold`,
/// today) that never computes one. Printed once per name, right after
/// Overridden, in the same "prose under the table" shape.
///
/// # Errors
/// The underlying write failed.
#[cfg_attr(windows, allow(dead_code))]
pub fn emit_described(
    out: &mut dyn io::Write,
    fmt: Format,
    command: &str,
    listing: Vec<ProcessInfo>,
    style: Presentation,
    secrets: &[rows::DescribedSecret],
) -> io::Result<()> {
    match fmt {
        Format::Json => {
            let envelope = DescribedEnvelope {
                schema_version: SCHEMA_VERSION,
                command,
                data: FlockRows(listing),
                secrets,
            };
            serde_json::to_writer(&mut *out, &envelope)?;
            writeln!(out)
        }
        Format::Table => {
            let flock = FlockRows(listing);
            write!(out, "{}", table_of(&flock, style))?;
            // Before the lamb trees, because this explains a cell in the
            // table directly above it and a lamb table would put a second
            // table between the two.
            for sheep in &flock.0 {
                if let Some(note) = rows::silence_note(sheep) {
                    writeln!(out, "\n{note}")?;
                }
            }
            for sheep in &flock.0 {
                let Some(lambs) = &sheep.lambs else {
                    continue;
                };
                if lambs.is_empty() {
                    continue;
                }
                writeln!(
                    out,
                    "\nLambs of {} (id {}) — parent-pid descendants of {}, which is not exactly \
                     the set a stop kills",
                    sheep.name,
                    sheep.id,
                    sheep
                        .pid
                        .map_or_else(|| "-".to_string(), |pid| pid.to_string()),
                )?;
                write!(out, "{}", table_of(&LambRows(lambs.clone()), style))?;
            }
            // Once per name, not once per row: a parked or overridden
            // config belongs to the app, and the daemon writes the same
            // entry onto every slot of a name. The rows themselves stay
            // per instance, since that is a claim about the process.
            let mut said: BTreeSet<&str> = BTreeSet::new();
            for sheep in &flock.0 {
                if !said.insert(sheep.name.as_str()) {
                    continue;
                }
                if !sheep.depends_on.is_empty() {
                    writeln!(out, "\nDepends on for {}:", sheep.name)?;
                    for name in &sheep.depends_on {
                        writeln!(out, "  {name}")?;
                    }
                }
                if let Some(fields) = sheep.pending.as_deref().filter(|f| !f.is_empty()) {
                    writeln!(
                        out,
                        "\nPending for {}, parked by a load; `shep reload {}` promotes it:",
                        sheep.name, sheep.name,
                    )?;
                    for field in fields {
                        writeln!(out, "  {field}")?;
                    }
                }
                if let Some(fields) = sheep.overridden.as_deref().filter(|f| !f.is_empty()) {
                    writeln!(
                        out,
                        "\nOverridden for {}, fields its current Flockfile does not declare:",
                        sheep.name,
                    )?;
                    for field in fields {
                        writeln!(out, "  {field}")?;
                    }
                }
                let mine: Vec<&rows::DescribedSecret> = secrets
                    .iter()
                    .filter(|entry| entry.name == sheep.name)
                    .collect();
                if !mine.is_empty() {
                    writeln!(out, "\nSecrets for {}:", sheep.name)?;
                    for entry in mine {
                        writeln!(
                            out,
                            "  {} ({}): {}",
                            entry.reference,
                            entry.environment,
                            entry.status.as_table_word()
                        )?;
                    }
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use shep_core::protocol::{DogSource, Lamb};
    use shep_core::status::ProcStatus;

    use crate::output::rows::tests::dog_info;

    use super::super::tests::{sheep_info, silent_dog};
    use super::*;

    /// Both rows read `silent` everywhere else; only `dog_stale` says
    /// whether anything further is going to happen. The give-up arm must
    /// not name a cause: the shepherd's own account lives in the dog's log.
    #[test]
    fn describe_says_whether_the_shepherd_has_given_up_on_a_silent_dog() {
        let render = |info: ProcessInfo| {
            let mut out = Vec::new();
            emit_described(
                &mut out,
                Format::Table,
                "describe",
                vec![info],
                Presentation::BARE,
                &[],
            )
            .unwrap();
            String::from_utf8(out).unwrap()
        };

        let waiting = render(silent_dog("log-rotate", Some(false)));
        assert!(
            waiting.contains("restarts a dog once"),
            "a dog still inside its budget is told what happens next: {waiting}"
        );
        assert!(
            !waiting.contains("GIVEN UP"),
            "and nothing has been given up on yet: {waiting}"
        );

        let given_up = render(silent_dog("log-rotate", Some(true)));
        assert!(
            given_up.contains("GIVEN UP"),
            "the latch is the thing no other surface reports: {given_up}"
        );
        assert!(
            given_up.contains("shep bleats log-rotate"),
            "and it sends the reader to the log that holds the evidence: {given_up}"
        );
        assert!(
            !given_up.contains("rebuild or reinstall it and run"),
            "it must not restate the daemon's verdict, which it cannot know: {given_up}"
        );

        let unknown = render(silent_dog("log-rotate", None));
        assert!(
            unknown.contains("too old to say"),
            "an older shepherd's silence about the latch is reported, not guessed: {unknown}"
        );
    }

    #[test]
    fn describe_says_nothing_extra_about_a_row_that_is_not_silent() {
        let mut talking = dog_info("bark", DogSource::BuiltIn);
        talking.handshook = Some(true);
        talking.dog_stale = Some(false);

        for info in [sheep_info("web"), talking] {
            let mut out = Vec::new();
            emit_described(
                &mut out,
                Format::Table,
                "describe",
                vec![info],
                Presentation::BARE,
                &[],
            )
            .unwrap();
            let rendered = String::from_utf8(out).unwrap();
            assert!(!rendered.contains("never answered"), "{rendered}");
        }
    }

    #[test]
    fn the_lamb_caption_does_not_promise_the_kill_set() {
        let info = ProcessInfo::builder(3, "web", ProcStatus::Online)
            .pid(Some(4242))
            .lambs(Some(vec![Lamb::new(4243, "node")]))
            .build();
        let mut out = Vec::new();
        emit_described(
            &mut out,
            Format::Table,
            "describe",
            vec![info],
            Presentation::BARE,
            &[],
        )
        .unwrap();
        let rendered = String::from_utf8(out).unwrap();

        assert!(rendered.contains("parent-pid descendants"), "{rendered}");
        assert!(
            rendered.contains("not exactly the set a stop kills"),
            "{rendered}"
        );
        // And the row itself, so the caption is not the only thing being
        // asserted.
        assert!(rendered.contains("4243"), "{rendered}");
        assert!(rendered.contains("node"), "{rendered}");
    }

    /// The same rule `emit_flock` follows for a flock with no dogs.
    #[test]
    fn a_sheep_with_no_lambs_renders_exactly_what_it_did_before() {
        let bare = ProcessInfo::builder(3, "web", ProcStatus::Online)
            .pid(Some(4242))
            .build();
        let walked_empty = ProcessInfo::builder(3, "web", ProcStatus::Online)
            .pid(Some(4242))
            .lambs(Some(Vec::new()))
            .build();

        for info in [bare, walked_empty] {
            let mut out = Vec::new();
            emit_described(
                &mut out,
                Format::Table,
                "describe",
                vec![info.clone()],
                Presentation::BARE,
                &[],
            )
            .unwrap();
            let rendered = String::from_utf8(out).unwrap();
            assert!(!rendered.contains("Lambs of"), "{rendered}");
        }
    }

    #[test]
    fn describe_lists_a_sheep_s_dependencies() {
        // fails if depends_on never reaches the operator, which leaves "why did
        // web start nine seconds in" unanswerable
        let info = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .depends_on(vec!["api".to_string(), "db".to_string()])
            .build();
        let mut out = Vec::new();
        emit_described(
            &mut out,
            Format::Table,
            "describe",
            vec![info],
            Presentation::BARE,
            &[],
        )
        .unwrap();
        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains("api"), "{rendered}");
        assert!(rendered.contains("db"), "{rendered}");
    }

    #[test]
    fn describe_says_nothing_about_dependencies_when_there_are_none() {
        // fails if an empty list prints a bare header, which every sheep in a
        // flock without ordering would then carry
        let info = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
        let mut out = Vec::new();
        emit_described(
            &mut out,
            Format::Table,
            "describe",
            vec![info],
            Presentation::BARE,
            &[],
        )
        .unwrap();
        let rendered = String::from_utf8(out).unwrap();
        assert!(!rendered.to_lowercase().contains("depends"), "{rendered}");
    }

    /// The same rule `emit_flock`'s JSON arm follows for dogs.
    #[test]
    fn the_json_surface_stays_one_array_with_lambs_on_each_row() {
        let info = ProcessInfo::builder(3, "web", ProcStatus::Online)
            .pid(Some(4242))
            .lambs(Some(vec![Lamb::new(4243, "node")]))
            .build();
        let mut out = Vec::new();
        emit_described(
            &mut out,
            Format::Json,
            "describe",
            vec![info],
            Presentation::BARE,
            &[],
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let rows = value["data"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["lambs"][0]["pid"], 4243);
    }

    /// `shep reload <name>` is the one fact an operator reading this
    /// table cannot get anywhere else.
    #[test]
    fn describe_names_pending_and_overridden_fields_and_the_promoting_verb() {
        let info = ProcessInfo::builder(3, "web", ProcStatus::Online)
            .pid(Some(4242))
            .pending(Some(vec!["env".to_string(), "cwd".to_string()]))
            .overridden(Some(vec!["max_restarts".to_string()]))
            .build();
        let mut out = Vec::new();
        emit_described(
            &mut out,
            Format::Table,
            "describe",
            vec![info],
            Presentation::BARE,
            &[],
        )
        .unwrap();
        let rendered = String::from_utf8(out).unwrap();

        assert!(rendered.contains("Pending for web"), "{rendered}");
        assert!(rendered.contains("shep reload web"), "{rendered}");
        assert!(rendered.contains("env"), "{rendered}");
        assert!(rendered.contains("cwd"), "{rendered}");
        assert!(rendered.contains("Overridden for web"), "{rendered}");
        assert!(rendered.contains("max_restarts"), "{rendered}");
    }

    /// The same rule
    /// `a_sheep_with_no_lambs_renders_exactly_what_it_did_before` follows
    /// for lambs.
    #[test]
    fn a_sheep_with_neither_list_renders_neither_heading() {
        // Both spellings of "nothing to say": `None`, and `Some(vec![])`,
        // which is what the store answers for an app with no parked
        // config. The `as_deref` filter is what keeps the second from
        // heading over no fields.
        for (pending, overridden) in [
            (None, None),
            (Some(Vec::new()), Some(Vec::new())),
            (None, Some(Vec::new())),
            (Some(Vec::new()), None),
        ] {
            let info = ProcessInfo::builder(3, "web", ProcStatus::Online)
                .pid(Some(4242))
                .pending(pending.clone())
                .overridden(overridden.clone())
                .build();
            let mut out = Vec::new();
            emit_described(
                &mut out,
                Format::Table,
                "describe",
                vec![info],
                Presentation::BARE,
                &[],
            )
            .unwrap();
            let rendered = String::from_utf8(out).unwrap();

            assert!(
                !rendered.contains("Pending for"),
                "{pending:?}/{overridden:?}: {rendered}"
            );
            assert!(
                !rendered.contains("Overridden for"),
                "{pending:?}/{overridden:?}: {rendered}"
            );
        }
    }

    /// `apply_one` writes the same store entry onto every slot of a name,
    /// so three rows carry three identical lists.
    #[test]
    fn a_clustered_app_prints_each_config_section_once() {
        let rows: Vec<ProcessInfo> = (0..3)
            .map(|slot| {
                ProcessInfo::builder(slot, "web", ProcStatus::Online)
                    .pid(Some(4242 + slot))
                    .instance(Some(slot))
                    .pending(Some(vec!["cwd".to_string()]))
                    .overridden(Some(vec!["max_restarts".to_string()]))
                    .build()
            })
            .collect();
        let mut out = Vec::new();
        emit_described(
            &mut out,
            Format::Table,
            "describe",
            rows,
            Presentation::BARE,
            &[],
        )
        .unwrap();
        let rendered = String::from_utf8(out).unwrap();

        assert_eq!(rendered.matches("Pending for web").count(), 1, "{rendered}");
        assert_eq!(
            rendered.matches("Overridden for web").count(),
            1,
            "{rendered}"
        );
    }

    /// The same rule `the_json_surface_stays_one_array_with_lambs_on_each_row`
    /// pins for lambs.
    #[test]
    fn describes_json_surface_carries_pending_and_overridden_on_each_row() {
        let info = ProcessInfo::builder(3, "web", ProcStatus::Online)
            .pid(Some(4242))
            .pending(Some(vec!["env".to_string()]))
            .overridden(Some(vec!["cwd".to_string()]))
            .build();
        let mut out = Vec::new();
        emit_described(
            &mut out,
            Format::Json,
            "describe",
            vec![info],
            Presentation::BARE,
            &[],
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let rows = value["data"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["pending"][0], "env");
        assert_eq!(rows[0]["overridden"][0], "cwd");
    }
}
