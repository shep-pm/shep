use super::super::super::app::{REVEAL_HOLDS, Reveal, SecretsPane, TypingWhat};
use super::super::super::secrets::SecretRow;
use super::super::super::theme::Palette;
use super::super::flock::fit;
use super::column_tiers::Column;
use crate::output::width::char_columns;
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

/// The last `width` columns of `text`, `\u{2026}` standing in for whatever it
/// dropped off the front, padded to `width` the way [`fit`] pads.
///
/// [`fit`]'s own truncation keeps the head, which is right for a stored
/// value and wrong for one being typed: the cursor sits at the end, so the
/// characters an operator is putting on screen right now are exactly the
/// ones `fit` drops.
pub(super) fn tail(text: &str, width: u16) -> String {
    let width = usize::from(width);
    let columns: usize = text.chars().map(char_columns).sum();
    if columns <= width {
        let mut out = String::from(text);
        out.extend(core::iter::repeat_n(' ', width - columns));
        return out;
    }
    if width == 0 {
        return String::new();
    }
    // One column pays for the `\u{2026}`, and a double-width character
    // straddling the boundary is dropped rather than split: `fit`'s own two
    // rules, read from the other end.
    let budget = width - 1;
    let mut kept = String::new();
    let mut used = 0;
    for c in text.chars().rev() {
        let c_width = char_columns(c);
        if used + c_width > budget {
            break;
        }
        kept.push(c);
        used += c_width;
    }
    let mut out = String::from("\u{2026}");
    out.extend(core::iter::repeat_n(' ', budget - used));
    out.extend(kept.chars().rev());
    out
}

/// A buffer being typed, with the block cursor after it, in a cell one
/// column short of `width` so the cursor never touches the next column's
/// own text. [`tail`], not [`fit`], for the reason [`tail`] gives.
pub(super) fn typed_cell(buffer: &str, width: u16) -> String {
    tail(&format!("{buffer}\u{2588}"), width.saturating_sub(1))
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
    typing: Option<&str>,
    width: u16,
) -> String {
    if let Some(buffer) = typing {
        return fit(&typed_cell(buffer, width), width);
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
    typing: Option<&str>,
    environment_count: usize,
    now: Instant,
) -> String {
    match column {
        Column::Key => row.key.clone(),
        Column::Value => value_cell(row, revealed, typing, column.width()),
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
    let typing = if selected {
        pane.typing.as_ref().and_then(|typing| match &typing.what {
            TypingWhat::ValueFor(key) if key == &row.key => Some(typing.buffer.as_str()),
            TypingWhat::ValueFor(_) | TypingWhat::NewKey => None,
        })
    } else {
        None
    };
    let environment_count = pane.model.environments.len();
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(columns.len());
    let mut used = 0u16;
    for column in columns {
        let text = fit(
            &row_cell(row, *column, revealed, typing, environment_count, now),
            column.width(),
        );
        spans.push(Span::styled(text, ground));
        used += column.width();
    }
    pad(&mut spans, used, width, ground);
    Line::from(spans)
}

/// Pads `spans` out to `width` with blank cells, the way a data row's
/// trailing slack past its last column still has to be blanked. Mirrors
/// `flock::pad_ground`, private there.
pub(super) fn pad(spans: &mut Vec<Span<'static>>, used: u16, width: u16, style: Style) {
    let short = width.saturating_sub(used);
    if short > 0 {
        spans.push(Span::styled(" ".repeat(usize::from(short)), style));
    }
}

/// The `+ new key` row's own [`Line`]: `KEY` names the affordance, `VALUE`
/// echoes the name step's own buffer while it is open, and every other
/// column is blank: there is no key yet for any of them to describe.
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
    let typing_name = selected
        .then_some(pane.typing.as_ref())
        .flatten()
        .and_then(|typing| {
            matches!(typing.what, TypingWhat::NewKey).then_some(typing.buffer.as_str())
        });
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(columns.len());
    let mut used = 0u16;
    for column in columns {
        let text = match column {
            Column::Key => "+ new key".to_string(),
            Column::Value => {
                typing_name.map_or_else(String::new, |buffer| typed_cell(buffer, column.width()))
            }
            Column::InForce | Column::SetIn | Column::ReadBy | Column::Lands => String::new(),
        };
        spans.push(Span::styled(fit(&text, column.width()), ground));
        used += column.width();
    }
    pad(&mut spans, used, width, ground);
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::super::column_tiers::Column;
    use super::super::draw_layout::cell;
    use super::super::pane_chrome::tab_line;

    use super::super::super::super::theme::Palette;

    use crate::output::width::char_columns;

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

    /// A double-width character straddling the boundary is dropped rather
    /// than split, the same rule `fit` keeps, read from the other end.
    #[test]
    fn the_tail_drops_a_wide_character_rather_than_splitting_it() {
        // Ten columns of hiragana. In nine the `…` pays one and the last
        // four characters spend the other eight exactly.
        let exact = tail("\u{3042}\u{3044}\u{3046}\u{3048}\u{304a}", 9);
        assert_eq!(exact, "\u{2026}\u{3044}\u{3046}\u{3048}\u{304a}");
        assert_eq!(exact.chars().map(char_columns).sum::<usize>(), 9);

        // In eight, seven columns are left for characters worth two each:
        // the fourth from the end is dropped whole and its odd column is
        // padded rather than half-drawn.
        let padded = tail("\u{3042}\u{3044}\u{3046}\u{3048}\u{304a}", 8);
        assert_eq!(padded, "\u{2026} \u{3046}\u{3048}\u{304a}");
        assert_eq!(padded.chars().map(char_columns).sum::<usize>(), 8);
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
