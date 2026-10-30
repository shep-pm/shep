//! What a copy prints: the change list, and the run's verdict.
//!
//! The change list prints before anything is asked about it, on the stream the
//! operator is reading. The table form's stdout is that list. A form whose
//! stdout is a JSON envelope prints the list on stderr instead, because its
//! envelope can only be written after the write the question gates, and one
//! object per invocation is the rule `--format json` keeps.

use crate::cli::Format;
use crate::exit::ExitCode;
use crate::output::{CopyChange, CopyReport, Streams, emit, write_outcome};

use super::address::Address;
use super::confirm::Go;
use super::plan::Plan;

/// The stage whose verdict a report describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportState {
    /// Changes are planned but have not been written.
    Preview,
    /// The operator requested a preview without a write.
    DryRun,
    /// The write succeeded.
    Written,
}

impl ReportState {
    /// The initial report, before any write.
    pub(crate) fn preview(dry_run: bool) -> Self {
        if dry_run { Self::DryRun } else { Self::Preview }
    }
}

/// The report a plan renders, before or after the write.
pub(crate) fn report_for(
    from: &Address,
    to: &Address,
    plan: &Plan,
    state: ReportState,
) -> CopyReport {
    CopyReport {
        from: from.sheep.clone(),
        to: to.sheep.clone(),
        path: from.path.dotted(),
        written: state == ReportState::Written,
        dry_run: state == ReportState::DryRun,
        pending: Vec::new(),
        changes: plan
            .changes
            .iter()
            .map(|change| CopyChange {
                key: change.key.clone(),
                change: change.kind.as_str(),
                old: change.old.clone(),
                new: change.new.clone(),
            })
            .collect(),
    }
}

/// Whether this run prints the change list before it asks anything about it.
///
/// The table form always prints it. Any other form prints it only when it is
/// about to ask, because a run that is not asking has nothing to show an
/// operator before it either writes or stops.
#[must_use]
pub(crate) fn shows_changes_first(fmt: Format, go: Go) -> bool {
    fmt == Format::Table || go == Go::Ask
}

/// Prints the change list as a table, on the stream the operator is reading.
///
/// The table form prints it on stdout. A form whose stdout is a JSON envelope
/// prints it on stderr, so the operator sees what they are being asked to
/// approve while stdout stays the one envelope the `--format json` rule keeps.
///
/// # Errors
/// [`ExitCode::Failure`] if the write itself failed.
pub(crate) fn print_changes(
    streams: &mut Streams<'_>,
    report: &CopyReport,
) -> Result<(), ExitCode> {
    let writer: &mut dyn std::io::Write = if streams.fmt == Format::Table {
        &mut *streams.out
    } else {
        &mut *streams.err
    };
    emit(writer, Format::Table, "copy", report.clone(), streams.style)
        .map_err(|_| ExitCode::Failure)
}

