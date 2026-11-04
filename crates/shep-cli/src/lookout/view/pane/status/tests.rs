use shep_core::dogs::pane::{Destructive, Pane, Row};

use super::super::draw::pane_lines;
use super::super::fixtures::text_of;
use crate::lookout::app::{App, KeyPress, Msg};
use crate::lookout::view::MIN_TERM_WIDTH;
use crate::lookout::view::fixtures;
use crate::output::width::visible_width;
use shep_core::protocol::ActionOutcome;

/// The pane the design draws: two host fields and a models table.
fn paddock() -> Pane {
    Pane::new()
        .fields("host", [("VRAM", "20 / 24 GiB"), ("RAM", "41 / 63 GiB")])
        .table(
            "models",
            ["MODEL", "STATE", "FOOTPRINT"],
            [
                Row::new("qwen3-32b", ["qwen3-32b", "loaded", "19.8 GiB"]),
                Row::new("strata-7b", ["strata-7b", "idle", "6.1 GiB"]),
            ],
        )
}

fn app_with(pane: &Pane) -> App {
    fixtures::app_in_pane_dog(Some(fixtures::pane_reply(pane)))
}

fn drawn(app: &App, width: u16, height: u16) -> Vec<String> {
    let pane = app.config_pane().expect("the pane is open");
    text_of(&pane_lines(pane, fixtures::plain(), width, height))
}

fn trimmed(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|l| l.trim_end().to_owned()).collect()
}

#[test]
fn the_design_draws_at_100_columns() {
    let lines = trimmed(drawn(&app_with(&paddock()), 100, 0));
    // `  [status]  settings` is 20 columns and `tab ⇆` 5, ending at column 98.
    let tab_strip = format!(
        "  [status]  settings{}tab \u{21c6}",
        " ".repeat(98 - 20 - 5)
    );
    assert_eq!(
        lines,
        [
            "  paddock  (dog config)",
            &tab_strip,
            "",
            "  host",
            "    VRAM  20 / 24 GiB",
            "    RAM   41 / 63 GiB",
            "",
            "  models",
            "    MODEL      STATE   FOOTPRINT",
            "\u{25b8}   qwen3-32b  loaded  19.8 GiB",
            "    strata-7b  idle    6.1 GiB",
        ]
    );
}

#[test]
fn the_design_draws_at_80_columns() {
    let lines = trimmed(drawn(&app_with(&paddock()), 80, 0));
    assert_eq!(
        visible_width(&lines[1]),
        78,
        "the strip ends at the body's edge"
    );
    assert!(lines[1].starts_with("  [status]  settings"));
    assert!(lines[1].ends_with("tab \u{21c6}"));
    assert_eq!(lines[8], "    MODEL      STATE   FOOTPRINT");
    assert_eq!(lines[9], "\u{25b8}   qwen3-32b  loaded  19.8 GiB");
}

#[test]
fn the_design_draws_at_the_narrow_floor_and_every_line_fits() {
    let lines = drawn(&app_with(&paddock()), MIN_TERM_WIDTH, 0);
    let lines = trimmed(lines);
    assert_eq!(lines[9], "\u{25b8}   qwen3-32b  loaded  19.8 GiB");
    for width in MIN_TERM_WIDTH..=120 {
        for line in drawn(&app_with(&paddock()), width, 0) {
            assert!(
                visible_width(&line) <= usize::from(width),
                "width {width}: {line:?}"
            );
        }
    }
}

#[test]
fn a_table_that_cannot_fit_drops_its_right_most_columns_and_says_so() {
    let pane = Pane::new().table(
        "models",
        ["MODEL", "STATE", "FOOTPRINT", "LEASED BY"],
        [Row::new(
            "a",
            ["qwen3-32b", "loaded", "19.8 GiB", "kelpie-worker"],
        )],
    );
    let lines = trimmed(drawn(&app_with(&pane), MIN_TERM_WIDTH, 0));
    assert!(
        lines.contains(&"    MODEL      STATE   \u{2026}".to_string()),
        "{lines:#?}"
    );
    assert!(
        lines.contains(&"\u{25b8}   qwen3-32b  loaded".to_string()),
        "{lines:#?}"
    );
}

#[test]
fn the_cursor_row_is_marked_by_text_and_moves_with_the_key() {
    let mut app = app_with(&paddock());
    app.update(Msg::Key(KeyPress::SelectDown));
    let lines = trimmed(drawn(&app, 100, 0));
    assert_eq!(lines[9], "    qwen3-32b  loaded  19.8 GiB");
    assert_eq!(lines[10], "\u{25b8}   strata-7b  idle    6.1 GiB");
}

