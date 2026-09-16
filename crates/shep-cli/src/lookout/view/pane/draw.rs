//! Putting the whole pane together: the chrome above the body, the panel
//! beside it where the width affords one, and the footer under a dog's.
//!
//! [`pane_lines`] is the entry every caller but the draw loop uses, and
//! [`draw_pane`] is the draw loop's. Which of the four layouts below runs
//! depends on two questions only: whether the field set has groups, and
//! whether the panel fits.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::super::super::app::App;
use super::super::super::pane::{ConfigPane, PaneTarget};
use super::super::super::theme::Palette;
use super::super::flock::fit;
use super::super::overlay;
use super::super::scroll;
use super::body::{body_from, cursor_only, grouped_body_lines};
use super::chrome::{
    column_header_line, hairline_line, has_groups, legend_line, tab_row_line, title_band_line,
    title_line,
};
use super::close::draw_close_dialog;
use super::field_row::push_wrapped_blurb;
use super::layout::{body_width, lands_fits_beside_panel, line_columns, panel_width};
use super::list::list_lines;
use super::panel::panel_lines;

/// The pane's lines when its fields carry groups (every sheep): the butter
/// title band naming the target once, the tab row, a hairline, the menu or
/// help line when one is up else the column header row, the body, a
/// hairline, and the legend.
pub(super) fn grouped_pane_lines(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
) -> Vec<Line<'static>> {
    grouped_pane_lines_with_panel(pane, palette, width, budget, None)
}

/// [`grouped_pane_lines`], with the cursor's own field's [`panel_lines`]
/// drawn beside the body only: the title band, tab row, hairline and
/// header row above it, and the trailing hairline and legend below it,
/// are chrome, not per-field, so widening them into the panel's own
/// column would only paint over where the panel sits.
///
/// Chrome is laid out at the real `width`, not clamped to the design
/// target: a terminal wider than 160 stretches the title band's own
/// reverse-video band and the hairlines the rest of the way, rather than
/// leaving blank space to the right of a pane pinned at 160. `panel_width`
/// governs how much of that width the panel itself claims; nothing here
/// grows the panel past [`super::layout::PANEL_MAX`].
///
/// Only reached when [`panel_width`] returns `Some`; a narrower terminal
/// never calls this and keeps today's single column.
pub(super) fn grouped_pane_with_panel_lines(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    panel_w: u16,
    budget: usize,
) -> Vec<Line<'static>> {
    let panel = panel_lines(pane, palette, panel_w);
    grouped_pane_lines_with_panel(pane, palette, width, budget, Some((panel, panel_w)))
}

