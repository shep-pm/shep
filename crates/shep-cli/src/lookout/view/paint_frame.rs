use super::super::app::{App, Body, Grouping, Link, RowKey};
use super::super::theme::Palette;
use super::MIN_TERM_WIDTH;
use super::flock::MIN_HEIGHT;
use super::layout_budget::{
    DETAIL_ROWS, FEED_ROWS, HOST_ROWS, ROOMY_HEIGHT, body_rows, panes_for, title_gap_rows,
};
use crate::vocabulary::Role;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// The refusal a terminal under [`MIN_TERM_WIDTH`] or [`MIN_HEIGHT`] gets
/// instead of a body.
///
/// Two short lines, not one long sentence: `Buffer::set_line` truncates at
/// `max_width` in silence, and this exists for terminals narrower than
/// `MIN_TERM_WIDTH`.
pub(super) fn draw_too_small(frame: &mut Frame<'_>, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let first = Line::from(Span::raw("too small"));
    frame
        .buffer_mut()
        .set_line(area.x, area.y, &first, area.width);
    if area.height >= 2 {
        let second = Line::from(Span::raw(format!("need {MIN_TERM_WIDTH}x{MIN_HEIGHT}")));
        frame
            .buffer_mut()
            .set_line(area.x, area.y + 1, &second, area.width);
    }
}

/// Real caller: `super::super::mod`'s `run_ui`, once per frame.
///
/// Three steps, so the overlay's own call is structural: a new `Body`
/// variant cannot skip it, since the call sits outside the match a new
/// arm would join. One call after whichever body drew, not a convention
/// repeated at each exit.
///
/// The refusal is the one path that gets no overlay: a terminal too
/// narrow for a body is too narrow for a box over it.
pub fn draw(app: &App, frame: &mut Frame<'_>) {
    let area = frame.area();
    let (width, height) = (area.width, area.height);

    if width < MIN_TERM_WIDTH || height < MIN_HEIGHT {
        draw_too_small(frame, area);
        return;
    }

    draw_body(app, frame);
    draw_keymap_overlay(app, area, frame.buffer_mut(), app.palette());
}