#[test]
fn the_cursor_row_is_in_reverse_video() {
    let app = app_with(&paddock());
    let pane = app.config_pane().unwrap();
    let lines = pane_lines(pane, fixtures::plain(), 100, 0);
    let reversed = |line: &ratatui::text::Line<'_>| {
        line.spans.iter().any(|span| {
            span.style
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        })
    };
    assert!(reversed(&lines[9]));
    assert!(!reversed(&lines[10]));
}

#[test]
fn a_cell_with_an_escape_or_a_wide_string_draws_cleaned_and_cut() {
    let wide = "w".repeat(90);
    let pane = Pane::new()
        .fields("host", [("VR\u{1b}[2JAM", "20\u{7} GiB")])
        .table(
            "models",
            ["MODEL", "NOTE"],
            [Row::new("a", ["q\u{1b}[31mwen\nx".to_string(), wide])],
        );
    let lines = drawn(&app_with(&pane), 100, 0);
    let all = lines.join("\n");
    assert!(!all.contains('\u{1b}'), "{all:?}");
    assert!(!all.contains('\u{7}'), "{all:?}");
    assert!(all.contains("q[31mwen x"), "{all:?}");
    assert!(all.contains("VR[2JAM"), "{all:?}");
    let cut = lines
        .iter()
        .find(|l| l.contains("www"))
        .expect("the wide cell");
    assert!(cut.trim_end().ends_with('\u{2026}'), "{cut:?}");
    assert!(!cut.contains(&"w".repeat(41)), "{cut:?}");
}

#[test]
fn a_section_with_no_rows_says_none() {
    let pane = Pane::new()
        .fields("host", Vec::<(String, String)>::new())
        .table("models", ["MODEL"], []);
    let lines = trimmed(drawn(&app_with(&pane), 100, 0));
    assert_eq!(
        &lines[3..],
        ["  host", "    none", "", "  models", "    none"]
    );
}

#[test]
fn a_pane_with_one_skipped_section_draws_the_rest_and_says_so() {
    let body = serde_json::json!({
        "sections": [
            { "kind": "chart", "id": "cpu", "title": "cpu" },
            { "kind": "fields", "id": "host", "title": "host",
              "fields": [{ "label": "VRAM", "value": "20 GiB" }] },
        ],
    })
    .to_string();
    let app = fixtures::app_in_pane_dog(Some(ActionOutcome::Replied { body }));
    let lines = trimmed(drawn(&app, 100, 0));
    assert!(
        lines.contains(&"    VRAM  20 GiB".to_string()),
        "{lines:#?}"
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("  1 section could not be drawn: this lookout does not know its kind"),
        "{lines:#?}"
    );
}

#[test]
fn several_skipped_sections_are_counted() {
    let body = serde_json::json!({
        "sections": [{ "kind": "a" }, { "kind": "b" }],
    })
    .to_string();
    let app = fixtures::app_in_pane_dog(Some(ActionOutcome::Replied { body }));
    let lines = trimmed(drawn(&app, 100, 0));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("2 sections could not be drawn")),
        "{lines:#?}"
    );
}

#[test]
fn waiting_says_who_is_being_asked() {
    let app = fixtures::app_in_pane_dog(None);
    let lines = trimmed(drawn(&app, 100, 0));
    assert_eq!(lines[3], "  asking paddock", "{lines:#?}");
}

#[test]
fn unavailable_draws_the_reason_the_dog_gave_and_settings_still_draws() {
    let mut app = fixtures::app_in_pane_dog(Some(ActionOutcome::Replied {
        body: "<html>nope</html>".to_string(),
    }));
    let lines = drawn(&app, 100, 0);
    let all = lines.join("\n");
    assert!(
        all.contains("the dog answered with something shep cannot read: <html>nope</html>"),
        "{all}"
    );

    app.update(Msg::Key(KeyPress::NextGroup));
    let lines = drawn(&app, 100, 0);
    assert!(lines[1].contains("status  [settings]"), "{:?}", lines[1]);
    assert!(
        lines.iter().any(|line| line.contains("ratio")),
        "the settings form draws: {lines:#?}"
    );
}

#[test]
fn a_timeout_is_one_line_on_the_status_tab() {
    let app = fixtures::app_in_pane_dog(Some(ActionOutcome::TimedOut));
    let lines = trimmed(drawn(&app, 100, 0));
    assert_eq!(lines[3], "  paddock did not answer in time", "{lines:#?}");
}