/// Prints the run's verdict.
///
/// The table form already printed the change list, so this adds nothing to it.
/// The JSON form prints the one envelope this invocation owes, with `written`
/// and `pending` filled in by what actually happened.
pub(crate) fn finish(streams: &mut Streams<'_>, report: &CopyReport) -> ExitCode {
    if streams.fmt != Format::Json {
        return ExitCode::Success;
    }
    write_outcome(emit(
        &mut *streams.out,
        Format::Json,
        "copy",
        report.clone(),
        streams.style,
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::commands::copy::address::ConfigPath;
    use crate::commands::copy::confirm::{Answer, confirm};
    use crate::commands::copy::plan::{Change, ChangeKind};
    use crate::output::Render;

    /// The two sides of every report test here.
    fn sides(dog: &str) -> (Address, Address) {
        let path = ConfigPath::Dog {
            dog: dog.to_string(),
            keys: Vec::new(),
        };
        (
            Address {
                sheep: "web".to_string(),
                path: path.clone(),
            },
            Address {
                sheep: "api".to_string(),
                path,
            },
        )
    }

    /// A report prints its changes and its verdict.
    #[test]
    fn a_report_prints_its_changes_and_its_verdict() {
        let (from, to) = sides("kelpie");
        let plan = Plan {
            changes: vec![Change {
                key: "rulings.decide".to_string(),
                kind: ChangeKind::Changed,
                old: Some(json!("issue")),
                new: Some(json!("panel")),
            }],
            write: None,
        };
        let report = report_for(&from, &to, &plan, ReportState::Written);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["from"], "web");
        assert_eq!(json["to"], "api");
        assert_eq!(json["path"], "dogs.kelpie");
        assert_eq!(json["written"], true);
        assert_eq!(json["dry_run"], false);
        assert_eq!(json["changes"][0]["key"], "rulings.decide");
        assert_eq!(json["changes"][0]["change"], "changed");
        assert_eq!(json["changes"][0]["old"], "issue");
        assert_eq!(json["changes"][0]["new"], "panel");
        assert_eq!(
            report.rows(),
            vec![vec![
                "rulings.decide".to_string(),
                "changed".to_string(),
                "issue".to_string(),
                "panel".to_string(),
            ]]
        );
    }

    /// The JSON form shows the change list before it asks, on stderr: its
    /// stdout is the one envelope, and that envelope can only be written after
    /// the write the question gates.
    #[test]
    fn the_json_form_shows_the_change_list_before_it_asks() {
        assert!(shows_changes_first(Format::Json, Go::Ask));
        assert!(shows_changes_first(Format::Table, Go::Write));
        assert!(shows_changes_first(Format::Table, Go::Refuse));
        assert!(
            !shows_changes_first(Format::Json, Go::Write),
            "--yes writes without asking, so a script gets no table on stderr"
        );
        assert!(
            !shows_changes_first(Format::Json, Go::DryRun),
            "the envelope is a dry run's output already"
        );
        assert!(!shows_changes_first(Format::Json, Go::Nothing));

        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = Streams::bare(&mut out, &mut err, Format::Json);
        let plan = Plan {
            changes: vec![
                Change {
                    key: "rulings.decide".to_string(),
                    kind: ChangeKind::Changed,
                    old: Some(json!("issue")),
                    new: Some(json!("panel")),
                },
                Change {
                    key: "restart_delay".to_string(),
                    kind: ChangeKind::Added,
                    old: None,
                    new: Some(json!(2)),
                },
            ],
            write: None,
        };
        let (from, to) = sides("kelpie");
        let report = report_for(&from, &to, &plan, ReportState::Preview);

        print_changes(&mut streams, &report).expect("the preview writes");
        let mut reader = std::io::Cursor::new(b"n\n".as_slice());
        let answer = confirm(&mut streams, "web", "api", "dogs.kelpie", 2, &mut reader);
        assert!(matches!(answer, Answer::No));

        let shown = String::from_utf8(err).unwrap();
        let list = shown
            .find("KEY")
            .expect("the change list printed on stderr");
        let question = shown.find("[y/N]").expect("the question, after the list");
        assert!(
            list < question,
            "the operator sees both changes before being asked to approve them: {shown}"
        );
        assert!(
            shown.contains("rulings.decide") && shown.contains("restart_delay"),
            "{shown}"
        );
        assert!(
            out.is_empty(),
            "stdout stays the one envelope this run never reached"
        );
    }

    /// The table form keeps its change list on stdout, where that form prints
    /// everything.
    #[test]
    fn the_table_form_keeps_the_change_list_on_stdout() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
        let plan = Plan {
            changes: vec![Change {
                key: "restart_delay".to_string(),
                kind: ChangeKind::Added,
                old: None,
                new: Some(json!(2)),
            }],
            write: None,
        };
        let (from, to) = (
            Address {
                sheep: "web".to_string(),
                path: ConfigPath::Field {
                    name: "restart_delay".to_string(),
                },
            },
            Address {
                sheep: "api".to_string(),
                path: ConfigPath::Field {
                    name: "restart_delay".to_string(),
                },
            },
        );
        let report = report_for(&from, &to, &plan, ReportState::Preview);
        print_changes(&mut streams, &report).expect("the table writes");
        let shown = String::from_utf8(out).unwrap();
        assert!(
            shown.contains("KEY") && shown.contains("restart_delay"),
            "{shown}"
        );
        assert!(
            err.is_empty(),
            "the table form's stderr stays for diagnostics"
        );
    }
}