/// Whichever of the six [`Body`] variants is up, plus the title band and
/// the status bar around it. Never the keymap overlay: [`draw`] draws that
/// once, over whatever this left on the screen.
pub(super) fn draw_body(app: &App, frame: &mut Frame<'_>) {
    let area = frame.area();
    let (width, height) = (area.width, area.height);
    let palette = app.palette();

    let panes = panes_for(height);
    let mut y = area.y;
    // The status bar's own row. Held once, up front: the bottom stack below
    // is laid out UPWARD from it, so nothing has to know the flock table's
    // length before deciding where the table ends.
    let bottom = area.y + height - 1;
    let buffer = frame.buffer_mut();

    buffer.set_line(area.x, y, &title_band(app, width), width);
    // The sheep pane owns the whole body between the title and the status
    // bar too, the same as the four below, but row 1 is its own identity
    // band rather than blank chrome, so it is checked here, ahead of the
    // roomy blank row the others are paid in: a tall terminal must not
    // push the band down to row 2 the way it pushes their body down. Its
    // own row, `y + 1`, rather than `title_gap_rows`, which is exactly the
    // gap it is skipping.
    if let Body::Sheep(pane) = app.body() {
        let top = y + 1;
        let body = Rect {
            x: area.x,
            y: top,
            width,
            height: bottom.saturating_sub(top),
        };
        super::sheep::draw(app, pane, body, buffer);
        buffer.set_line(
            area.x,
            bottom,
            &super::status::status_line(app, width),
            width,
        );
        return;
    }

    // `title_gap_rows` also covers the blank row under the title on a
    // roomy terminal; the rule further down spends a second one of its
    // own, both from the design's own row allocation. One function, read
    // by `body_rows` too, so the two cannot drift.
    y += title_gap_rows(height);
    let roomy = height >= ROOMY_HEIGHT;
    // Read four times below: the column header's own wording, and the three
    // panes the bottom stack chooses between.
    let frozen = matches!(app.link(), Link::Lost { .. });

    // The settings screen and the config pane each own the whole body
    // between the title and the status bar: a swap, not an overlay, so
    // nothing below draws while one is up. One `match` on `App::body`
    // rather than two sequential `if let`s, since the two can never both
    // be open at once. Still true of these four bodies; two overlays draw
    // over a body rather than swapping for it. The config pane's own close
    // dialog came first, inside `super::pane::draw_pane` rather than here, over
    // whichever body that pane already is. The keymap overlay came second
    // and is general: `draw` draws it after this function returns, over
    // whichever of the six bodies drew, so nothing here has to remember.
    match app.body() {
        Body::ConfigPane(pane) => {
            let body = Rect {
                x: area.x,
                y,
                width,
                height: body_rows(area),
            };
            super::pane::draw_pane(app, pane, body, buffer);
            buffer.set_line(
                area.x,
                bottom,
                &super::status::status_line(app, width),
                width,
            );
            return;
        }
        Body::Settings(settings) => {
            let body = Rect {
                x: area.x,
                y,
                width,
                height: body_rows(area),
            };
            super::settings::draw_settings(app, settings, body, buffer);
            buffer.set_line(
                area.x,
                bottom,
                &super::status::status_line(app, width),
                width,
            );
            return;
        }
        Body::Bleats(pane) => {
            let body = Rect {
                x: area.x,
                y,
                width,
                height: body_rows(area),
            };
            super::bleats_full::draw(app, pane, body, buffer);
            buffer.set_line(
                area.x,
                bottom,
                &super::status::status_line(app, width),
                width,
            );
            return;
        }
        Body::Secrets(pane) => {
            let body = Rect {
                x: area.x,
                y,
                width,
                height: body_rows(area),
            };
            super::secrets::draw(app, pane, body, buffer);
            buffer.set_line(
                area.x,
                bottom,
                &super::status::status_line(app, width),
                width,
            );
            return;
        }
        Body::Sheep(_) => unreachable!("handled above, ahead of the roomy blank row"),
        Body::FlockTable => {}
    }

    if let Some(banner) = super::status::banner_line(app, width) {
        buffer.set_line(area.x, y, &banner, width);
        y += 1;
    }

    if panes.host {
        buffer.set_line(area.x, y, &super::host::strip_line(app, width), width);
        y += HOST_ROWS;
    }

    // width >= MIN_TERM_WIDTH, checked above, so this never underflows.
    let table_width = width - super::flock::GUTTER;
    // Two column sets, one per `Grouping`: the flat table's own and the fold
    // view's own, never mixed. Whichever one supplies the header row below
    // must be the same one the rows drawn under it read, or a header lines
    // up with cells it did not describe.
    let flat_columns = super::flock::columns_for(table_width);
    let fold_columns = super::flock::fold_columns_for(table_width);
    // The rule sits between the host strip and the table, not between the
    // headers and their own rows: it separates two regions, and a rule
    // directly under the headers reads as underlining them instead.
    //
    // Full width, unlike the headers, which start after the gutter: it is
    // chrome, and a rule that stopped two columns short of the left edge
    // would look like a rendering bug.
    buffer.set_line(
        area.x,
        y,
        &super::status::rule_line(palette.line(), width),
        width,
    );
    y += 1;
    if roomy {
        y += 1;
    }
    let header = match app.grouping() {
        Grouping::Flat => {
            super::flock::header_line(flat_columns, table_width, palette.muted(), frozen)
        }
        Grouping::ByFold => {
            super::flock::fold_columns_header_line(fold_columns, table_width, palette.muted())
        }
    };
    buffer.set_line(area.x + super::flock::GUTTER, y, &header, table_width);
    y += 1;

    // The bottom stack, laid out upward from the status bar: whichever of
    // the detail pane and the feed are up claim their rows off `bottom`
    // first, and the table gets whatever is left between `y` and `floor`.
    let mut floor = bottom;
    // One pane or two, never both shapes. A frozen dashboard's detail band
    // and feed are two more frozen readings, and neither answers the
    // question their operator is holding; the link panel is the only thing
    // on the screen that can. It claims the rows at the tier the feed
    // appears at, since that is the shorter of the two the pair need.
    let link_at = (frozen && panes.feed).then(|| {
        floor -= super::link_panel::LINK_ROWS;
        floor
    });
    let feed_at = (!frozen && panes.feed).then(|| {
        floor -= FEED_ROWS;
        floor
    });
    let detail_at = (!frozen && panes.detail).then(|| {
        floor -= DETAIL_ROWS;
        floor
    });

    // Everything from `y` up to `floor`: the viewport stops at `floor`
    // rather than at the status bar.
    let viewport = usize::from(floor - y);
    let keys = app.visible_rows();
    if keys.is_empty() {
        // Two sentences, because there are two reasons and an operator cannot
        // tell them apart from a blank table. `the flock is empty` stays for
        // the case it describes and no other.
        let text = if app.flock_len() == 0 {
            "the flock is empty".to_string()
        } else {
            format!("no sheep's name contains \"{}\"", app.filter())
        };
        let line = Line::from(Span::styled(text, palette.muted()));
        buffer.set_line(area.x, y, &line, width);
    } else {
        let offset =
            super::flock::scroll_offset(app.selected_index().unwrap_or(0), viewport, keys.len());
        let selected = app.selected();
        for (slot, key) in keys.iter().skip(offset).take(viewport).enumerate() {
            let slot = u16::try_from(slot).unwrap_or(0);
            let is_selected = selected.as_ref() == Some(key);
            let (gutter_text, gutter_style) = super::flock::gutter(is_selected, palette);
            buffer.set_line(
                area.x,
                y + slot,
                &Line::from(Span::styled(gutter_text, gutter_style)),
                1,
            );
            let line = if let RowKey::Section(label) = key {
                // The `Flock`/`Dogs` header becomes a band, drawn here
                // rather than through `super::flock::key_line`'s own
                // `RowKey::Section` arm: that arm's `section_line` stays,
                // muted rather than a band, but no current caller reaches
                // it, since this task's file list does not extend to
                // `flock.rs`.
                //
                // Meadow for the flock band, sky for the dogs band
                // (docs/lookout/design-files/README.md:149). `"Dogs"` and
                // `"no fold"` are the labels `RowKey::Section` carries, so
                // anything else stays meadow.
                let role = if *label == "Dogs" {
                    Role::Sky
                } else {
                    Role::Meadow
                };
                // The data palette, not the chrome one: this band sits
                // inside the table, and a meadow row in a dead-grey table
                // reads as the one thing on it that is still alive. Frozen,
                // meadow and sky both resolve to the muted ink and the two
                // bands are told apart by their own words, which is what
                // `NO_COLOR` already asks of them.
                section_band(
                    &label.to_ascii_uppercase(),
                    role,
                    &app.data_palette(),
                    table_width,
                )
            } else {
                match app.grouping() {
                    Grouping::Flat => {
                        super::flock::key_line(app, key, flat_columns, table_width, is_selected)
                    }
                    Grouping::ByFold => super::flock::fold_key_line(
                        app,
                        key,
                        fold_columns,
                        table_width,
                        is_selected,
                    ),
                }
            };
            buffer.set_line(area.x + super::flock::GUTTER, y + slot, &line, table_width);
        }
    }

    if let Some(top) = detail_at {
        buffer.set_line(
            area.x,
            top,
            &super::status::rule_line(palette.line(), width),
            width,
        );
        for (offset, line) in super::detail::detail_lines(app, width).iter().enumerate() {
            let offset = u16::try_from(offset).unwrap_or(0);
            buffer.set_line(area.x, top + 1 + offset, line, width);
        }
    }
    if let Some(top) = feed_at {
        buffer.set_line(
            area.x,
            top,
            &super::status::rule_line(palette.line(), width),
            width,
        );
        let rows = usize::from(FEED_ROWS - 1);
        for (offset, line) in super::bleats::feed_lines(app, width, rows)
            .iter()
            .enumerate()
        {
            let offset = u16::try_from(offset).unwrap_or(0);
            buffer.set_line(area.x, top + 1 + offset, line, width);
        }
    }
    if let Some(top) = link_at {
        buffer.set_line(
            area.x,
            top,
            &super::status::rule_line(palette.line(), width),
            width,
        );
        for (offset, line) in super::link_panel::panel_lines(app, width)
            .iter()
            .enumerate()
        {
            let offset = u16::try_from(offset).unwrap_or(0);
            buffer.set_line(area.x, top + 1 + offset, line, width);
        }
    }

    buffer.set_line(
        area.x,
        bottom,
        &super::status::status_line(app, width),
        width,
    );
}