#[test]
fn a_taller_pane_than_the_box_scrolls_with_the_cursor() {
    let rows: Vec<Row> = (0..20)
        .map(|n| Row::new(format!("k{n}"), [format!("row-{n}")]))
        .collect();
    let mut app = app_with(&Pane::new().table("things", ["NAME"], rows));
    let height = 10;
    let first = drawn(&app, 100, height);
    assert_eq!(first.len(), usize::from(height));
    assert!(first.iter().any(|l| l.contains("row-0")), "{first:#?}");

    app.update(Msg::Key(KeyPress::SelectLast));
    let last = trimmed(drawn(&app, 100, height));
    assert_eq!(last.len(), usize::from(height));
    assert!(
        last.iter()
            .any(|l| l.starts_with('\u{25b8}') && l.contains("row-19")),
        "{last:#?}"
    );
    assert!(last.iter().any(|l| l.contains("above")), "{last:#?}");
    assert!(
        last[1].contains("[status]"),
        "the tab strip stays: {last:#?}"
    );
}

#[test]
fn the_cursor_draws_at_the_height_floor() {
    let rows: Vec<Row> = (0..20)
        .map(|n| Row::new(format!("k{n}"), [format!("row-{n}")]))
        .collect();
    let mut app = app_with(&Pane::new().table("things", ["NAME"], rows));
    for _ in 0..7 {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
    for height in 1..=crate::lookout::view::flock::MIN_HEIGHT {
        let lines = trimmed(drawn(&app, MIN_TERM_WIDTH, height));
        assert!(lines.len() <= usize::from(height), "{height}: {lines:#?}");
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with('\u{25b8}') && l.contains("row-7")),
            "height {height}: {lines:#?}"
        );
    }
}

fn pairs(count: usize) -> Vec<(String, String)> {
    (0..count)
        .map(|n| (format!("field-{n}"), format!("value-{n}")))
        .collect()
}

fn press(app: &mut App, key: KeyPress, times: usize) {
    for _ in 0..times {
        app.update(Msg::Key(key));
    }
}

fn shows(lines: &[String], text: &str) -> bool {
    lines.iter().any(|line| line.contains(text))
}

#[test]
fn a_pane_with_no_rows_scrolls_to_its_last_field_and_back() {
    let mut app = app_with(&Pane::new().fields("host", pairs(30)));
    let height = 10;
    app.note_body_rows(height);
    let top = drawn(&app, 100, height);
    assert!(shows(&top, "field-0"), "{top:#?}");
    assert!(shows(&top, "below"), "{top:#?}");

    press(&mut app, KeyPress::SelectDown, 40);
    let bottom = drawn(&app, 100, height);
    assert!(shows(&bottom, "field-29"), "{bottom:#?}");
    assert!(shows(&bottom, "above"), "{bottom:#?}");
    assert!(!shows(&bottom, "below"), "{bottom:#?}");

    press(&mut app, KeyPress::SelectUp, 1);
    let up_one = drawn(&app, 100, height);
    assert_ne!(up_one, bottom, "one key back moves the window at once");

    press(&mut app, KeyPress::SelectUp, 40);
    assert_eq!(drawn(&app, 100, height), top);
}

#[test]
fn first_and_last_jump_a_pane_with_no_rows_to_its_ends() {
    let mut app = app_with(&Pane::new().fields("host", pairs(30)));
    let height = 10;
    app.note_body_rows(height);
    press(&mut app, KeyPress::SelectLast, 1);
    assert!(shows(&drawn(&app, 100, height), "field-29"));
    press(&mut app, KeyPress::SelectFirst, 1);
    assert!(shows(&drawn(&app, 100, height), "field-0"));
}

#[test]
fn fields_after_the_last_table_row_scroll_into_view() {
    let rows: Vec<Row> = (0..3)
        .map(|n| Row::new(format!("k{n}"), [format!("row-{n}")]))
        .collect();
    let pane = Pane::new()
        .table("things", ["NAME"], rows)
        .fields("host", pairs(30));
    let mut app = app_with(&pane);
    let height = 10;
    app.note_body_rows(height);
    press(&mut app, KeyPress::SelectLast, 1);
    press(&mut app, KeyPress::SelectDown, 60);
    let bottom = drawn(&app, 100, height);
    assert!(shows(&bottom, "field-29"), "{bottom:#?}");

    press(&mut app, KeyPress::SelectUp, 60);
    let back = trimmed(drawn(&app, 100, height));
    assert!(
        back.iter().any(|l| l.starts_with('\u{25b8}')),
        "scrolling back reaches the cursor, which then moves: {back:#?}"
    );
}

