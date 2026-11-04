use shep_core::dogs::pane::{Pane, Row};

use super::*;
use crate::lookout::pane::{DeclaredPane, DogStatus};

fn rows(keys: &[&str]) -> Vec<Row> {
    keys.iter().map(|key| Row::new(*key, [*key])).collect()
}

/// A status holding `pane`, answered.
fn status_with(pane: Pane) -> DogStatus {
    let mut status = DogStatus::new(DeclaredPane::default());
    status.land(Ok(pane));
    status
}

fn two_tables() -> Pane {
    Pane::new()
        .fields("host", [("VRAM", "20 / 24 GiB")])
        .table("models", ["MODEL"], rows(&["a", "b"]))
        .table("leases", ["LEASE"], rows(&["c"]))
}

#[test]
fn a_pane_dog_opens_on_the_status_tab() {
    let status = DogStatus::new(DeclaredPane::default());
    assert_eq!(status.tab(), Tab::Status);
}

#[test]
fn the_cursor_walks_the_rows_of_two_tables_in_order_and_stops_at_the_ends() {
    let mut status = status_with(two_tables());
    assert_eq!(
        status.cursor(),
        Some(0),
        "an answer with rows lands on the first"
    );

    status.step(Step::Up);
    assert_eq!(status.cursor(), Some(0), "stops at the top");

    let mut seen = vec![status.cursor()];
    for _ in 0..4 {
        status.step(Step::Down);
        seen.push(status.cursor());
    }
    assert_eq!(
        seen,
        [Some(0), Some(1), Some(2), Some(2), Some(2)],
        "two rows in the first table, one in the second, then the end"
    );

    status.step(Step::Up);
    assert_eq!(status.cursor(), Some(1));
}

#[test]
fn first_and_last_land_on_the_first_and_last_row_of_all_tables() {
    let mut status = status_with(two_tables());
    status.step(Step::Last);
    assert_eq!(status.cursor(), Some(2));
    status.step(Step::First);
    assert_eq!(status.cursor(), Some(0));
}

#[test]
fn a_pane_with_no_rows_leaves_the_cursor_unset_and_down_does_nothing() {
    let mut status = status_with(Pane::new().fields("host", [("VRAM", "20 / 24 GiB")]).table(
        "models",
        ["MODEL"],
        [],
    ));
    assert_eq!(status.cursor(), None);
    for step in [Step::Down, Step::Up, Step::First, Step::Last] {
        status.step(step);
        assert_eq!(status.cursor(), None, "{step:?}");
    }
}

#[test]
fn a_shorter_answer_pulls_the_cursor_back_onto_a_row() {
    let mut status = status_with(two_tables());
    status.step(Step::Last);
    status.land(Ok(Pane::new().table("models", ["MODEL"], rows(&["a"]))));
    assert_eq!(status.cursor(), Some(0));
    status.land(Ok(Pane::new()));
    assert_eq!(status.cursor(), None);
}

#[test]
fn switching_twice_comes_back_to_the_first_tab() {
    let mut status = DogStatus::new(DeclaredPane::default());
    status.switch_tab();
    assert_eq!(status.tab(), Tab::Settings);
    status.switch_tab();
    assert_eq!(status.tab(), Tab::Status);
}