/// Last, over everything: the overlay covers whatever body is showing,
/// unlike 1g's dialog, which only ever covers the config pane and so draws
/// from inside `view::pane::draw_pane`.
///
/// [`App::keymap_open`]'s own doc says it is reached from every body's own
/// `Help` arm; [`draw`] calls this once, after [`draw_body`] returns from
/// whichever of the six it took.
pub(super) fn draw_keymap_overlay(app: &App, area: Rect, buffer: &mut Buffer, palette: Palette) {
    if app.keymap_open() {
        super::overlay::mute(buffer, area, palette);
        super::keymap::draw(app, area, buffer);
    }
}

/// The title row: a full-width reverse-video band naming the mode.
///
/// Meadow while [`App::link`] is live, bark once it is [`Link::Lost`] — the
/// only two arms this pane needs; the editing and secrets bands belong to
/// panes this plan does not build. What this is, where it points, and how
/// big the flock is, padded to `width` before styling ([`band_line`]):
/// ratatui paints a span's background, and applies `Modifier::REVERSED`,
/// only under the cells its text occupies, so a band that stopped where its
/// text stopped would leave the rest of the row unpainted.
///
/// The bark arm says something else entirely. A dead shepherd's row spends
/// its whole width on what happened and when, since every other number on
/// the screen is now history and the flock count is one of them;
/// `super::status::banner_line` picks up the `$SHEP_HOME` this loses.
pub(super) fn title_band(app: &App, width: u16) -> Line<'static> {
    let (left, right, role) = match app.link() {
        Link::Lost { at_local, .. } => (
            format!(" THE SHEPHERD HAS DIED  \u{2596}  these values are frozen as of {at_local}"),
            " nothing here is live  \u{2596}  q to quit ".to_string(),
            Role::Bark,
        ),
        Link::Live | Link::Retrying { .. } => {
            let visible = app.rows().len();
            let total = app.flock_len();
            let right = if app.filter().is_empty() {
                format!(" {total} in the flock")
            } else {
                format!(" {visible} of {total} in the flock")
            };
            (
                format!("shep lookout   {}", app.home()),
                right,
                Role::Meadow,
            )
        }
    };
    let budget = width.saturating_sub(u16::try_from(right.chars().count()).unwrap_or(0));
    let text = format!("{}{right}", super::flock::fit(&left, budget));
    band_line(text, width, app.palette().band(role))
}

