use super::super::super::app::{App, SecretsPane};
use super::super::super::secrets::Source;
use super::super::super::theme::Palette;
use super::super::flock::{GUTTER, gutter};
use super::super::status;
use super::column_tiers::{Column, MIN_WIDTH, columns_for};
use super::detail_panels::{PANEL_ROWS, draw_panels};
use super::pane_chrome::{
    STORE_LINE, gates_line, group_header_line, heading_line, pane_band, roll_status_line, tab_line,
};
use super::row_cells::{new_key_row_line, row_line};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

/// Rows [`draw`] spends before the first group header: this pane's own
/// band, the store's terms, the two gates, the roll's status, the tab row,
/// the heading row and the hairline. `the_chrome_is_the_rows_it_claims`
/// reads the count back off a render, so the two cannot drift.
const CHROME_ROWS: u16 = 7;

/// The shortest `area` with room for the chrome, a secret, and both
/// panels.
///
/// Two rows for the secret, not one: `draw` writes a group header before
/// the first row of each source, so a body one row shorter than this spends
/// everything it has left on the header and shows the panels over an empty
/// table. Captured at 100x16 before this counted the header.
const PANELS_MIN_ROWS: u16 = CHROME_ROWS + 2 + PANEL_ROWS;

/// `area`'s own bottom, short by [`PANEL_ROWS`] whenever there is room for
/// the two panels below it, so no data row ever draws underneath them.
///
/// Room counts the chrome, not the panels alone. Measuring the panels by
/// themselves reserved six rows at every height from `PANEL_ROWS + 1` up,
/// and `draw`'s own chrome guards reached that boundary first: the panels
/// were skipped and their rows stayed blank.
fn content_bottom(area: Rect) -> u16 {
    let bottom = area.y + area.height;
    if area.height >= PANELS_MIN_ROWS {
        bottom - PANEL_ROWS
    } else {
        bottom
    }
}

/// Draws the `+ new key` affordance at the current `y` and advances it,
/// stopping short of [`content_bottom`] the same way every row above it
/// does. `table_width` and `bottom` are `draw`'s own locals, not this
/// function's arguments, since both are cheap to recompute from `area` and
/// doing so keeps this under clippy's argument-count lint.
///
/// The one place `draw` places it: right after the last visible operator
/// row and before the first namespace group's header, closing the operator
/// group even when that group has no members of its own to close.
fn draw_new_key_row(
    pane: &SecretsPane,
    columns: &[Column],
    palette: Palette,
    area: Rect,
    buffer: &mut Buffer,
    y: &mut u16,
) {
    let bottom = content_bottom(area);
    if *y >= bottom {
        return;
    }
    let table_width = area.width.saturating_sub(GUTTER);
    let selected = pane.selected_is_new_key_row();
    let (gutter_text, gutter_style) = gutter(selected, palette);
    buffer.set_line(
        area.x,
        *y,
        &Line::from(Span::styled(gutter_text, gutter_style)),
        1,
    );
    buffer.set_line(
        area.x + GUTTER,
        *y,
        &new_key_row_line(pane, columns, table_width, palette, selected),
        table_width,
    );
    *y += 1;
}

/// What [`draw`] puts on screen below [`MIN_WIDTH`].
///
/// Two short lines, for the reason `view::draw`'s own floor refusal gives:
/// `Buffer::set_line` truncates in silence, so a refusal written as one
/// sentence could lose the number it is about. The way out is already on
/// the status bar (`esc/S close`) and is not repeated here.
fn draw_too_narrow(width: u16, palette: Palette, area: Rect, buffer: &mut Buffer) {
    buffer.set_line(
        area.x,
        area.y,
        &Line::from(Span::styled("too narrow for secrets", palette.refusal())),
        width,
    );
    if area.height < 2 {
        return;
    }
    buffer.set_line(
        area.x,
        area.y + 1,
        &Line::from(Span::styled(
            format!("need {MIN_WIDTH} columns"),
            palette.muted(),
        )),
        width,
    );
}