/// The body shared by [`grouped_pane_lines`] and
/// [`grouped_pane_with_panel_lines`]: `panel` is `None` for the former,
/// which lays the body out at `width` exactly as it always has, and
/// `Some((lines, panel_width))` for the latter, which lays the body out at
/// `width - panel_width` instead and merges `lines` beside it, since that
/// is the one region tall enough and narrow enough to hold both.
pub(super) fn grouped_pane_lines_with_panel(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
    panel: Option<(Vec<Line<'static>>, u16)>,
) -> Vec<Line<'static>> {
    let has_panel = panel.is_some();
    let show_lands = lands_fits_beside_panel(width, has_panel);
    let left_width = panel
        .as_ref()
        .map_or(width, |(_, panel_width)| width.saturating_sub(*panel_width));
    let mut lines = vec![title_band_line(pane, palette, width)];
    let mut remaining = budget - 1;
    if remaining == 0 {
        return lines;
    }
    // Everything from here down is best-effort, and every push below is
    // gated on `remaining > 1` rather than `> 0`: the body's own cursor
    // outranks every one of these lines, per `cursor_only`'s own doc that
    // the selected row is drawn at every height the pane claims to
    // support, so a chrome line is only added when doing so still leaves
    // at least one line for the body once the budget reaches it.
    if remaining > 1 {
        lines.push(tab_row_line(pane, palette, width));
        remaining -= 1;
    }
    if remaining > 1 {
        lines.push(hairline_line(palette, width));
        remaining -= 1;
    }
    if remaining > 1 {
        lines.push(column_header_line(palette, left_width, show_lands));
        remaining -= 1;
    }
    push_wrapped_blurb(&mut lines, &mut remaining, pane, palette, width);
    // Unreachable given `push_wrapped_blurb`'s own floor of `<= 1`. Kept as
    // the explicit statement of that invariant;
    // `the_blurb_never_spends_the_row_the_cursor_needs` is what actually
    // fails if the floor is ever loosened.
    if remaining == 0 {
        return lines;
    }
    // The trailing hairline and legend are reserved ahead of the body,
    // same rule every other footer in this module follows (see
    // `body_from`'s own doc on markers): a line nothing counted is a line
    // that can overrun. Never the body's last line, though, for the same
    // reason as above.
    let footer_lines = if remaining > 1 {
        2.min(remaining - 1)
    } else {
        0
    };
    let body_budget = remaining - footer_lines;
    if body_budget > 0 {
        let body = grouped_body_lines(pane, palette, left_width, body_budget, show_lands);
        lines.extend(match panel {
            Some((panel, _)) => merge_beside_panel(body, panel, left_width, body_budget),
            None => body,
        });
    }
    if footer_lines >= 1 {
        lines.push(hairline_line(palette, width));
    }
    if footer_lines >= 2 {
        lines.push(legend_line(palette, width));
    }
    lines
}

/// `left`'s lines and `panel`'s lines, side by side: `left` padded out to
/// exactly `left_width` columns with a blank span (never truncated, since
/// every left line is already laid out to fit inside its own width), then
/// whichever `panel` row shares that index appended after it.
///
/// `left` and `panel` are rarely the same length: a short field list (a
/// dog's group-free body, or a group with few fields) can run out before
/// `NEIGHBOURS` does, and a field with nothing to say in three of its six
/// regions can leave `panel` shorter than the field list above it. Either
/// side short of the other draws blank rather than dropping the longer
/// side's own rows, up to `budget`, the same vertical ceiling
/// [`grouped_body_lines`] was already laid out against: this only ever
/// lengthens `left`'s own count, never grows past what the caller already
/// reserved room for.
pub(super) fn merge_beside_panel(
    left: Vec<Line<'static>>,
    panel: Vec<Line<'static>>,
    left_width: u16,
    budget: usize,
) -> Vec<Line<'static>> {
    let left_width = usize::from(left_width);
    // `left` never exceeds `budget` on its own, since `grouped_body_lines`
    // already laid it out against the same ceiling; the `min` only ever
    // trims `panel`'s own overrun.
    let rows = left.len().max(panel.len()).min(budget);
    let mut left = left.into_iter();
    let mut panel = panel.into_iter();
    (0..rows)
        .map(|_| {
            let line = left.next().unwrap_or_default();
            let used = line_columns(&line);
            let mut spans = line.spans;
            if used < left_width {
                spans.push(Span::raw(" ".repeat(left_width - used)));
            }
            if let Some(panel_line) = panel.next() {
                spans.extend(panel_line.spans);
            }
            Line::from(spans)
        })
        .collect()
}

/// The trailing "shep publishes..." line a dog-target render reserves out
/// of its own budget before the body claims what is left: `None` for a
/// sheep, which owns its own reload rather than handing that decision to a
/// dog's own binary, and for a dog with no budget left to spend on it.
///
/// Text only; the caller decides whether reserving it costs one line of
/// `body_budget`, since [`pane_lines`]'s plain branch and
/// `ungrouped_pane_lines_with_panel` both need that decision made before
/// this call, not after.
pub(super) fn dog_footer_text(pane: &ConfigPane, body_budget: usize) -> Option<String> {
    let PaneTarget::Dog { name, .. } = pane.target() else {
        return None;
    };
    (body_budget > 0).then(|| format!("shep publishes the change; {name} decides what to reload"))
}

