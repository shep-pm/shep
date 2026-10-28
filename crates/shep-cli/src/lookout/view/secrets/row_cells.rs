use super::super::super::app::{REVEAL_HOLDS, Reveal, SecretsPane};
use super::super::super::keyvalue::{Field, KeyValues};
use super::super::super::secrets::SecretRow;
use super::super::super::textbox::TextBox;
use super::super::super::theme::Palette;
use super::super::flock::{fit, fit_owned, pad_ground};
use super::column_tiers::Column;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use std::time::Instant;

/// `len` in bytes, singular when there is one of them.
pub(super) fn byte_count(len: usize) -> String {
    if len == 1 {
        "1 byte".to_string()
    } else {
        format!("{len} bytes")
    }
}

/// A buffer being typed, with its cursor drawn where it is, in a cell one
/// column short of `width` so the cursor never touches the next column's
/// own text. The box scrolls to keep the cursor in view.
pub(super) fn typed_cell(buffer: &TextBox, width: u16) -> String {
    buffer.drawn(width.saturating_sub(1))
}

/// One field of the open editor, drawn for a cell `width` wide.
///
/// The cursor is drawn only in the field that holds it, which is how the row
/// says which of the two the operator is typing into. The unfocused field
/// still draws what has been typed in it: a rename is unreadable if the key
/// being renamed disappears the moment `Tab` moves off it.
fn field_cell(editor: &KeyValues, field: Field, width: u16) -> String {
    let buffer = match field {
        Field::Key => editor.key(),
        Field::Value => editor.value(),
    };
    if editor.focus() == field {
        typed_cell(buffer, width)
    } else {
        buffer.as_str().to_owned()
    }
}

/// What one row shows in `VALUE`.
///
/// A run proportional to the value's length rather than equal to it: the
/// column is 30 cells against `MAX_VALUE_BYTES`'s 4096, so an equal run
/// cannot be drawn. It stops one cell short of `width` so it never touches
/// `IN FORCE`'s own text.
///
/// `typing` wins over a reveal: the operator is looking at what they are
/// about to send, not at what the store already holds. The block run and a
/// reveal both name a length or a plaintext already on screen; typed text is
/// the same kind of thing, one keystroke ahead of the store.
pub(super) fn value_cell(
    row: &SecretRow,
    revealed: Option<&Reveal>,
    typed: Option<String>,
    width: u16,
) -> String {
    if let Some(typed) = typed {
        return fit(&typed, width);
    }
    if let Some(reveal) = revealed {
        return fit(&reveal.value, width);
    }
    let Some(len) = row.byte_len else {
        return "not set here".to_string();
    };
    let suffix = format!(" {}", byte_count(len));
    let run = usize::from(width)
        .saturating_sub(suffix.len())
        .saturating_sub(1)
        .min(len);
    format!("{}{suffix}", "█".repeat(run.max(1)))
}

/// `SET IN`'s text: a numerator against every tab in [`tab_line`](crate::lookout::view::secrets::pane_chrome::tab_line), `all`
/// included, since `all` is a slot a key can hold and a tab an operator can
/// select. Every tab named once each is the common case and needs no list;
/// anything short of that names which ones.
fn set_in_cell(row: &SecretRow, environment_count: usize) -> String {
    if row.set_in.is_empty() {
        return "-".to_string();
    }
    let count = row.set_in.len();
    if environment_count > 0 && count >= environment_count {
        format!("{count} of {environment_count}")
    } else {
        format!(
            "{count} of {environment_count} \u{b7} {}",
            row.set_in.join(", ")
        )
    }
}

/// The reveal countdown's gauge, in cells.
const GAUGE_CELLS: usize = 10;

/// How long a revealed value has left, in words and in blocks.
///
/// The gauge is scaled from the number printed beside it rather than from
/// the duration underneath, so the two can never read a second apart. That
/// number rounds up: a part-second still on screen is a second the operator
/// can still read the value in.
pub(super) fn countdown(until: Instant, now: Instant) -> String {
    let left = until
        .saturating_duration_since(now)
        .as_millis()
        .div_ceil(1000);
    let hold = u128::from(REVEAL_HOLDS.as_secs()).max(1);
    let cells = u128::try_from(GAUGE_CELLS).unwrap_or(0);
    let filled = usize::try_from(left * cells / hold)
        .unwrap_or(0)
        .min(GAUGE_CELLS);
    format!(
        "visible {left}s {}{}",
        "\u{2588}".repeat(filled),
        "\u{2591}".repeat(GAUGE_CELLS - filled)
    )
}

/// One data row's text for `column`.
///
/// `Lands` carries the reveal's own countdown for the revealed row and `-`
/// everywhere else: no [`SecretRow`] field carries a propagation ETA yet,
/// and `-` is `view/detail.rs`'s convention for an absent value.
fn row_cell(
    row: &SecretRow,
    column: Column,
    revealed: Option<&Reveal>,
    editor: Option<&KeyValues>,
    environment_count: usize,
    now: Instant,
) -> String {
    match column {
        Column::Key => editor.map_or_else(
            || row.key.clone(),
            |editor| field_cell(editor, Field::Key, column.width()),
        ),
        Column::Value => value_cell(
            row,
            revealed,
            editor.map(|editor| field_cell(editor, Field::Value, column.width())),
            column.width(),
        ),
        Column::InForce => row.in_force.clone().unwrap_or_else(|| "-".to_string()),
        Column::SetIn => set_in_cell(row, environment_count),
        Column::ReadBy => {
            if row.readers.is_empty() {
                "-".to_string()
            } else {
                let online = row.readers.iter().filter(|reader| reader.online).count();
                format!("{} ({online} online)", row.readers.len())
            }
        }
        Column::Lands => {
            revealed.map_or_else(|| "-".to_string(), |reveal| countdown(reveal.until, now))
        }
    }
}

