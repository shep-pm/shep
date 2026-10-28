//! The bleats feed embedded to the right of the divider: the window's
//! newest surviving lines, and the header that says what was filtered out.
//!
//! Not a strip of its own and not [`super::super::bleats_full`]: this
//! column has no cursor to page with, so a line past what fits is counted
//! by the header's own `N earlier` clause and never drawn.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::lookout::app::App;
use crate::lookout::pane_bleats::{BleatsPane, Filters, MatchKind};
use crate::lookout::pane_sheep::SheepPane;
use crate::lookout::tail::{Stream, TailLine};
use crate::lookout::theme::Palette;

use super::super::bleats::unavailable_line;
use super::super::detail::chip_text;
use super::super::feed_ink;
use super::super::flock::fit;
use super::layout::{COLUMN_BODY_ROWS, FEED_WIDTH, FEED_X};

/// From `top`, through [`COLUMN_BODY_ROWS`] rows, right of the divider:
/// the header, then the window's newest lines that fit, oldest at the
/// top.
///
/// Reads [`SheepPane::feed_sheep`]'s tail through [`App::feed`], never
/// [`App::selected`]. No scrolling: a line past what fits is only counted
/// by the header's `N earlier`.
pub(super) fn draw_feed(
    app: &App,
    pane: &SheepPane,
    top: u16,
    area: Rect,
    buffer: &mut Buffer,
    palette: Palette,
) {
    if app.remote_host().is_some() {
        write_feed_row(buffer, area, top, &unavailable_line(palette, FEED_WIDTH));
        return;
    }
    let feed = pane.feed();
    let tail = app.feed();
    let levels = app.feed_classifier();
    // Filtered once for both halves. The header counts what the body does
    // not show, so computing it twice is how the two come to disagree, and
    // it also compiled the classifier's regexes a second time per frame.
    let survivors = feed.visible(&tail.lines, &levels);
    let hidden = survivors.len().saturating_sub(COLUMN_BODY_ROWS);
    let ink = feed_ink::Ink::plain(app.data_palette());
    write_feed_row(buffer, area, top, &feed_header_line(feed, hidden, palette));

    for (i, line) in survivors.iter().skip(hidden).enumerate() {
        write_feed_row(
            buffer,
            area,
            top + 1 + i as u16,
            &feed_line(line, palette, ink),
        );
    }
}

/// One feed line: the stream tag, muted the same way
/// [`super::super::bleats::feed_lines`] draws it, then the text, truncated
/// rather than wrapped, since this column has no row budget for a second
/// line.
///
/// The text goes through [`feed_ink`] like the other two feed draws.
/// `App::data_palette` is the palette, so a lost link draws it muted.
fn feed_line(line: &TailLine, palette: Palette, ink: feed_ink::Ink<'_>) -> Line<'static> {
    let tag = match line.stream {
        Stream::Out => "out",
        Stream::Err => "err",
    };
    let mut spans = vec![Span::styled(format!("{tag}  "), palette.muted())];
    spans.extend(feed_ink::fitted_spans(
        line,
        ink,
        FEED_WIDTH.saturating_sub(5),
    ));
    Line::from(spans)
}

/// The feed's header row: the `BLEATS` chip, then `out then err`, then how
/// many of the window's surviving lines this column has no room to show,
/// then a bracketed chip per filter axis currently set, then `/ narrow`,
/// naming the one key this row does not otherwise spell out.
fn feed_header_line(feed: &BleatsPane, hidden: usize, palette: Palette) -> Line<'static> {
    let chip = chip_text("BLEATS");
    let chip_width = u16::try_from(chip.chars().count() + 1).unwrap_or(FEED_WIDTH);
    let budget = FEED_WIDTH.saturating_sub(chip_width);
    Line::from(vec![
        Span::styled(chip, palette.band(crate::vocabulary::Role::Butter)),
        Span::raw(" "),
        Span::styled(
            fit(&feed_header_text(feed, hidden), budget),
            palette.muted(),
        ),
    ])
}

/// [`feed_header_line`]'s own sentence, built separately so a test can pin
/// its wording without rendering a [`Line`] back into a string.
fn feed_header_text(feed: &BleatsPane, hidden: usize) -> String {
    let mut parts = vec!["out then err".to_string()];
    if hidden > 0 {
        parts.push(format!("{hidden} earlier"));
    }
    for chip in feed_chip_labels(feed.filters()) {
        parts.push(format!("[{chip}]"));
    }
    parts.push("/ narrow".to_string());
    parts.join(" \u{b7} ")
}