/// Pushes `footer`'s line onto `lines`, muted and fit to `width`: the tail
/// half of [`dog_footer_text`], shared the same way the reservation half is.
pub(super) fn push_footer_line(
    lines: &mut Vec<Line<'static>>,
    footer: Option<String>,
    width: u16,
    palette: Palette,
) {
    if let Some(text) = footer {
        lines.push(Line::from(Span::styled(
            format!("  {}", fit(&text, body_width(width))),
            palette.muted(),
        )));
    }
}

/// Every line of the pane, top to bottom, laid out for a terminal `height`
/// rows tall.
///
/// `height` counts lines, and no more than that ever come back. Zero means
/// unlimited, which is what a test with no terminal behind it gets. See
/// [`super::super::scroll`] for why the viewport's offset is a starting point
/// here, not an answer.
///
/// The close dialog is not drawn here: it overlays the whole field list
/// (`draw_pane`), rather than taking the one line under the title the
/// apply menu this pane replaced used to.
#[must_use]
pub fn pane_lines(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    height: u16,
) -> Vec<Line<'static>> {
    let budget = if height == 0 {
        usize::MAX
    } else {
        usize::from(height)
    };
    if budget == 0 {
        return Vec::new();
    }
    if let Some(list) = pane.list() {
        return list_lines(pane, list, palette, width, budget);
    }
    if has_groups(pane) {
        if let Some(panel_w) = panel_width(width) {
            return grouped_pane_with_panel_lines(pane, palette, width, panel_w, budget);
        }
        return grouped_pane_lines(pane, palette, width, budget);
    }
    let panel = panel_width(width).map(|panel_w| (panel_lines(pane, palette, panel_w), panel_w));
    ungrouped_pane_lines_with_panel(pane, palette, width, budget, panel)
}

/// The body [`pane_lines`] draws for a pane with no groups, which is a
/// dog's own shape: `panel` is [`None`] below [`panel_width`]'s floor, which
/// lays the body out at `width` in one column, and `Some((lines,
/// panel_width))` above it, which lays the body out at `width -
/// panel_width` and merges `lines` beside it.
///
/// One parameter rather than two near-identical functions, the same
/// treatment [`grouped_pane_lines_with_panel`] gives the grouped pair.
///
/// No tab row and no legend: a dog's schema carries no group to name, and
/// adding the panel is not a reason to invent one. The title is laid out at
/// the real `width`, and the trailing "shep publishes..." footer is chrome,
/// reserved out of the budget before the body claims what is left.
pub(super) fn ungrouped_pane_lines_with_panel(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
    panel: Option<(Vec<Line<'static>>, u16)>,
) -> Vec<Line<'static>> {
    let show_lands = lands_fits_beside_panel(width, panel.is_some());
    let left_width = panel
        .as_ref()
        .map_or(width, |(_, panel_width)| width.saturating_sub(*panel_width));
    let mut lines = vec![title_line(pane, palette, width)];
    // The title is unconditional, so the body is laid out against what is
    // left after it. An empty form (unreachable for a sheep, whose schema
    // is a committed file with 40 properties, but a dog answers `--schema`
    // for itself) leaves the title as the whole pane.
    let mut body_budget = budget - 1;
    // The one line a dog pane has that a sheep pane does not: shep does not
    // know what a dog's field costs, so every row's COST cell is empty.
    // Reserved out of the budget before rows are laid out, for the same
    // reason the top line is: a footer appended afterwards is a line
    // nothing counted.
    //
    // Reserved before the BLURB too, not between the blurb and the rows:
    // the blurb's floor below keeps one line back for the cursor's own
    // row, and a footer reserved afterward would take exactly that line.
    // The blurb is the line to lose, since it describes the row rather
    // than being it.
    let footer = dog_footer_text(pane, body_budget);
    if footer.is_some() {
        body_budget -= 1;
    }
    // The selected field's own help text, on the lines under the title.
    // Subtracted from the budget rather than appended, per `body_from`'s
    // own doc on markers. See `top_lines`.
    //
    // `push_wrapped_blurb`'s own floor of `<= 1`, not `== 0`: a long
    // wrapped help must not spend the last line reserved for the cursor's
    // own row.
    push_wrapped_blurb(&mut lines, &mut body_budget, pane, palette, width);
    if !pane.fields().is_empty() && body_budget > 0 {
        let total = pane.rows().len();
        let cursor_row = pane.view().cursor().min(total - 1);
        let body = scroll::to_cursor(
            cursor_row,
            pane.view().offset(),
            |offset| {
                body_from(
                    pane,
                    palette,
                    left_width,
                    body_budget,
                    offset,
                    true,
                    show_lands,
                )
            },
            || {
                cursor_only(
                    pane,
                    palette,
                    left_width,
                    body_budget,
                    cursor_row,
                    show_lands,
                )
            },
        );
        lines.extend(match panel {
            Some((panel, _)) => merge_beside_panel(body, panel, left_width, body_budget),
            None => body,
        });
    }
    push_footer_line(&mut lines, footer, width, palette);
    lines
}