/// One row's [`Line`], every column packed against the next with no gap
/// (see the module doc): [`GUTTER`](crate::lookout::view::flock::GUTTER) plus `columns` is exactly what `draw_layout::cell`
/// reads back.
pub(super) fn row_line(
    pane: &SecretsPane,
    row: &SecretRow,
    columns: &[Column],
    width: u16,
    palette: Palette,
    selected: bool,
    now: Instant,
) -> Line<'static> {
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    let revealed = pane.reveal.as_ref().filter(|reveal| reveal.key == row.key);
    // The editor belongs to this row when it was opened under this row's key,
    // which is the same fact a rename is measured against: it keeps drawing on
    // the row it opened on while its KEY field names something else.
    let editor = selected
        .then_some(pane.typing.as_ref())
        .flatten()
        .filter(|editor| editor.opened_under() == Some(row.key.as_str()));
    let environment_count = pane.model.environments.len();
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(columns.len());
    let mut used = 0u16;
    for column in columns {
        let text = fit_owned(
            row_cell(row, *column, revealed, editor, environment_count, now),
            column.width(),
        );
        spans.push(Span::styled(text, ground));
        used += column.width();
    }
    pad_ground(&mut spans, used, width, ground);
    Line::from(spans)
}

/// The `+ new key` row's own [`Line`]: `KEY` names the affordance, and while
/// the editor is open its KEY field draws in that column and its VALUE field
/// in `VALUE`. Every other column is blank: there is no key yet for any of
/// them to describe.
pub(super) fn new_key_row_line(
    pane: &SecretsPane,
    columns: &[Column],
    width: u16,
    palette: Palette,
    selected: bool,
) -> Line<'static> {
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    // An editor opened on an existing row never draws here: the add row is
    // under no key, and `opened_under` is how the two are told apart.
    let editor = selected
        .then_some(pane.typing.as_ref())
        .flatten()
        .filter(|editor| editor.opened_under().is_none());
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(columns.len());
    let mut used = 0u16;
    for column in columns {
        let text = match column {
            Column::Key => match editor {
                None => "+ new key".to_string(),
                Some(editor) => field_cell(editor, Field::Key, column.width()),
            },
            Column::Value => match editor {
                None => String::new(),
                Some(editor) => field_cell(editor, Field::Value, column.width()),
            },
            Column::InForce | Column::SetIn | Column::ReadBy | Column::Lands => String::new(),
        };
        spans.push(Span::styled(fit(&text, column.width()), ground));
        used += column.width();
    }
    pad_ground(&mut spans, used, width, ground);
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::super::column_tiers::Column;
    use super::super::draw_layout::cell;
    use super::super::pane_chrome::tab_line;

    use super::super::super::super::theme::Palette;

    use super::*;

    use super::super::testing::*;
    use crate::lookout::view::fixtures;

    /// The marker says which side the hidden tabs are on, so a row showing
    /// the last tab does not look like a row showing every tab.
    #[test]
    fn the_elision_marker_stands_on_the_side_the_tabs_went() {
        let palette = Palette::detect(None, None, None);
        let head = fixtures::rendered(&tab_line(&pane_with_environments(20, 0), palette, 100));
        let tail = fixtures::rendered(&tab_line(&pane_with_environments(20, 19), palette, 100));

        assert!(
            !head.trim_start().starts_with('\u{2026}'),
            "nothing is hidden before the first tab: {head:?}"
        );
        assert!(head.contains('\u{2026}'), "and plenty after it: {head:?}");
        assert!(
            tail.trim_start().starts_with('\u{2026}'),
            "hidden tabs before the last one: {tail:?}"
        );
    }

    /// One row of one byte: the length is printed in two places and both
    /// used to read `1 bytes`.
    #[test]
    fn a_one_byte_value_is_one_byte_in_both_places_that_print_a_length() {
        assert_eq!(byte_count(1), "1 byte");
        assert_eq!(byte_count(0), "0 bytes");
        assert_eq!(byte_count(9), "9 bytes");
    }

    #[test]
    fn a_provider_group_says_it_is_read_only() {
        let app = fixtures::app_with_a_pushed_secret();
        let buffer = fixtures::render(&app, 160, 48);
        let text = fixtures::rows_of(&buffer);

        assert!(
            text.iter().any(|line| line.contains("read-only here")),
            "the group header states it: {text:?}"
        );
    }

    /// `set_in_cell`'s `count >= environment_count` branch: a key set in
    /// every environment slot names none of them, since the count alone
    /// already says so.
    #[test]
    fn a_key_set_everywhere_names_no_environments() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 48);
        let header_count = header_environment_count(&buffer);
        let row = row_of(&buffer, "SET_IN_ALL_THREE");

        assert_eq!(
            cell(&buffer, row, Column::SetIn).trim(),
            format!("{header_count} of {header_count}")
        );
    }
}