/// A section header band: [`super::cell::band`]'s two-block marker and `label`,
/// reverse video in `role`.
///
/// `super::cell::band` already pads its result to `width`, so unlike [`title_band`]
/// this needs no separate padding step.
pub(super) fn section_band(
    label: &str,
    role: Role,
    palette: &Palette,
    width: u16,
) -> Line<'static> {
    Line::from(Span::styled(
        super::cell::band(label, usize::from(width)),
        palette.band(role),
    ))
}

/// Pads `text` to `width` columns before wrapping it in one styled span.
///
/// Shared by callers that build their own text rather than going through
/// [`super::cell::band`], so a band's `REVERSED` modifier paints every cell of the
/// row rather than stopping where the text does.
pub(super) fn band_line(text: String, width: u16, style: Style) -> Line<'static> {
    let drawn = text
        .chars()
        .map(crate::output::width::char_columns)
        .sum::<usize>();
    let mut padded = text;
    if drawn < usize::from(width) {
        padded.extend(std::iter::repeat_n(' ', usize::from(width) - drawn));
    }
    Line::from(Span::styled(padded, style))
}

#[cfg(test)]
mod tests {

    use super::super::super::app::App;
    use super::super::super::theme::Palette;

    use super::super::testing::*;
    use super::*;
    use crate::lookout::app::{Control, KeyPress, Msg};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::{Color, Modifier};
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;
    use std::time::Instant;