/// Draws the secrets pane into `area`, straight into `buffer`.
///
/// Seven rows of chrome before the first group header: this pane's own
/// band (design rule 1), the store's terms, the two gates, the roll's own
/// status, the tab row, the heading row, a hairline, then one group header
/// per source change in [`SecretsPane::model`](crate::lookout::app::SecretsPane::model)'s rows, which are already
/// contiguous by source (`SecretsModel::rows`'s own doc comment: operator
/// rows first, then each namespace's), with the `+ new key` affordance
/// closing the operator group before the first namespace header prints.
///
/// The last [`PANEL_ROWS`] rows, when `area` is tall enough to spare them,
/// belong to [`draw_panels`] instead: FOCUSED and WHO READS IT, below their
/// own hairline.
pub fn draw(app: &App, pane: &SecretsPane, area: Rect, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let palette = app.palette();
    let width = area.width;
    if width < MIN_WIDTH {
        draw_too_narrow(width, palette, area, buffer);
        return;
    }
    let table_width = width.saturating_sub(GUTTER);
    // `width`, not `table_width`: [`SECRET_TIERS`]' thresholds are the
    // design's own row width, gutter included.
    let columns = columns_for(width);
    let bottom = area.y + area.height;
    let content_bottom = content_bottom(area);
    let mut y = area.y;

    buffer.set_line(area.x, y, &pane_band(width, palette), width);
    y += 1;
    if y >= content_bottom {
        return;
    }

    buffer.set_line(
        area.x,
        y,
        &Line::from(Span::styled(STORE_LINE, palette.muted())),
        width,
    );
    y += 1;
    if y >= content_bottom {
        return;
    }

    buffer.set_line(area.x, y, &gates_line(pane, app.control(), palette), width);
    y += 1;
    if y >= content_bottom {
        return;
    }

    buffer.set_line(area.x, y, &roll_status_line(pane, palette), width);
    y += 1;
    if y >= content_bottom {
        return;
    }

    buffer.set_line(area.x, y, &tab_line(pane, palette, width), width);
    y += 1;
    if y >= content_bottom {
        return;
    }

    buffer.set_line(
        area.x + GUTTER,
        y,
        &heading_line(columns, palette),
        table_width,
    );
    y += 1;
    if y >= content_bottom {
        return;
    }

    buffer.set_line(area.x, y, &status::rule_line(palette.line(), width), width);
    y += 1;

    // `pane.new_key_anchor()` is the one place that decides where the
    // affordance sits: `SecretsPane::screen_slots` (the cursor) reads the
    // same call, so a row drawn here at the wrong spot would also move the
    // cursor there, never leave the two disagreeing about which line it is.
    let anchor = pane.new_key_anchor();
    let mut new_key_row_drawn = false;
    if anchor.is_none() {
        draw_new_key_row(pane, columns, palette, area, buffer, &mut y);
        new_key_row_drawn = true;
    }
    let mut last_source: Option<&Source> = None;
    for (index, row) in pane.model.rows.iter().enumerate() {
        if y >= content_bottom {
            break;
        }
        if last_source != Some(&row.source) {
            buffer.set_line(
                area.x + GUTTER,
                y,
                &group_header_line(pane, &row.source, palette, table_width),
                table_width,
            );
            y += 1;
            last_source = Some(&row.source);
            if y >= content_bottom {
                break;
            }
        }
        if pane.is_collapsed(&row.source) {
            continue;
        }
        let selected = index == pane.selected;
        let (gutter_text, gutter_style) = gutter(selected, palette);
        buffer.set_line(
            area.x,
            y,
            &Line::from(Span::styled(gutter_text, gutter_style)),
            1,
        );
        buffer.set_line(
            area.x + GUTTER,
            y,
            &row_line(
                pane,
                row,
                columns,
                table_width,
                palette,
                selected,
                app.now(),
            ),
            table_width,
        );
        y += 1;

        // This row is the anchor: the affordance draws right after it,
        // closing the operator group even when zero-membered, matching
        // `SecretsPane::screen_slots`'s own insertion point exactly.
        if !new_key_row_drawn && anchor == Some(index) {
            draw_new_key_row(pane, columns, palette, area, buffer, &mut y);
            new_key_row_drawn = true;
            if y >= content_bottom {
                break;
            }
        }
    }

    // The anchor row never got drawn (truncated by `content_bottom` before
    // reaching it): the affordance still belongs on screen if there is room
    // left.
    if !new_key_row_drawn {
        draw_new_key_row(pane, columns, palette, area, buffer, &mut y);
    }

    if content_bottom < bottom {
        draw_panels(pane, palette, area, buffer, content_bottom);
    }
}

