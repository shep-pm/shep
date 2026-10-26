//! A sheep's `dogs` tab: one row per dog that can hold a table on this
//! sheep, what state its table is in, and under an open dog its table.
//!
//! A closed row names its table's keys and never a value. A set table's
//! secrets are marked by a schema the row does not read, and a read-only
//! table has no schema at all, so a row stops at key names; the open table
//! below it is where values are drawn, masked.

use ratatui::text::{Line, Span};

use super::super::super::pane::{ConfigPane, DogTableState, PaneRow};
use super::super::super::theme::Palette;
use super::super::flock::{fit, mark};
use super::super::scroll::{self, Attempt};
use super::body::row_line;
use super::field_row::open_glyph;
use super::layout::body_width;

/// The widest a dog's name column grows before it is cut.
const NAME_W: u16 = 16;
/// `read-only`, the widest state word.
const STATE_W: u16 = 9;
/// How far an open table's rows sit in from the dog's own.
const INDENT: u16 = 2;

/// The tab's body: the armed removal's question when there is one, then the
/// rows, laid out through [`scroll::to_cursor`] so the cursor is drawn at
/// every height.
pub(super) fn dogs_body_lines(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
    show_lands: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(dog) = pane.dogs_tab().armed() {
        let sheep = pane.target().name();
        lines.push(Line::from(Span::styled(
            format!(
                "  {}",
                fit(
                    &format!("remove {sheep}'s {dog} table? enter confirms, any other key cancels"),
                    body_width(width)
                )
            ),
            palette.attention(),
        )));
    }
    let body_budget = budget.saturating_sub(lines.len());
    if body_budget == 0 {
        return lines;
    }
    let rows = pane.rows();
    if rows.is_empty() {
        let text = if pane.dogs_tab().is_listed() {
            "no dog publishes a sheep schema, and this sheep carries no table"
        } else {
            "listing dogs..."
        };
        lines.push(Line::from(Span::styled(
            format!("  {}", fit(text, body_width(width))),
            palette.muted(),
        )));
        return lines;
    }
    let cursor_row = pane.view().cursor().min(rows.len() - 1);
    lines.extend(scroll::to_cursor(
        cursor_row,
        pane.view().offset(),
        |offset| dogs_body_from(pane, &rows, palette, width, body_budget, offset, show_lands),
        || {
            vec![tab_row_line(
                pane,
                rows[cursor_row],
                true,
                width,
                palette,
                show_lands,
            )]
        },
    ));
    lines
}

/// One row of the tab: a dog's own, or one of its open table's.
fn tab_row_line(
    pane: &ConfigPane,
    row: PaneRow,
    selected: bool,
    width: u16,
    palette: Palette,
    show_lands: bool,
) -> Line<'static> {
    match row {
        PaneRow::Dog(index) => dog_line(pane, index, selected, width, palette),
        PaneRow::Under { dog, at } => {
            let Some(table) = pane.dogs_tab().rows().get(dog).and_then(|row| row.table()) else {
                return Line::default();
            };
            let Some(inner) = table.rows().get(at).copied() else {
                return Line::default();
            };
            let mut line = row_line(
                table,
                inner,
                selected,
                width.saturating_sub(INDENT),
                palette,
                show_lands,
            );
            // After the selection mark, so the mark stays in its column.
            line.spans.insert(
                1.min(line.spans.len()),
                Span::raw(" ".repeat(usize::from(INDENT))),
            );
            line
        }
        PaneRow::Field(_)
        | PaneRow::Table(_)
        | PaneRow::Entry(_)
        | PaneRow::Choice { .. }
        | PaneRow::Arm { .. }
        | PaneRow::Env(_)
        | PaneRow::AddEnv => Line::default(),
    }
}

/// One dog's row: the selection mark, whether it is open, its name, its
/// table's state, and what the table holds, by key name only.
fn dog_line(
    pane: &ConfigPane,
    index: usize,
    selected: bool,
    width: u16,
    palette: Palette,
) -> Line<'static> {
    let Some(row) = pane.dogs_tab().rows().get(index) else {
        return Line::default();
    };
    let body = body_width(width);
    let name_w = NAME_W.min(body.saturating_sub(2));
    let state_w = STATE_W.min(body.saturating_sub(name_w + 4));
    let rest_w = body.saturating_sub(name_w + state_w + 6);
    let keys = row.keys().join(", ");
    let (state, rest, muted) = if row.is_waiting() {
        ("asking", String::new(), true)
    } else {
        match row.state() {
            DogTableState::Set => ("set", keys, false),
            DogTableState::Unset => ("-", "(no table)".to_owned(), true),
            DogTableState::ReadOnly => (
                "read-only",
                format!("{keys}  (publishes no sheep schema)"),
                true,
            ),
            DogTableState::NoSchema => ("-", "(publishes no sheep schema)".to_owned(), true),
        }
    };
    let mut text = format!("{} ", open_glyph(row.is_open()));
    text.push_str(&fit(row.name(), name_w));
    if state_w > 0 {
        text.push_str("  ");
        text.push_str(&fit(state, state_w));
    }
    if rest_w > 0 {
        text.push_str("  ");
        text.push_str(&fit(&rest, rest_w));
    }
    let text = format!("{}{text}", mark(selected));
    if muted {
        Line::from(Span::styled(text, palette.muted()))
    } else {
        Line::from(Span::raw(text))
    }
}