    #[test]
    fn the_title_band_is_reverse_video_across_the_whole_width() {
        let app = super::super::fixtures::app_with(Vec::new(), super::super::fixtures::coloured());
        let line = title_band(&app, 80);
        assert_eq!(
            line.spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum::<usize>(),
            80,
            "a band that stops where its text stops leaves unpainted cells"
        );
        assert!(
            line.spans[0]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn a_frozen_link_turns_the_title_band_bark() {
        let mut app =
            super::super::fixtures::app_with(Vec::new(), super::super::fixtures::coloured());
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: super::super::fixtures::FROZEN_WHY.to_string(),
        });
        let line = title_band(&app, 80);
        assert_eq!(line.spans[0].style.fg, Some(Color::Indexed(166)));
    }

    #[test]
    fn the_title_band_counts_both_numbers_while_a_filter_is_on() {
        let app = super::super::fixtures::filtered_app("web");
        let title = super::super::fixtures::rendered(&title_band(&app, 120));
        assert!(title.contains("2 of 4 in the flock"), "got {title:?}");
    }

    #[test]
    fn the_unfiltered_title_band_is_unchanged() {
        let app = super::super::fixtures::filtered_app("");
        let title = super::super::fixtures::rendered(&title_band(&app, 120));
        assert!(title.contains("4 in the flock"), "got {title:?}");
        assert!(
            !title.contains(" of "),
            "no second number when nothing is hidden"
        );
    }

    #[test]
    fn the_flock_and_dogs_bands_carry_different_roles() {
        // Meadow for the flock band, sky for the dogs band
        // (docs/lookout/design-files/README.md:149).
        let flock = vec![
            ProcessInfo::builder(1, "web", ProcStatus::Online).build(),
            ProcessInfo::builder(90, "otel", ProcStatus::Online)
                .pid(Some(90_000))
                .dog(Some(shep_core::protocol::DogSource::BuiltIn))
                .build(),
        ];
        let app = super::super::fixtures::app_with(flock, super::super::fixtures::coloured());
        let width = 60;
        let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
        terminal.draw(|frame| draw(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let text = crate::lookout::frames::render_text(buffer);
        let lines: Vec<&str> = text.lines().collect();
        let flock_y = u16::try_from(
            lines
                .iter()
                .position(|l| l.contains("FLOCK"))
                .expect("a FLOCK band is drawn"),
        )
        .unwrap();
        let dogs_y = u16::try_from(
            lines
                .iter()
                .position(|l| l.contains("DOGS"))
                .expect("a DOGS band is drawn"),
        )
        .unwrap();
        let flock_fg = buffer
            .cell((super::super::flock::GUTTER, flock_y))
            .unwrap()
            .fg;
        let dogs_fg = buffer
            .cell((super::super::flock::GUTTER, dogs_y))
            .unwrap()
            .fg;
        assert_ne!(
            flock_fg, dogs_fg,
            "the flock and dogs bands must carry different roles"
        );
    }

    #[test]
    fn the_section_bands_name_their_section_in_words() {
        let flock = section_band(
            "FLOCK",
            crate::vocabulary::Role::Meadow,
            &super::super::fixtures::coloured(),
            40,
        );
        assert!(flock.spans.iter().any(|s| s.content.contains("FLOCK")));
    }

    /// The refusal is the one screen the keymap overlay does not cover, and
    /// `draw`'s own doc says so, so something has to check it.
    ///
    /// It reads as free, since `draw` returns before the overlay call. It is
    /// not: that early return is the only thing holding it, and the whole
    /// point of moving the overlay call out of the six body exits was that
    /// a call made in one place is easy to move to another. Drawing a box
    /// over `too small` would bury the one sentence naming the fix.
    #[test]
    fn the_refusal_gets_no_keymap_overlay_over_it() {
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            Instant::now(),
        );
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.keymap_open(), "the overlay did not open");

        // Roomy enough for the box at 130 columns, three rows short of a
        // body: `MIN_HEIGHT` is what refuses here, not the width, so the
        // overlay would have had the room it needs.
        let frame = draw_to(&app, 130, 3);
        assert_eq!(frame.lines().next().unwrap().trim_end(), "too small");
        // `lookout::keymap`, not `lookout::view::keymap`. Two modules carry
        // that name: this one's sibling draws the overlay, and `Group` lives
        // in the parent's, beside the bindings it groups. Spelled out
        // rather than shortened, since the two are easy to conflate.
        for group in crate::lookout::keymap::Group::DRAWN {
            assert!(
                !frame.contains(group.heading()),
                "the overlay drew {} over the refusal: {frame}",
                group.heading()
            );
        }
    }