/// One cell's text, read at its own column offset.
///
/// Assertions go through this rather than searching the whole row.
/// `production` is the tab label, a `SET IN` entry and an `IN FORCE`
/// value at the same time, so a row-wide `contains` would pass on any of
/// the three.
#[cfg(test)]
pub(in crate::lookout::view) fn cell(buffer: &Buffer, row: u16, column: Column) -> String {
    let columns = columns_for(buffer.area.width);
    let mut x = GUTTER;
    for candidate in columns {
        if *candidate == column {
            return (x..x + column.width())
                .map(|x| buffer[(x, row)].symbol())
                .collect();
        }
        x += candidate.width();
    }
    panic!("{column:?} is not drawn at width {}", buffer.area.width);
}

#[cfg(test)]
mod tests {
    use super::super::column_tiers::Column;

    use ratatui::layout::Rect;

    use std::time::Duration;

    use super::*;
    use crate::lookout::app::{Body, KeyPress, Msg};

    use super::super::testing::*;
    use crate::lookout::view::fixtures;

    /// [`CHROME_ROWS`] is read by [`content_bottom`], so a row added to
    /// `draw`'s own chrome without a matching bump here would go back to
    /// reserving the panels over the top of it. Counted off a render
    /// rather than off the source, band through hairline inclusive.
    #[test]
    fn the_chrome_is_the_rows_it_claims() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 48);

        let band = row_of(&buffer, "SECRETS   flock-wide");
        let hairline = row_of(&buffer, "\u{2500}\u{2500}\u{2500}");

        assert_eq!(hairline - band + 1, CHROME_ROWS, "{}", frame_text(&buffer));
    }

    /// The regression: `content_bottom` measured room by the panels alone,
    /// so every height from `PANEL_ROWS + 1` up reserved six rows that
    /// `draw` never reached, and the pane below the heading row was blank.
    #[test]
    fn a_terminal_too_short_for_both_spends_the_panel_rows_on_the_table() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 14);
        let text = frame_text(&buffer);

        assert!(
            !text.contains("FOCUSED"),
            "no room for the panels at this height: {text}"
        );
        assert!(
            text.contains(FIRST_DATA_ROW),
            "their rows go to the table instead: {text}"
        );
        assert!(
            text.contains("operator \u{d7}"),
            "the group header is drawn too: {text}"
        );
    }

    /// The boundary, both sides of it. One row short of
    /// [`PANELS_MIN_ROWS`] the panels would have nothing above them but
    /// chrome, which is the height they are not worth.
    #[test]
    fn the_panels_arrive_with_the_row_that_makes_room_for_them() {
        let app = fixtures::app_with_secrets();
        let short = usize::from(PANELS_MIN_ROWS - 1);
        let exact = usize::from(PANELS_MIN_ROWS);

        // `body_rows` spends the title band and the status bar, so a body
        // of N rows wants a terminal of N + 2.
        let below = frame_text(&fixtures::render(
            &app,
            160,
            u16::try_from(short + 2).unwrap(),
        ));
        let at = frame_text(&fixtures::render(
            &app,
            160,
            u16::try_from(exact + 2).unwrap(),
        ));

        assert!(!below.contains("FOCUSED"), "one row short: {below}");
        assert!(at.contains("FOCUSED"), "exactly enough: {at}");
        assert!(at.contains("WHO READS IT"), "both panels or neither: {at}");
        assert!(
            at.contains("operator \u{d7}"),
            "and the group header above them: {at}"
        );
        assert!(
            at.contains(FIRST_DATA_ROW),
            "and a secret under it, which is the point of the pane: {at}"
        );
    }

    /// Swept rather than sampled: the defect was invisible at every height
    /// the other tests render at, and showed only between
    /// [`PANEL_ROWS`] and [`PANELS_MIN_ROWS`].
    #[test]
    fn no_supported_height_reserves_panel_rows_it_never_draws() {
        let app = fixtures::app_with_secrets();
        for height in crate::lookout::view::flock::MIN_HEIGHT..=40 {
            let buffer = fixtures::render(&app, 160, height);
            let area = Rect::new(0, 0, 160, crate::lookout::view::body_rows(buffer.area));
            let text = frame_text(&buffer);
            if content_bottom(area) < area.y + area.height {
                assert!(
                    text.contains("FOCUSED"),
                    "{height} rows reserved the panels: {text}"
                );
                // The panels describe the selected row, so a screen
                // carrying them and no row is six rows spent saying
                // nothing. This is the half of the boundary that is not a
                // judgement call: where exactly the panels start earning
                // their rows is `PANELS_MIN_ROWS`' own doc to argue, but
                // over an empty table they never do.
                assert!(
                    text.contains(FIRST_DATA_ROW),
                    "{height} rows drew the panels over an empty table: {text}"
                );
            } else {
                assert!(
                    !text.contains("FOCUSED"),
                    "{height} rows reserved nothing for them: {text}"
                );
            }
        }
    }

    /// `fit` keeps the head, so once the buffer outgrew its column the cell
    /// showed the first 29 characters of a value and an ellipsis where the
    /// cursor had been: the operator was shown the start of what they typed
    /// while typing the end of it. The status bar's own editor carries the
    /// whole buffer, but the cell they are looking at should not disagree
    /// with it about where they are.
    #[test]
    fn a_value_outgrowing_its_column_is_shown_from_the_end_being_typed() {
        let typed = "abcdefghijklmnopqrstuvwxyz0123456789-THE-TAIL";
        let app = app_typing(typed);
        let buffer = fixtures::render(&app, 160, 48);
        let drawn = cell(&buffer, row_of(&buffer, "DB_PASSWORD"), Column::Value);

        assert!(
            drawn.trim_end().ends_with('\u{2588}'),
            "the cursor is the last thing on the cell: {drawn:?}"
        );
        assert!(
            drawn.contains("THE-TAIL"),
            "and the characters just typed are next to it: {drawn:?}"
        );
        assert!(
            drawn.starts_with('\u{2026}'),
            "with the marker where the head went: {drawn:?}"
        );
        assert!(
            !drawn.contains("abcdef"),
            "the head is what gives way, not the tail: {drawn:?}"
        );
        assert!(
            drawn.ends_with(' '),
            "one column short, so the cursor never touches IN FORCE: {drawn:?}"
        );
        assert_eq!(
            drawn.chars().count(),
            usize::from(Column::Value.width()),
            "the cell still measures its column: {drawn:?}"
        );
    }

    #[test]
    fn in_force_is_read_out_of_its_own_column_not_found_anywhere_in_the_row() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 48);

        // `production` is also the tab label and appears in SET IN. Reading
        // the cell rather than the row is what makes this assertion mean
        // anything.
        assert_eq!(
            cell(&buffer, first_row(), Column::InForce).trim(),
            "production"
        );
    }

    #[test]
    fn a_key_with_no_slot_here_says_so_rather_than_showing_a_block_run() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 48);
        let row = row_of(&buffer, "ELSEWHERE_ONLY");

        assert_eq!(cell(&buffer, row, Column::Value).trim(), "not set here");
        assert_eq!(cell(&buffer, row, Column::InForce).trim(), "-");
    }

    /// The affordance closes the operator group: it draws right after the
    /// last operator row and right before the provider group's own header,
    /// never trailing after it the way the pane once drew it.
    #[test]
    fn the_new_key_row_sits_before_the_first_namespace_group() {
        let app = fixtures::app_with_secrets_and_a_provider_row();
        let buffer = fixtures::render(&app, 160, 48);

        let operator_row = row_of(&buffer, "DB_PASSWORD");
        let new_key_row = row_of(&buffer, "+ new key");
        let provider_header = row_of(&buffer, "vercel (dog)");

        assert_eq!(
            new_key_row,
            operator_row + 1,
            "right after the last operator row"
        );
        assert_eq!(
            provider_header,
            new_key_row + 1,
            "and right before the provider group's own header"
        );
    }

    /// Interleaved sources (operator, namespace, operator): `draw`'s own
    /// scan used to close the operator group at the first row that was not
    /// the operator's own, index 1, while `SecretsPane::screen_slots` (the
    /// cursor) anchored on the highest operator index, 2. `G` would then
    /// select the affordance while it rendered two lines above where the
    /// cursor logic put it. Both now read [`SecretsPane::new_key_anchor`](crate::lookout::app::SecretsPane::new_key_anchor),
    /// so `G`'s target and the drawn affordance are the same line.
    #[test]
    fn interleaved_sources_still_agree_between_the_cursor_and_the_drawn_affordance() {
        let mut app = fixtures::app_with_interleaved_secret_sources();
        app.update(Msg::Key(KeyPress::SelectLast));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.selected_is_new_key_row(),
            "G lands on the affordance: it is the last screen slot here"
        );
        let buffer = fixtures::render(&app, 160, 48);

        let last_operator_row = row_of(&buffer, "SECOND_OPERATOR_KEY");
        let new_key_row = row_of(&buffer, "+ new key");

        assert_eq!(
            new_key_row,
            last_operator_row + 1,
            "the affordance draws right after the highest-index operator \
                 row, matching the cursor's own anchor"
        );
    }

    /// `SET IN`'s denominator has to be the tab row's own count, `all`
    /// included, or the header states a number that names an environment
    /// outside it. Checked against a key set in `all` and a key set in a
    /// named environment, since either alone would pass by half.
    #[test]
    fn the_header_denominator_matches_set_in_for_all_and_a_named_environment() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 48);
        let header_count = header_environment_count(&buffer);

        let named_row = row_of(&buffer, "DB_PASSWORD");
        let all_row = row_of(&buffer, "SET_EVERYWHERE");
        assert_eq!(
            cell(&buffer, named_row, Column::SetIn).trim(),
            format!("1 of {header_count} \u{b7} production")
        );
        assert_eq!(
            cell(&buffer, all_row, Column::SetIn).trim(),
            format!("1 of {header_count} \u{b7} all")
        );
    }

    /// The two cells a reveal changes, and the one row it changes them on.
    #[test]
    fn a_revealed_row_prints_its_value_and_its_countdown() {
        let dir = tempfile::tempdir().unwrap();
        let app = fixtures::app_revealing(dir.path());

        let buffer = fixtures::render(&app, 160, 48);

        let revealed = row_of(&buffer, "DB_PASSWORD");
        assert_eq!(
            cell(&buffer, revealed, Column::Value).trim(),
            fixtures::REVEALED_VALUE
        );
        assert_eq!(
            cell(&buffer, revealed, Column::Lands).trim(),
            "visible 10s \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}"
        );

        let untouched = row_of(&buffer, "OTHER_KEY");
        assert_eq!(
            cell(&buffer, untouched, Column::Value).trim(),
            "\u{2588}\u{2588}\u{2588} 3 bytes",
            "a reveal reaches one row, not the pane"
        );
        assert_eq!(cell(&buffer, untouched, Column::Lands).trim(), "-");
    }

    /// The words and the blocks come off one remaining duration, so a
    /// partly-spent reveal has to agree with itself: four seconds left is
    /// four of ten cells.
    #[test]
    fn the_countdown_and_its_gauge_spend_together() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_revealing(dir.path());
        let start = app.now();
        let _ = app.update(Msg::Tick {
            now: start + Duration::from_secs(6),
        });

        let buffer = fixtures::render(&app, 160, 48);

        assert_eq!(
            cell(&buffer, row_of(&buffer, "DB_PASSWORD"), Column::Lands).trim(),
            "visible 4s \u{2588}\u{2588}\u{2588}\u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}"
        );

        // Half a second later the number has not changed, so neither has
        // the gauge: rounding one and truncating the other would read `4s`
        // against three blocks here.
        let _ = app.update(Msg::Tick {
            now: start + Duration::from_millis(6_500),
        });
        let buffer = fixtures::render(&app, 160, 48);
        assert_eq!(
            cell(&buffer, row_of(&buffer, "DB_PASSWORD"), Column::Lands).trim(),
            "visible 4s \u{2588}\u{2588}\u{2588}\u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}"
        );
    }
}
