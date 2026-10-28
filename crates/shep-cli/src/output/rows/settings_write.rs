//! The payload `shep set` reports: one row per key the write touches, and
//! the run's own verdict beside it.

use serde::Serialize;

use crate::output::Render;
use crate::style::Presentation;
use crate::vocabulary::Role;

use super::copy::{CopyChange, render_value};
use super::toolkit::{Paint, paint};

/// One settings write: which holder the address named, the keys it changed,
/// and the run's own verdict.
///
/// The four fields beside `changes` have no column, for [`crate::output::CopyReport`]'s
/// reason: the table prints the change list, and the run's state rides as a
/// notice line. `Debug` is derived: `changes` prints through [`CopyChange`]'s
/// redacting impl.
#[derive(Debug, Clone, Serialize)]
pub struct SetReport {
    /// `sheep` or `dog`: which half of the flock the address named.
    pub kind: &'static str,
    /// The name the address named, spelled as the flock spells it.
    pub name: String,
    /// The path inside those settings, spelled as the address spelled it.
    pub path: String,
    /// Whether this run wrote anything.
    pub written: bool,
    /// Whether the operator asked for the changes and nothing else.
    pub dry_run: bool,
    /// Keys the sheep picked up only at its next spawn, naming the verb that
    /// promotes them.
    pub pending: Vec<String>,
    /// One entry per key the write touches, in dotted-key order.
    pub changes: Vec<CopyChange>,
}

impl Render for SetReport {
    fn headers() -> &'static [&'static str] {
        &["KEY", "CHANGE", "OLD", "NEW"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.changes
            .iter()
            .map(|change| {
                vec![
                    change.key.clone(),
                    change.change.to_string(),
                    change
                        .old
                        .as_ref()
                        .map_or_else(|| "-".to_string(), render_value),
                    change
                        .new
                        .as_ref()
                        .map_or_else(|| "-".to_string(), render_value),
                ]
            })
            .collect()
    }

    /// CHANGE alone: `added` is the row that puts a key the flock had nowhere
    /// else, so it takes `Meadow` the way a copy's added key does.
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        let rows = self.rows();
        paint(
            rows,
            Self::headers(),
            presentation,
            status_word,
            |header, cell, _index| match (header, cell) {
                ("CHANGE", "removed") => Paint::Role(Role::Bark),
                ("CHANGE", "added") => Paint::Role(Role::Meadow),
                _ => Paint::Default,
            },
        )
    }

    // Parallel to `headers()`. KEY and CHANGE identify the row; the two
    // value columns are the unbounded ones a narrow terminal loses first.
    const PRIORITIES: &'static [u8] = &[0, 0, 6, 7];
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::tests::assert_no_drift;
    use super::*;

    fn report() -> SetReport {
        SetReport {
            kind: "sheep",
            name: "api".to_string(),
            path: "dogs.kelpie.git".to_string(),
            written: true,
            dry_run: false,
            pending: Vec::new(),
            changes: vec![
                CopyChange {
                    key: "merging".to_string(),
                    change: "changed",
                    old: Some(json!("ff")),
                    new: Some(json!("rebase")),
                },
                CopyChange {
                    key: "remote".to_string(),
                    change: "added",
                    old: None,
                    new: Some(json!("upstream")),
                },
            ],
        }
    }

    #[test]
    fn a_set_report_does_not_drift() {
        // OLD and NEW are cells rendered from the value, so the cell and the
        // JSON field are deliberately different renderings of one value.
        assert_no_drift(&report(), |json| &json["changes"][0], &["OLD", "NEW"]);
    }

    /// The change list is the record count: the table and the JSON must never
    /// disagree about how many keys a write touches.
    #[test]
    fn a_set_report_renders_one_row_per_change() {
        let report = report();
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["changes"].as_array().unwrap().len(), 2);
        assert_eq!(report.rows().len(), 2);
        assert_eq!(report.rows()[0], ["merging", "changed", "ff", "rebase"]);
    }

    /// A dog's table can hold a credential, and this is the type a log line
    /// would print (IR-41).
    #[test]
    fn a_set_report_debug_never_prints_a_value() {
        let report = SetReport {
            kind: "dog",
            name: "kelpie".to_string(),
            path: "token".to_string(),
            written: true,
            dry_run: false,
            pending: Vec::new(),
            changes: vec![CopyChange {
                key: "token".to_string(),
                change: "changed",
                old: Some(json!("hunter2")),
                new: Some(json!("rotated")),
            }],
        };
        assert_eq!(
            format!("{report:?}"),
            "SetReport { kind: \"dog\", name: \"kelpie\", path: \"token\", written: true, \
             dry_run: false, pending: [], changes: [CopyChange { key: \"token\", change: \
             \"changed\", old: <value>, new: <value> }] }"
        );
    }
}
