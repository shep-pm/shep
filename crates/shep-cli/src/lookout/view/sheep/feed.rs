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

use super::super::detail::chip_text;
use super::super::flock::fit;
use super::{COLUMN_BODY_ROWS, FEED_WIDTH, FEED_X};

/// `top` (the same row [`draw_column`](super::column::draw_column) and [`draw_divider`](super::column::draw_divider) were handed)
/// through [`COLUMN_BODY_ROWS`] rows below it, right of the divider: the
/// header, then the window's newest surviving lines that fit, oldest at
/// the top.
///
/// Reads [`SheepPane::feed_sheep`]'s tail through [`App::feed`], never
/// [`App::selected`]: the same rule [`draw`](super::draw)'s own identity band, charts and
/// config column already follow, now enforced one level up too, in
/// [`App::feed_row`] itself: `app.feed()` already answers with the pinned
/// sheep's own lines while this pane is open, not the dashboard's selection.
///
/// No scrolling: unlike [`super::super::bleats_full::draw`], this column has no
/// cursor of its own to page through, so a line past what fits is only
/// counted by the header's own `N earlier` clause, never drawn.
pub(super) fn draw_feed(
    app: &App,
    pane: &SheepPane,
    top: u16,
    area: Rect,
    buffer: &mut Buffer,
    palette: Palette,
) {
    let feed = pane.feed();
    let tail = app.feed();
    let levels = app.feed_classifier();
    // Filtered once for both halves. The header counts what the body does
    // not show, so computing it twice is how the two come to disagree, and
    // it also compiled the classifier's regexes a second time per frame.
    let survivors = feed.visible(&tail.lines, &levels);
    let hidden = survivors.len().saturating_sub(COLUMN_BODY_ROWS);
    write_feed_row(buffer, area, top, &feed_header_line(feed, hidden, palette));

    for (i, line) in survivors.iter().skip(hidden).enumerate() {
        write_feed_row(buffer, area, top + 1 + i as u16, &feed_line(line, palette));
    }
}

/// One feed line: the stream tag, muted the same way
/// [`super::super::bleats::feed_lines`] draws it (stderr is most runtimes' default,
/// not `--bark`), then the text, truncated rather than wrapped, since this
/// column has no row budget to spend on a second line for one that
/// overruns.
fn feed_line(line: &TailLine, palette: Palette) -> Line<'static> {
    let tag = match line.stream {
        Stream::Out => "out",
        Stream::Err => "err",
    };
    Line::from(vec![
        Span::styled(format!("{tag}  "), palette.muted()),
        Span::raw(fit(&line.text, FEED_WIDTH.saturating_sub(5))),
    ])
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
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use crate::lookout::app::{Body, KeyPress, Msg, RowKey};
    use crate::lookout::frames::render_text;
    use crate::lookout::level::{Classifier, Level};
    use crate::lookout::tail::Tail;

    use super::super::super::fixtures;
    use super::super::{MIN_HEIGHT_FOR_COLUMN, draw};
    use super::*;

    /// `draw_feed` draws whatever `App::feed` holds, filtered through the
    /// pane's own pinned `BleatsPane`, and never reaches for `App::selected`
    /// itself: this pins that half. It does *not* pin `App::feed_row`'s own
    /// scoping to the pane's pinned sheep rather than the reseated
    /// selection: a fixture with no polling loop cannot re-fetch the tail
    /// after the reseat below, so the tail this test asserts on is exactly
    /// the one `Msg::Bleats` injected before it, whatever `feed_row` would
    /// answer now. `the_feed_row_follows_the_sheep_panes_own_pinned_sheep_when_the_selection_moves`
    /// in `app.rs` is what pins that half; a mutation removing `feed_row`'s
    /// own `Body::Sheep` branch left this test green.
    ///
    /// The area is 160x46, at [`FEED_X`] plus [`FEED_WIDTH`] and
    /// [`MIN_HEIGHT_FOR_COLUMN`] exactly: `draw`'s own gate skips
    /// `draw_feed` below either floor, and a narrower or shorter area would
    /// pass this test whether or not `draw_feed` ever ran, the same way a
    /// too-small area let Task 8's own chart bug through once.
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
                lines: vec![TailLine {
                    stream: Stream::Out,
                    text: "alpha wrote this".to_string(),
                }],
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
                .map(|n| TailLine {
                    stream: Stream::Out,
                    text: format!("line-{n}"),
                })
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
                    .map(|n| TailLine {
                        stream: Stream::Out,
                        text: format!("line-{n}"),
                    })
                    .chain(std::iter::once(TailLine {
                        stream: Stream::Err,
                        text: "x".repeat(200),
                    }))
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
