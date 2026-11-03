//! The ACCESS column of the dogs table, and the `access` field beside it in
//! the JSON.

use shep_core::protocol::{DogAccess, DogSource, ProcessInfo};
use shep_core::status::ProcStatus;

use super::DogRows;
use super::tests::{coloured, painted};
use crate::output::Render;
use crate::vocabulary::Role;

fn dog(access: Option<DogAccess>) -> ProcessInfo {
    ProcessInfo::builder(1, "otel", ProcStatus::Online)
        .dog(Some(DogSource::BuiltIn {
            grants: None,
            events: Vec::new(),
        }))
        .access(access)
        .build()
}

fn access_cell(access: Option<DogAccess>) -> String {
    let rows = DogRows(vec![dog(access)]).rows();
    let at = DogRows::headers()
        .iter()
        .position(|header| *header == "ACCESS")
        .unwrap();
    rows[0][at].clone()
}

fn marks(undeclared: bool, not_reviewed: bool, waiting: &[&str]) -> DogAccess {
    DogAccess {
        undeclared,
        not_reviewed,
        waiting: waiting.iter().map(|name| (*name).to_owned()).collect(),
    }
}

#[test]
fn a_dog_with_nothing_to_flag_reads_a_dash() {
    assert_eq!(access_cell(None), "-");
    assert_eq!(access_cell(Some(marks(false, false, &[]))), "-");
}

#[test]
fn each_mark_reads_as_its_own_words() {
    assert_eq!(access_cell(Some(marks(true, false, &[]))), "undeclared");
    assert_eq!(access_cell(Some(marks(false, true, &[]))), "not reviewed");
    assert_eq!(
        access_cell(Some(marks(false, false, &["logs.read"]))),
        "waiting"
    );
}

#[test]
fn marks_that_hold_together_are_listed_in_one_cell() {
    assert_eq!(
        access_cell(Some(marks(false, true, &["logs.read", "flock.edit"]))),
        "not reviewed, waiting"
    );
}

#[test]
fn the_json_names_what_waits_and_a_dog_with_no_marks_omits_the_field() {
    let json = serde_json::to_value(DogRows(vec![
        dog(Some(marks(false, true, &["logs.read"]))),
        dog(None),
    ]))
    .unwrap();

    assert_eq!(
        json[0]["access"],
        serde_json::json!({"undeclared": false, "not_reviewed": true, "waiting": ["logs.read"]})
    );
    assert!(
        json[1].get("access").is_none(),
        "a row with no marks is the row it always was"
    );
}

#[test]
fn a_mark_is_worth_a_glance_and_a_dash_is_muted() {
    let flagged = DogRows(vec![dog(Some(marks(true, false, &[])))]).rows_for(coloured(), true);
    let clear = DogRows(vec![dog(None)]).rows_for(coloured(), true);
    let last = |rows: &[Vec<String>]| rows[0].last().unwrap().clone();

    assert_eq!(last(&flagged), painted("undeclared", Role::Butter));
    assert_eq!(last(&clear), painted("-", Role::Ink3));
}

#[test]
fn marks_that_are_all_clear_read_as_a_muted_dash() {
    let rows = DogRows(vec![dog(Some(DogAccess::default()))]).rows_for(coloured(), true);

    assert_eq!(rows[0].last().unwrap(), &painted("-", Role::Ink3));
}
