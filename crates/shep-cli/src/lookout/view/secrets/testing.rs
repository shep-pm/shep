//! Fixtures and helpers shared by this module's tests.

use super::super::super::app::{App, SecretsPane};
use crate::lookout::app::{KeyPress, Msg};
use crate::lookout::secrets::SecretsModel;
use crate::lookout::view::fixtures;
use ratatui::buffer::Buffer;
use std::collections::HashSet;

/// The first data row `draw` places, fixed regardless of which source
/// the first group is.
/// `160x48`: the width and height every test in this module renders
/// at, and so the exact chrome above the first data row: the title
/// band and its blank line the dashboard draws before handing off to
/// [`super::draw`], then this pane's own band, the store's terms, the
/// two gates, the roll's own status, the tab row, the heading row, the
/// hairline and the operator group's header.
pub(super) fn first_row() -> u16 {
    10
}

/// The row whose rendered text contains `needle`, or panics: every
/// fixture below names a key unique enough that only one row can match.
pub(super) fn row_of(buffer: &Buffer, needle: &str) -> u16 {
    let area = buffer.area;
    (0..area.height)
        .find(|&y| {
            let line: String = (0..area.width).map(|x| buffer[(x, y)].symbol()).collect();
            line.contains(needle)
        })
        .unwrap_or_else(|| panic!("{needle:?} is not drawn anywhere"))
}

/// The heading row `draw` places, fixed like [`first_row`] for the same
/// reason: it is the row right above the hairline, one above
/// [`first_row`]'s own chrome count.
pub(super) fn heading_row() -> u16 {
    7
}

/// The leading number off the tab row's own trailing caption, `N` in `N
/// environments in this store`.
pub(super) fn header_environment_count(buffer: &Buffer) -> usize {
    let row = row_of(buffer, "environments in this store");
    let line: String = (0..buffer.area.width)
        .map(|x| buffer[(x, row)].symbol())
        .collect();
    line.split("environments")
        .next()
        .and_then(|prefix| prefix.split_whitespace().next_back())
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("no leading number in {line:?}"))
}

/// `DB_PASSWORD`'s own `VALUE` cell, and the needle for "a data row is
/// on screen".
///
/// Not the key name: both panels head themselves with the selected
/// key, so a frame carrying the panels over an empty table contains
/// `DB_PASSWORD` twice and passes a `contains` that meant to ask about
/// the table. The block run is drawn nowhere but a `VALUE` cell.
pub(super) const FIRST_DATA_ROW: &str =
    "\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588} 9 bytes";

/// The whole rendered frame as text, for the assertions below that
/// have to say a thing is *not* drawn: [`row_of`] panics instead.
pub(super) fn frame_text(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            let line: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            format!("{}\n", line.trim_end())
        })
        .collect()
}

/// A pane on `tab`, with `count` environments named long enough that
/// the row cannot hold them all.
pub(super) fn pane_with_environments(count: usize, tab: usize) -> SecretsPane {
    SecretsPane {
        model: Box::new(SecretsModel {
            environments: (0..count).map(|n| format!("environment-{n:02}")).collect(),
            ..SecretsModel::default()
        }),
        tab,
        selected: 0,
        collapsed: HashSet::new(),
        reveal: None,
        pending_reveal: None,
        armed: None,
        typing: None,
    }
}

/// A value long enough to outgrow `VALUE`, typed through the keys an
/// operator actually presses.
pub(super) fn app_typing(buffer: &str) -> App {
    let mut app = fixtures::app_with_secrets_and_control();
    app.update(Msg::Key(KeyPress::Confirm));
    for typed in buffer.chars() {
        app.update(Msg::Key(KeyPress::TextChar(typed)));
    }
    app
}
