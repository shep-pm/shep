use shep_core::dogs::pane::{Pane, Row};

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