/// Lays the rows out from `offset`, spending at most `budget` lines, both
/// markers reserved before a row is admitted, as the list sub-screen does.
fn dogs_body_from(
    pane: &ConfigPane,
    rows: &[PaneRow],
    palette: Palette,
    width: u16,
    budget: usize,
    offset: usize,
    show_lands: bool,
) -> Attempt {
    let total = rows.len();
    let cursor_row = pane.view().cursor().min(total.saturating_sub(1));
    let above = usize::from(offset > 0);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut drawn = 0usize;
    for (index, row) in rows.iter().enumerate().skip(offset) {
        if lines.len() + 1 + above + usize::from(index + 1 < total) > budget {
            break;
        }
        lines.push(tab_row_line(
            pane,
            *row,
            index == cursor_row,
            width,
            palette,
            show_lands,
        ));
        drawn += 1;
    }
    let hidden_below = total.saturating_sub(offset + drawn);
    if hidden_below > 0 {
        lines.push(Line::from(Span::styled(
            format!("  ... {hidden_below} below"),
            palette.muted(),
        )));
    }
    if offset > 0 {
        lines.insert(
            0,
            Line::from(Span::styled(
                format!("  ... {offset} above"),
                palette.muted(),
            )),
        );
    }
    Attempt {
        cursor_drawn: drawn > 0 && (offset..offset + drawn).contains(&cursor_row),
        lines,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use shep_core::config::AppConfig;
    use shep_core::protocol::SheepConfigView;

    use super::super::draw::pane_lines;
    use super::super::fixtures::{marked, text_of};
    use crate::lookout::pane::{ConfigPane, ListedDog, SheepDogEntry};
    use crate::lookout::view::fixtures;

    const TOKEN: &str = "sk-live-51Hx9Qa";
    const PASSWORD: &str = "hunter2-legacy";

    fn schema() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "concurrency": { "type": "integer" },
                "token": { "type": "string", "x-shep-secret": true },
            },
        })
    }

    fn listed(name: &str, schema: Option<serde_json::Value>) -> ListedDog {
        ListedDog {
            entry: SheepDogEntry {
                name: name.to_owned(),
                adopted_path: None,
                schema,
            },
            asked: true,
        }
    }

    /// `web` on its `dogs` tab: `jobs` set, `deploy` unset, `legacy`
    /// read-only, every row collapsed.
    fn web_dogs() -> ConfigPane {
        let mut config = AppConfig {
            name: "web".into(),
            ..AppConfig::default()
        };
        let table = |value: serde_json::Value| value.as_object().cloned().expect("a table");
        config.dogs.insert(
            "jobs".into(),
            table(json!({ "concurrency": 7, "token": TOKEN })).into(),
        );
        config.dogs.insert(
            "legacy".into(),
            table(json!({ "url": format!("https://ops:{PASSWORD}@example.test") })).into(),
        );
        let mut pane = ConfigPane::sheep(SheepConfigView::new(config, Vec::new(), Vec::new()));
        pane.set_group(9);
        pane.list_dogs(vec![
            listed("jobs", Some(schema())),
            listed(
                "deploy",
                Some(json!({ "type": "object", "properties": {} })),
            ),
            listed("legacy", None),
        ]);
        pane
    }

    fn drawn(pane: &ConfigPane, width: u16, height: u16) -> String {
        text_of(&pane_lines(pane, fixtures::plain(), width, height)).join("\n")
    }

    #[test]
    fn a_collapsed_row_names_its_state_and_key_names_and_never_a_value() {
        let text = drawn(&web_dogs(), 120, 0);
        let row = |dog: &str| {
            text.lines()
                .find(|line| line.contains(dog))
                .unwrap_or_else(|| panic!("{dog} is drawn: {text}"))
        };
        assert!(row("deploy").contains("(no table)"), "{text}");
        assert!(
            row("jobs").contains("set") && row("jobs").contains("concurrency, token"),
            "{text}"
        );
        assert!(
            row("legacy").contains("read-only") && row("legacy").contains("url"),
            "{text}"
        );
        for secret in [TOKEN, PASSWORD, "7"] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
    }

    #[test]
    fn an_open_dog_draws_its_table_under_its_row_with_the_secret_masked() {
        let mut pane = web_dogs();
        let jobs = pane
            .dogs_tab()
            .rows()
            .iter()
            .position(|row| row.name() == "jobs")
            .expect("listed");
        pane.toggle_dog(jobs);
        let text = drawn(&pane, 120, 0);
        let lines: Vec<&str> = text.lines().collect();
        let head = lines
            .iter()
            .position(|line| line.contains("\u{25be}") && line.contains("jobs"))
            .unwrap_or_else(|| panic!("jobs is open: {text}"));
        let concurrency = lines[head + 1];
        assert!(
            concurrency.contains("concurrency") && concurrency.contains('7'),
            "{concurrency:?}"
        );
        let indent = |line: &str| line.len() - line.trim_start().len();
        assert!(indent(concurrency) > indent(lines[head]), "{text}");
        let token = lines[head + 2];
        assert!(
            token.contains("token") && token.contains("<set>"),
            "{token:?}"
        );
        assert!(!text.contains(TOKEN), "{text}");
    }

    #[test]
    fn the_cursor_row_is_drawn_at_every_height() {
        let mut pane = web_dogs();
        pane.set_rows(3);
        pane.move_to_last();
        for height in 3..=8 {
            let text = drawn(&pane, 120, height);
            assert_eq!(marked(&text), 1, "height {height}: {text}");
        }
    }

    #[test]
    fn a_tab_with_no_dogs_says_so() {
        let mut pane = ConfigPane::sheep(SheepConfigView::new(
            AppConfig {
                name: "web".into(),
                ..AppConfig::default()
            },
            Vec::new(),
            Vec::new(),
        ));
        pane.set_group(9);
        assert!(drawn(&pane, 120, 0).contains("listing dogs..."));
        pane.list_dogs(Vec::new());
        assert!(drawn(&pane, 120, 0).contains("no dog publishes a sheep schema"));
    }
}