/// One chip's text per filter axis currently set on the embedded feed, in
/// field order. Deliberately its own, smaller vocabulary rather than
/// `view::bleats_full`'s private `chip_labels`: this row has 83 cells for
/// the whole sentence, not a dedicated filter row underneath it. The match
/// axis's suffix is the one exception, shared as [`MatchKind::chip_suffix`].
fn feed_chip_labels(filters: &Filters) -> Vec<String> {
    let mut chips = Vec::new();
    if let Some(stream) = filters.stream {
        chips.push(match stream {
            Stream::Out => "out only".to_string(),
            Stream::Err => "err only".to_string(),
        });
    }
    if let Some(min) = filters.min_level {
        chips.push(format!("level\u{2265}{min}"));
    }
    if let Some(text) = filters.match_text() {
        let suffix = filters.match_kind().map_or("", MatchKind::chip_suffix);
        chips.push(format!("match {text}{suffix}"));
    }
    chips
}

/// Writes one already-styled line into `buffer`, `row` cells below `area`'s
/// own top and [`FEED_X`] cells right of its own left edge, clipped to
/// [`FEED_WIDTH`].
fn write_feed_row(buffer: &mut Buffer, area: Rect, row: u16, line: &Line<'static>) {
    if row >= area.height {
        return;
    }
    buffer.set_line(area.x + FEED_X, area.y + row, line, FEED_WIDTH);
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use crate::lookout::app::{Body, KeyPress, Msg, RowKey};
    use crate::lookout::frames::render_text;
    use crate::lookout::level::{Classifier, Level};
    use crate::lookout::tail::Tail;
    use crate::vocabulary::Role;

    use super::super::super::fixtures;
    use super::super::draw;
    use super::super::layout::MIN_HEIGHT_FOR_COLUMN;
    use super::*;

    #[test]
    fn the_feed_does_not_draw_the_sheep_that_replaced_the_pinned_one() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(1, "alpha", ProcStatus::Online).build(),
                ProcessInfo::builder(2, "bravo", ProcStatus::Online).build(),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "setup: the pane opened on alpha"
        );
        app.update(Msg::Bleats {
            tail: Tail {
                lines: vec![TailLine::new(Stream::Out, "alpha wrote this")],
                missed_lines: 0,
                missed_bytes: 0,
                read_bytes: 0,
                note: None,
            },
        });
        let _ = app.update(Msg::Snapshot {
            rows: vec![ProcessInfo::builder(2, "bravo", ProcStatus::Online).build()],
            at: std::time::Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(2)),
            "setup: the reseat moved the selection to bravo"
        );

        let Body::Sheep(pane) = app.body() else {
            panic!("the pane is still open");
        };
        let area = Rect::new(0, 0, FEED_X + FEED_WIDTH, MIN_HEIGHT_FOR_COLUMN);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        let text = render_text(&buffer);
        assert!(
            text.contains("alpha wrote this"),
            "the pinned sheep's own line must still draw: {text:?}"
        );
    }

    /// The embedded column draws its text through [`feed_ink`] like the
    /// dashboard strip and the full-screen pane: a plain stdout line is meadow,
    /// a plain stderr line is bark, and a line that coloured itself keeps the
    /// colour it wrote, which [`TailLine::new`] has already taken out of `text`.
    #[test]
    fn the_embedded_feed_colours_a_plain_line_and_keeps_a_sheeps_own() {
        let palette = fixtures::coloured();
        let ink = feed_ink::Ink::plain(palette);
        let out = feed_line(
            &TailLine::new(Stream::Out, "alpha wrote this"),
            palette,
            ink,
        );
        let err = feed_line(&TailLine::new(Stream::Err, "warn: retrying"), palette, ink);
        let own = feed_line(
            &TailLine::new(Stream::Out, "\u{1b}[31mconnection refused\u{1b}[0m"),
            palette,
            ink,
        );

        assert_eq!(out.spans[0].style, palette.muted(), "the tag stays muted");
        assert_eq!(
            out.spans[1].style.fg,
            palette.ink(Role::Meadow).fg,
            "a plain stdout line is meadow"
        );
        assert_eq!(
            err.spans[1].style.fg,
            palette.ink(Role::Bark).fg,
            "a plain stderr line is bark"
        );
        assert_eq!(
            own.spans[1].content, "connection refused",
            "the escapes are out of the text the column measures"
        );
        assert_eq!(
            own.spans[1].style.fg,
            Some(Color::Red),
            "and the red the sheep wrote is still on it"
        );
    }

    /// A line too long for the column is cut the way [`fit`] cuts it, ellipsis
    /// included, and the ellipsis wears the colour of the run it cut into.
    #[test]
    fn the_embedded_feed_cuts_a_long_line_inside_its_own_run() {
        let palette = fixtures::coloured();
        let ink = feed_ink::Ink::plain(palette);
        let line = feed_line(
            &TailLine::new(
                Stream::Err,
                &format!("\u{1b}[31m{}\u{1b}[0m", "x".repeat(90)),
            ),
            palette,
            ink,
        );
        let body: String = line.spans[1..].iter().map(|s| s.content.as_ref()).collect();
        assert!(
            body.starts_with(&format!("{}\u{2026}", "x".repeat(77))),
            "77 characters plus the ellipsis in the column's 78: {body:?}"
        );
        assert!(!body.contains(&"x".repeat(78)), "and no further: {body:?}");
        let ellipsis = &line.spans[1];
        assert_eq!(
            ellipsis.style.fg,
            Some(Color::Red),
            "the ellipsis belongs to the run it landed in"
        );
    }

    /// [`feed_header_text`]'s own chip: `[level≥warn]`, the same word
    /// [`Level`]'s own doc says the filter row
    /// renders it as, once a minimum is set on the embedded feed.
    #[test]
    fn the_headers_level_chip_names_the_minimum() {
        let mut feed = BleatsPane::new(RowKey::Sheep(1));
        feed.set_min_level(Some(Level::Warn));
        let text = feed_header_text(&feed, 0);
        assert!(text.contains("[level≥warn]"), "got {text:?}");
        assert!(text.contains("/ narrow"), "got {text:?}");
    }

    /// A regex matcher's chip names itself a regex, the same suffix
    /// `bleats_full.rs`'s own `chip_labels` carries: an operator who
    /// narrows the embedded feed with `/…/` needs the same tell this
    /// column's full-screen twin already gives.
    #[test]
    fn the_headers_match_chip_names_a_regex() {
        let mut feed = BleatsPane::new(RowKey::Sheep(1));
        feed.set_match("/po+l/".to_string());
        let text = feed_header_text(&feed, 0);
        assert!(text.contains("match /po+l/ (regex)"), "got {text:?}");
    }

    /// A literal matcher's chip is the typed text and nothing after it.
    /// The two assertions around this one read a suffix they expect, so
    /// neither can see a suffix that should not be there; this one anchors
    /// on the brackets `feed_header_text` draws around each chip, which
    /// anything appended would fall outside of.
    #[test]
    fn the_headers_match_chip_carries_nothing_after_a_literal() {
        let mut feed = BleatsPane::new(RowKey::Sheep(1));
        feed.set_match("pool".to_string());
        let text = feed_header_text(&feed, 0);
        assert!(text.contains("[match pool]"), "got {text:?}");
    }

    /// A pattern that fails to compile says so on the chip, rather than the
    /// embedded feed just going quiet with no explanation until the
    /// operator presses `b` to reach the full-screen pane's own chip.
    #[test]
    fn the_headers_match_chip_names_an_invalid_regex() {
        let mut feed = BleatsPane::new(RowKey::Sheep(1));
        feed.set_match("/pool(/".to_string());
        let text = feed_header_text(&feed, 0);
        assert!(
            text.contains("invalid regex, matches nothing"),
            "got {text:?}"
        );
    }

    /// The `N earlier` clause counts survivors the body has no room to show,
    /// never the raw line count: with `COLUMN_BODY_ROWS` rows to draw into
    /// and one more line than that, exactly one line is hidden.
    #[test]
    fn the_headers_earlier_clause_counts_hidden_survivors_not_raw_lines() {
        let feed = BleatsPane::new(RowKey::Sheep(1));
        let tail = Tail {
            lines: (0..COLUMN_BODY_ROWS + 1)
                .map(|n| TailLine::new(Stream::Out, &format!("line-{n}")))
                .collect(),
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 0,
            note: None,
        };
        let hidden = feed
            .visible(&tail.lines, &Classifier::new(&[]))
            .len()
            .saturating_sub(COLUMN_BODY_ROWS);
        let text = feed_header_text(&feed, hidden);
        assert!(text.contains("1 earlier"), "got {text:?}");
    }

    /// The feed shows the window's newest lines, oldest at the top, and
    /// truncates a line too long for [`FEED_WIDTH`] rather than wrapping it
    /// onto a second row: this column has no row budget to spend on one.
    #[test]
    fn the_feed_shows_the_newest_lines_and_truncates_a_long_one() {
        let mut app = fixtures::app_with(
            vec![ProcessInfo::builder(1, "alpha", ProcStatus::Online).build()],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Bleats {
            tail: Tail {
                lines: (0..COLUMN_BODY_ROWS + 5)
                    .map(|n| TailLine::new(Stream::Out, &format!("line-{n}")))
                    .chain(std::iter::once(TailLine::new(
                        Stream::Err,
                        &"x".repeat(200),
                    )))
                    .collect(),
                missed_lines: 0,
                missed_bytes: 0,
                read_bytes: 0,
                note: None,
            },
        });
        let Body::Sheep(pane) = app.body() else {
            panic!("the pane is still open");
        };
        let area = Rect::new(0, 0, FEED_X + FEED_WIDTH, MIN_HEIGHT_FOR_COLUMN);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        let text = render_text(&buffer);
        assert!(
            !text.contains("line-0\n") && !text.contains("line-0 "),
            "the oldest lines scroll off the top: {text:?}"
        );
        assert!(
            text.contains(&format!("{}\u{2026}", "x".repeat(77))),
            "the long line truncates with an ellipsis at the column's own \
             width (78 cells: 77 characters plus the ellipsis), not before \
             it and not after: {text:?}"
        );
        assert!(
            !text.contains(&"x".repeat(78)),
            "and no further: a truncated line, not a wrapped one: {text:?}"
        );
    }
}