/// Draws the pane into `area`, straight into `buffer`.
pub fn draw_pane(app: &App, pane: &ConfigPane, area: Rect, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let lines = pane_lines(pane, app.palette(), area.width, area.height);
    for (offset, line) in lines.iter().enumerate().take(usize::from(area.height)) {
        let offset = u16::try_from(offset).unwrap_or(0);
        buffer.set_line(area.x, area.y + offset, line, area.width);
    }
    if let Some(dialog) = app.close_dialog() {
        // The pane draws first and is then muted whole, so 1e's own render
        // is untouched and its four pinned snapshots do not move.
        overlay::mute(buffer, area, app.palette());
        draw_close_dialog(dialog, app.palette(), app.now(), area, buffer);
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{
        bark_pane, config_pane_lines_for_tests, pane_to, text_of, web_pane,
    };
    use super::super::layout::{FULL_WIDTH, LANDS_WITH_PANEL_MIN};
    use super::*;
    use crate::lookout::app::{Effect, KeyPress, Msg};
    use crate::lookout::view::MIN_TERM_WIDTH;
    use crate::lookout::view::fixtures;
    use crate::output::width::visible_width;

    /// The whole pane at a comfortable width, unbounded. The snapshot is the
    /// assertion: it pins the title, the four section headers in order, all
    /// 40 rows, the two flags and the cost cell beside each one.
    #[test]
    fn a_sheep_pane_at_a_comfortable_width() {
        let lines = pane_lines(&web_pane(), fixtures::plain(), 120, 0);
        insta::assert_snapshot!("sheep_pane_wide", text_of(&lines).join("\n"));
    }

    /// `Buffer::set_line` clips in silence, so an overrun renders as a
    /// truncated cost cell with nothing saying it was cut.
    #[test]
    fn every_pane_line_fits_the_width_it_was_drawn_for() {
        let pane = web_pane();
        for width in MIN_TERM_WIDTH..=200 {
            for line in text_of(&pane_lines(&pane, fixtures::plain(), width, 0)) {
                assert!(
                    visible_width(&line) <= usize::from(width),
                    "width {width} drew {}: {line:?}",
                    visible_width(&line)
                );
            }
        }
    }

    /// Both directions: `watch` is `Live`, draws `now`, and can still
    /// park. `autostart` is `NextSpawn`, draws `next start`, and takes
    /// effect at muster, not at the next spawn. The column is never
    /// corrected after a reply, since a reply covers one row of
    /// forty; it stays a prediction everywhere, the bar reports the
    /// outcome, and the row's `!` flag carries it afterwards.
    #[test]
    fn the_cost_column_predicts_and_the_status_bar_reports() {
        for (key, column, pending, sentence) in [
            ("watch", "now", true, "waits for `shep reload web`"),
            ("autostart", "next start", false, "set to false"),
        ] {
            let mut app = fixtures::app_in_sheep_pane_with_control();
            pane_to(&mut app, key);

            // Below the panel's own floor: the panel suppresses the row's own
            // COST cell, which is exactly what this test reads.
            let text = text_of(&pane_lines(
                app.config_pane().unwrap(),
                fixtures::plain(),
                89,
                0,
            ));
            // A group header line is bare, just its name; a field row always
            // carries a value and a cost cell after the key, so the header
            // never matches once a second token is required. `key` can equal
            // its own group's name (the `watch` field, the `watch` group).
            let row = text
                .iter()
                .find(|line| {
                    let mut words = line.split_whitespace();
                    let mut rest = line.get(3..).map(str::split_whitespace);
                    (words.next() == Some(key) && words.next().is_some())
                        || rest
                            .as_mut()
                            .is_some_and(|w| w.next() == Some(key) && w.next().is_some())
                })
                .unwrap_or_else(|| panic!("{key} is drawn at 89 columns: {text:?}"));
            assert!(row.contains(column), "{key}: {row:?}");

            app.update(Msg::Key(KeyPress::Cycle));
            // `app_in_sheep_pane_with_control` parks `kill_signal`
            // unconditionally, so `esc` only asks; `c` is what actually
            // gets the write onto the wire.
            let _ = app.update(Msg::Key(KeyPress::Escape));
            let effect = if app.close_dialog().is_some() {
                app.update(Msg::Key(KeyPress::Continue))
            } else {
                Effect::None
            };
            let Effect::SendAll(mut batch) = effect else {
                panic!("{key}: closing the pane sends");
            };
            app.update(Msg::Replied {
                sent: batch.remove(0),
                result: Ok(shep_core::protocol::Response::SheepFieldSet {
                    name: "web".to_string(),
                    key: key.to_string(),
                    pending,
                    warning: None,
                }),
            });
            let bar = crate::lookout::view::status::status_line(&app, 200).to_string();
            assert!(bar.contains(sentence), "{key}: {bar:?}");
        }
    }

    /// The whole dog pane at a comfortable width. The snapshot is the
    /// assertion: the title says `dog config`, the rows are flat with no
    /// section headers, every COST cell is empty, and the foot says once
    /// that the dog decides.
    ///
    /// The three assertions ahead of it fail loudly rather than as a
    /// snapshot diff: a webhook URL on screen is the leak the whole secret
    /// contract exists to prevent.
    #[test]
    fn a_dog_pane_at_a_comfortable_width() {
        let text = text_of(&pane_lines(&bark_pane(), fixtures::plain(), 120, 0));
        assert!(
            !text.iter().any(|line| line.contains("hooks.example")),
            "a secret is never rendered: {text:?}"
        );
        assert!(text.iter().any(|line| line.contains("<set>")), "{text:?}");
        assert!(
            text.iter()
                .any(|line| line.contains("decides what to reload")),
            "{text:?}"
        );
        insta::assert_snapshot!("dog_pane_wide", text.join("\n"));
    }

    #[test]
    fn a_dog_panes_footer_is_paid_for_out_of_the_height_it_was_given() {
        let mut pane = bark_pane();
        for height in 1..=20u16 {
            pane.set_rows(usize::from(height.saturating_sub(1)));
            for cursor in [0usize, 2, 4] {
                pane.move_to_first();
                pane.move_by(isize::try_from(cursor).unwrap());
                let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, height));
                assert!(
                    text.len() <= usize::from(height),
                    "height {height}, cursor {cursor}: {text:?}"
                );
            }
        }
    }

    /// The footer is the newest line and the longest, and
    /// `Buffer::set_line` clips in silence.
    #[test]
    fn every_dog_pane_line_fits_the_width_it_was_drawn_for() {
        let pane = bark_pane();
        for width in MIN_TERM_WIDTH..=200 {
            for line in text_of(&pane_lines(&pane, fixtures::plain(), width, 0)) {
                assert!(
                    visible_width(&line) <= usize::from(width),
                    "width {width} drew {}: {line:?}",
                    visible_width(&line)
                );
            }
        }
    }

    /// No row may cross the panel boundary or run past the terminal it was
    /// drawn for, at the design target and at a terminal wider than it: the
    /// chrome now stretches to fill a wide terminal instead of capping at
    /// 160 with blank space to the right, so the ceiling this checks
    /// against is `width` itself, not a fixed 160.
    #[test]
    fn no_row_spills_across_the_panel_boundary_or_past_its_own_width() {
        let app = fixtures::app_in_sheep_pane();
        let pane = app.config_pane().expect("the pane is open");
        for width in [160, 200] {
            for line in pane_lines(pane, fixtures::plain(), width, 48) {
                let cols = line_columns(&line);
                assert!(
                    cols <= usize::from(width),
                    "a row is {cols} columns wide at terminal width {width}: {line:?}"
                );
            }
        }
    }

    /// The drop order the whole ladder rests on: where both the panel and
    /// `LANDS` fit, both draw; where they cannot, `LANDS` gives way first.
    /// Corrected 2026-09-11, reversing this test's own original name and
    /// claim, which asserted the two were never both present. Asserted at
    /// every width rather than at three of them, through the real render
    /// path ([`pane_lines`]/[`config_pane_lines_for_tests`]) rather than
    /// [`widths`] called directly: `widths`'s own `show_lands` parameter is
    /// wired from the caller's own [`lands_fits_beside_panel`] state, so
    /// calling it directly with a fixed `show_lands` cannot see whether
    /// that wiring is actually in place.
    #[test]
    fn lands_gives_way_to_the_panel_below_the_design_target_and_joins_it_above() {
        let app = fixtures::app_in_sheep_pane();
        for width in MIN_TERM_WIDTH..=200 {
            let rows = fixtures::render_all(&config_pane_lines_for_tests(&app, width, 48));
            let panel = rows.contains("FOCUSED");
            let lands = rows.contains("LANDS");
            let has_panel = panel_width(width).is_some();
            match (has_panel, width >= LANDS_WITH_PANEL_MIN) {
                // 160 and up: the panel and the column share the row.
                (true, true) => assert!(
                    panel && lands,
                    "the panel and LANDS must both draw at {width} columns"
                ),
                // 90 to 159: the panel draws, LANDS gives way to it.
                (true, false) => assert!(
                    panel && !lands,
                    "LANDS must give way to the panel at {width} columns"
                ),
                // below 90: no panel, so LANDS carries cost alone, subject
                // to `widths`'s own pre-existing narrow-terminal cascade,
                // which this fix leaves untouched.
                (false, _) => {
                    assert!(!panel, "the panel must not draw at {width} columns");
                    let expected_lands = body_width(width) >= FULL_WIDTH;
                    assert_eq!(
                        lands, expected_lands,
                        "LANDS mismatch at {width} columns, no panel"
                    );
                }
            }
        }
    }

    /// Every width the pane can be drawn at draws inside itself. This is
    /// the same sweep the existing pane tests run and it stays.
    #[test]
    fn every_row_fits_the_width_it_was_drawn_for() {
        for width in MIN_TERM_WIDTH..=200 {
            let app = fixtures::app_in_sheep_pane();
            for row in config_pane_lines_for_tests(&app, width, 48) {
                assert!(
                    fixtures::render_all(std::slice::from_ref(&row))
                        .chars()
                        .count()
                        <= usize::from(width),
                    "a row overflows at {width}"
                );
            }
        }
    }
}