    /// A bare empty screen does not tell an operator whether the shepherd
    /// has nothing to run or the dashboard is broken.
    #[test]
    fn an_empty_flock_still_prints_the_header_and_says_it_is_empty() {
        let app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            Instant::now(),
        );
        let frame = draw_to(&app, 100, 12);
        assert!(frame.contains("STATUS"));
        assert!(frame.contains("the flock is empty"));
    }

    /// A filter matching nothing is not the same as an empty flock; that
    /// sentence belongs to the case it describes.
    #[test]
    fn a_filter_matching_nothing_does_not_say_the_flock_is_empty() {
        let app = super::super::fixtures::filtered_app("zzz");
        let frame = draw_to(&app, 120, 30);
        assert!(
            frame.contains("no sheep's name contains \"zzz\""),
            "the table body names the query: {frame:?}"
        );
        assert!(
            !frame.contains("the flock is empty"),
            "and does not claim the flock is: {frame:?}"
        );
        assert!(
            frame.contains("no sheep selected: no name contains \"zzz\""),
            "the detail pane says its own reason: {frame:?}"
        );
        assert!(
            frame.contains("BLEATS no sheep is selected"),
            "the feed's sentence is already true and is unchanged: {frame:?}"
        );
    }

    /// The mirror of the test above: swapped branches would still pass
    /// either test alone.
    #[test]
    fn an_empty_flock_still_says_the_flock_is_empty() {
        let app = super::super::fixtures::filtered_app_of(Vec::new(), "");
        let frame = draw_to(&app, 120, 30);
        assert!(frame.contains("the flock is empty"), "got {frame:?}");
        assert!(!frame.contains("no sheep's name contains"), "got {frame:?}");
    }

    /// The regression the padding step in `row_line`/`group_line` guards
    /// against: a `Span`'s background only paints the cells under its own
    /// text, so a row styled only to the end of its content would leave a
    /// ragged, unpainted tail rather than a full row. Checked against the
    /// live `Buffer`'s own cells, not the rendered text, since a text-only
    /// assertion cannot see a background at all.
    #[test]
    fn the_selected_rows_ground_paints_every_column_of_the_table_not_just_its_text() {
        let mut app = App::new(
            Palette::detect(None, None, Some(std::ffi::OsStr::new("truecolor"))),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            Instant::now(),
        );
        app.update(Msg::Snapshot {
            rows: vec![
                ProcessInfo::builder(0, "web", ProcStatus::Online).build(),
                ProcessInfo::builder(1, "worker", ProcStatus::Online).build(),
            ],
            at: Instant::now(),
        });
        // Row 0 is selected by default. The gutter reads as a space either
        // way at this palette ([`super::super::flock::gutter`] paints rather than
        // switching glyphs), so the row is identified by content, not by
        // the marker character.
        let width = 100;
        let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
        terminal.draw(|frame| draw(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let text = crate::lookout::frames::render_text(buffer);
        let lines: Vec<&str> = text.lines().collect();
        // title, header, rule, "Flock" section: the selected sheep row is
        // the first one after them.
        let selected_y = 4;
        let unselected_y = 5;
        assert!(
            lines[selected_y].contains("web"),
            "row 0 is the selected one: {:?}",
            lines[selected_y]
        );
        assert!(
            lines[unselected_y].contains("worker"),
            "row 1 stays unselected: {:?}",
            lines[unselected_y]
        );

        let palette = app.palette();
        let ground = palette.ground().bg;
        assert!(
            ground.is_some(),
            "truecolor gets a real ground to paint with"
        );

        let table_width = width - super::super::flock::GUTTER;
        let painted = (super::super::flock::GUTTER..width)
            .filter(|&x| {
                buffer
                    .cell((x, u16::try_from(selected_y).unwrap()))
                    .is_some_and(|cell| Some(cell.bg) == ground)
            })
            .count();
        assert_eq!(
            painted,
            usize::from(table_width),
            "the ground must reach every column of the table, not just the text"
        );

        let unpainted = (super::super::flock::GUTTER..width)
            .filter(|&x| {
                buffer
                    .cell((x, u16::try_from(unselected_y).unwrap()))
                    .is_some_and(|cell| Some(cell.bg) == ground)
            })
            .count();
        assert_eq!(unpainted, 0, "an unselected row carries no ground at all");
    }

    /// Also found by capturing a real screen: at 90 columns the row under
    /// the band ran off the edge mid-word while the five lines below it all
    /// marked their own cuts, which reads as a rendering fault rather than
    /// as a narrow terminal.
    #[test]
    fn the_row_under_the_band_marks_its_own_truncation() {
        let mut app = super::super::fixtures::full_app();
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: super::super::fixtures::FROZEN_WHY.to_string(),
        });
        let under = |width| {
            draw_to(&app, width, 24)
                .lines()
                .nth(1)
                .expect("a second line")
                .to_string()
        };
        assert!(under(90).trim_end().ends_with('…'), "{:?}", under(90));
        assert!(
            under(160).contains("it will not exit on its own"),
            "and the whole sentence survives where it fits: {:?}",
            under(160)
        );
    }

    /// Found by capturing a real dashboard rather than by any test here:
    /// killing the shepherd leaves `the shepherd is shutting down` as a
    /// notice, notices outrank the key hint, and the bar then spends the
    /// rest of the session on a sentence about a process that is gone
    /// instead of on the three keys that still work.
    #[test]
    fn a_freeze_clears_the_notice_that_would_sit_on_the_key_hint() {
        let mut app = super::super::fixtures::full_app();
        app.update(Msg::BusLagged { count: 4 });
        assert!(app.notice().is_some(), "a notice to be cleared");

        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: super::super::fixtures::FROZEN_WHY.to_string(),
        });
        assert!(app.notice().is_none());

        let bar = draw_to(&app, 160, 30)
            .lines()
            .next_back()
            .expect("a status bar")
            .to_string();
        assert!(bar.contains("j/k g/G move"), "{bar:?}");
        assert!(
            !bar.contains("   r "),
            "the bar must not offer a key a freeze has already refused: {bar:?}"
        );
        assert!(bar.contains("\u{2588} frozen"), "{bar:?}");
    }

    /// Last values stay on screen, under a band that says in words how
    /// stale they are.
    #[test]
    fn a_frozen_link_says_so_in_the_band_and_keeps_the_home_path_below_it() {
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            Instant::now(),
        );
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: super::super::fixtures::FROZEN_WHY.to_string(),
        });
        // 160, the design target: the row below the band carries three
        // clauses and a `$SHEP_HOME`, and a narrower terminal truncates the
        // tail rather than rewrapping it.
        let frame = draw_to(&app, 160, 12);
        let mut lines = frame.lines();

        let band = lines.next().expect("a title band");
        assert!(band.contains("THE SHEPHERD HAS DIED"), "{band:?}");
        assert!(band.contains("2026-08-14 14:32:07"), "{band:?}");
        assert!(
            !band.contains("in the flock"),
            "the flock count is one more frozen number: {band:?}"
        );

        // The row the band displaced. `$SHEP_HOME` is the one thing on the
        // title row an operator still needs, since it says which dashboard
        // they are looking at.
        let under = lines.next().expect("a second line");
        assert!(under.contains("/home/ada/.shep"), "{under:?}");
        assert!(under.contains("it will not exit on its own"), "{under:?}");
    }

    /// An operator who does not know the control state is one keystroke
    /// from finding out the wrong way.
    #[test]
    fn the_status_bar_always_says_which_control_state_is_in_force() {
        let now = Instant::now();
        let read_only = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            now,
        );
        assert!(draw_to(&read_only, 100, 12).contains("read-only"));

        let allowed = App::new(
            Palette::detect(None, None, None),
            Control::Allowed,
            "/home/ada/.shep".to_string(),
            now,
        );
        let frame = draw_to(&allowed, 100, 12);
        assert!(frame.contains("control enabled"));
        assert!(!frame.contains("read-only"));
    }

    /// The settings screen's own twin of the sweep above. `Rect::height`
    /// is `bottom - y`, computed once per frame rather than checked, so
    /// this is what proves `MIN_HEIGHT` (6) actually keeps it from
    /// underflowing at the floor the guard claims to cover.
    #[test]
    fn drawing_the_settings_screen_never_panics_across_the_size_sweep() {
        let app = super::super::fixtures::app_in_settings();
        for (width, height) in [(1, 1), (20, 3), (33, 6), (80, 24), (250, 60), (400, 200)] {
            let _ = draw_to(&app, width, height);
        }
    }

    /// `F` used to draw the flock's fourteen-column header over an eight-
    /// column fold table: `view::mod`'s draw loop called
    /// `super::super::flock::columns_for` unconditionally for both the header and the
    /// rows under it, never branching on `App::grouping`. This pins the fix:
    /// the STATUS label in the column header and the STATUS word in a
    /// fold's own member row must land at the same column.
    #[test]
    fn the_fold_views_header_and_its_rows_share_one_column_set() {
        let mut app = super::super::fixtures::app_with(
            vec![super::super::fixtures::sheep_in_fold(
                1,
                "api",
                Some("edge"),
            )],
            super::super::fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        let text = draw_to(&app, 120, 16);
        let lines: Vec<&str> = text.lines().collect();
        let header = lines
            .iter()
            .find(|line| line.contains("STATUS"))
            .expect("the column header row is drawn");
        // `api`'s own member row, not `edge`'s fold header row: the header
        // row always computes its own width from `fold_columns_for`
        // regardless of what it is passed, so it would pass this test even
        // with the flat table's columns wired in behind it. A member row
        // has no such fallback, so it is the row that actually exercises
        // the wiring this test means to pin.
        let member_row = lines
            .iter()
            .find(|line| line.contains("api"))
            .expect("the fold's one member row is drawn");
        // Character offsets, not `str::find`'s byte offsets: a name column
        // wide enough to carry `\u{d7}` shifts a byte offset out of step
        // with the display column this test means to compare.
        fn char_position(line: &str, needle: &str) -> Option<usize> {
            let byte = line.find(needle)?;
            Some(line[..byte].chars().count())
        }
        let header_status_at = char_position(header, "STATUS").expect("checked above");
        let row_status_at =
            char_position(member_row, "online").expect("the fold's one member is online");
        assert_eq!(
            header_status_at, row_status_at,
            "header:\n{header}\nrow:\n{member_row}"
        );
    }

    /// A fold header reads as one with every colour stripped, and says
    /// whether it is collapsed.
    ///
    /// Design rule 3 (`docs/lookout/design-files/README.md:47`): "Strip every
    /// glyph and colour and the frame still reads." Before the disclosure
    /// triangle, brightness was the only thing separating a fold header from
    /// the member row beneath it, and a collapsed fold was marked by nothing
    /// at all. Below the `FOLD_ALL` tier there is no Share or Notes cell to
    /// rescue either, and that tier needs 158 columns, so the common terminal
    /// was the one that lost the hierarchy.
    ///
    /// Drawn at 120 columns through `super::super::fixtures::plain`, which is
    /// `Palette::detect(None, None, None)` and carries no colour, so this
    /// fails the way a `NO_COLOR` terminal would.
    #[test]
    fn a_fold_header_reads_as_one_without_any_colour() {
        let mut app = super::super::fixtures::app_with(
            vec![
                super::super::fixtures::sheep_in_fold(1, "api", Some("edge")),
                super::super::fixtures::sheep_in_fold(2, "cdn", Some("edge")),
            ],
            super::super::fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::FoldView));

        let expanded = draw_to(&app, 120, 16);
        assert!(
            expanded.contains("\u{25be} edge"),
            "an expanded fold points down: {expanded}"
        );
        assert!(
            !expanded.contains("\u{25be} api"),
            "a member row carries no triangle: {expanded}"
        );

        app.select_fold_for_tests("edge");
        let _ = app.update(Msg::Key(KeyPress::Collapse));
        let collapsed = draw_to(&app, 120, 16);
        assert!(
            collapsed.contains("\u{25b8} edge"),
            "a collapsed fold points right: {collapsed}"
        );
        assert!(
            !collapsed.contains("\u{25be} edge"),
            "and never both ways at once: {collapsed}"
        );
    }
}
