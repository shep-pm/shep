use shep_core::dogs::pane::{Destructive, Pane, Row};

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

#[test]
fn the_layout_counts_the_lines_the_view_draws_and_where_each_row_falls() {
    // blank, host, VRAM, blank, models, header, a, b, blank, leases, header, c
    let at = layout(&two_tables());
    assert_eq!(at.total, 12);
    assert_eq!(at.row_lines, [6, 7, 11]);
}

#[test]
fn a_tab_scrolls_past_its_cursor_only_once_it_knows_its_room() {
    let pane = Pane::new().fields("host", (0..30).map(|n| (format!("f{n}"), format!("v{n}"))));
    let mut status = status_with(pane);
    status.step(Step::Down);
    assert_eq!(
        status.scroll(),
        None,
        "no room is known before the first draw"
    );

    status.set_rows(10);
    status.step(Step::Last);
    assert_eq!(
        status.scroll(),
        Some(32 - 8),
        "the window ends on the last line"
    );
    status.step(Step::Up);
    assert_eq!(status.scroll(), Some(32 - 8 - 1));
    status.step(Step::First);
    assert_eq!(
        status.scroll(),
        None,
        "the top is where the window follows from"
    );
}

fn declared(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn letters(pane: &Pane, names: &[&str]) -> Vec<Option<char>> {
    keyed(pane, &declared(names))
        .iter()
        .map(|key| key.letter)
        .collect()
}

#[test]
fn letters_are_the_first_free_letter_of_each_label_in_declared_order() {
    let pane = Pane::new()
        .table("leases", ["LEASE"], rows(&["a"]))
        .row_action("revoke", "revoke lease", Destructive::Yes)
        .row_action("renew", "renew lease", Destructive::No)
        .action("pause", "pause the board", Destructive::No);

    let keys = keyed(&pane, &declared(&["revoke", "renew", "pause"]));

    let shown: Vec<_> = keys
        .iter()
        .map(|key| (key.letter, key.action.action.as_str(), key.rows.is_some()))
        .collect();
    assert_eq!(
        shown,
        [
            (Some('r'), "revoke", true),
            // `r` is taken, `e` is the label's next letter.
            (Some('e'), "renew", true),
            (Some('p'), "pause", false),
        ]
    );
}

#[test]
fn a_reserved_letter_moves_to_the_next_letter_of_the_label() {
    let pane = Pane::new()
        .action("go", "go", Destructive::No)
        .action("kill", "kill", Destructive::No)
        .action("quiet", "quiet", Destructive::No)
        .action("jump", "jump", Destructive::No);

    // `g`, `k`, `q` and `j` are lookout's own, and `u` goes to "quiet" first.
    assert_eq!(
        letters(&pane, &["go", "kill", "quiet", "jump"]),
        [Some('o'), Some('i'), Some('u'), Some('m')]
    );
}

#[test]
fn a_label_with_no_free_letter_takes_the_first_free_letter_from_a_to_z() {
    let pane = Pane::new()
        .action("a", "a", Destructive::No)
        .action("again", "aa", Destructive::No)
        .action("jk", "jk", Destructive::No);

    // `a` is taken, so "aa" has none of its own; "jk" has only reserved ones.
    assert_eq!(
        letters(&pane, &["a", "again", "jk"]),
        [Some('a'), Some('b'), Some('c')]
    );
}

#[test]
fn an_action_the_manifest_lacks_has_no_letter_and_takes_none() {
    let pane = Pane::new()
        .action("ghost", "reap ghosts", Destructive::No)
        .action("reap", "reap", Destructive::No);

    assert_eq!(letters(&pane, &["reap"]), [None, Some('r')]);
}

#[test]
fn two_panes_assign_their_letters_independently() {
    let first = Pane::new().action("run", "run", Destructive::No);
    let second = Pane::new().action("run", "run", Destructive::No);

    assert_eq!(letters(&first, &["run"]), letters(&second, &["run"]));
    assert_eq!(letters(&first, &["run"]), [Some('r')]);
}

#[test]
fn a_row_action_covers_the_rows_of_its_own_table() {
    let pane = Pane::new()
        .table("models", ["MODEL"], rows(&["a", "b"]))
        .table("leases", ["LEASE"], rows(&["c"]))
        .row_action("revoke", "revoke", Destructive::No);

    let keys = keyed(&pane, &declared(&["revoke"]));

    assert_eq!(keys[0].rows, Some(2..3));
}