#[test]
fn fields_before_the_first_table_row_scroll_into_view() {
    let rows: Vec<Row> = (0..3)
        .map(|n| Row::new(format!("k{n}"), [format!("row-{n}")]))
        .collect();
    let pane = Pane::new()
        .fields("host", pairs(30))
        .table("things", ["NAME"], rows);
    let mut app = app_with(&pane);
    let height = 10;
    app.note_body_rows(height);
    let first = drawn(&app, 100, height);
    assert!(
        shows(&first, "row-0"),
        "the cursor's row is drawn: {first:#?}"
    );

    press(&mut app, KeyPress::SelectUp, 60);
    let top = drawn(&app, 100, height);
    assert!(shows(&top, "field-0"), "{top:#?}");

    press(&mut app, KeyPress::SelectDown, 60);
    let back = trimmed(drawn(&app, 100, height));
    assert!(
        back.iter().any(|l| l.starts_with('\u{25b8}')),
        "scrolling back reaches the cursor, which then moves: {back:#?}"
    );
}

/// Two leases with a row action each and a pane action.
fn leases() -> Pane {
    Pane::new()
        .table(
            "leases",
            ["LEASE"],
            ["qwen3-32b", "strata-7b"].map(|key| Row::new(key, [key])),
        )
        .row_action("revoke", "revoke lease", Destructive::Yes)
        .row_action("renew", "renew lease", Destructive::No)
        .action("pause", "pause the board", Destructive::No)
}

fn app_declaring(pane: &Pane, actions: &[&str]) -> App {
    fixtures::app_in_pane_dog_declaring(actions, Some(fixtures::pane_reply(pane)))
}

#[test]
fn the_footer_shows_each_letter_with_its_label_and_marks_row_actions() {
    let pane = Pane::new()
        .table("leases", ["LEASE"], [Row::new("qwen3-32b", ["qwen3-32b"])])
        .row_action("revoke", "revoke lease", Destructive::Yes)
        .action("pause", "pause the board", Destructive::No);
    let app = app_declaring(&pane, &["revoke", "pause"]);

    for width in [80, 100] {
        let lines = trimmed(drawn(&app, width, 0));
        assert_eq!(
            lines.last().map(String::as_str),
            Some("  r revoke lease (row)  p pause the board"),
            "width {width}: {lines:#?}"
        );
    }
}

#[test]
fn the_footer_drops_the_actions_that_do_not_fit_and_says_so_at_the_narrow_floor() {
    let app = app_declaring(&leases(), &["revoke", "renew", "pause"]);

    let lines = drawn(&app, MIN_TERM_WIDTH, 0);

    let footer = lines.last().expect("the pane draws").trim_end().to_owned();
    assert_eq!(footer, "  r revoke lease (row)  \u{2026}");
    for width in MIN_TERM_WIDTH..=120 {
        for line in drawn(&app, width, 0) {
            assert!(
                visible_width(&line) <= usize::from(width),
                "width {width}: {line:?}"
            );
        }
    }
}

#[test]
fn an_action_the_manifest_lacks_is_named_and_gets_no_letter() {
    let app = app_declaring(&leases(), &["renew", "pause"]);

    let lines = trimmed(drawn(&app, 100, 0));

    assert!(
        lines.contains(&"  revoke is not in the manifest".to_owned()),
        "{lines:#?}"
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("  r renew lease (row)  p pause the board"),
        "{lines:#?}"
    );
}

#[test]
fn the_last_result_shows_above_the_actions() {
    let mut app = app_declaring(&leases(), &["revoke", "renew", "pause"]);
    app.update(Msg::Key(KeyPress::TextChar('p')));
    app.update(Msg::Replied {
        sent: crate::lookout::app::Sent::DogPaneAction {
            name: "paddock".to_owned(),
            action: "pause".to_owned(),
            row: None,
        },
        result: Ok(shep_core::protocol::Response::Triggered(vec![
            shep_core::protocol::ActionReply {
                id: 92,
                name: "paddock".to_owned(),
                outcome: ActionOutcome::Replied {
                    body: "paused".to_owned(),
                },
            },
        ])),
    });

    let lines = trimmed(drawn(&app, 100, 0));

    let at = lines.len() - 2;
    assert_eq!(lines[at], "  paused", "{lines:#?}");
}

#[test]
fn a_short_box_keeps_the_actions_line_and_drops_the_older_notes() {
    let app = app_declaring(&leases(), &["renew", "pause"]);

    let lines = trimmed(drawn(&app, 100, 4));

    assert_eq!(lines.len(), 4, "{lines:#?}");
    assert_eq!(
        lines.last().map(String::as_str),
        Some("  r renew lease (row)  p pause the board"),
        "{lines:#?}"
    );
}
