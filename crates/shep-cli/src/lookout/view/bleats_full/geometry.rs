//! The bleats pane's row geometry: how tall one feed line draws, which
//! survivors a window of body rows shows, how far one page key moves the
//! scroll offset, and the rows a line actually paints.

use ratatui::text::{Line, Span};

use super::super::super::app::App;
use super::super::super::pane_bleats::{BleatsPane, Filters};
use super::super::super::tail::{Stream, TailLine};
use super::super::feed_ink;
use crate::output::width::char_columns;
use crate::vocabulary::Role;

/// The stream tag's own width, shared by [`feed_line_rows`] (what it
/// indents a wrapped line's continuation rows under) and [`page_amount_up`]
/// (what it reserves before measuring a line's row cost): `"out  "` and
/// `"err  "` are both three letters and two trailing spaces.
pub(super) const TAG_PREFIX_WIDTH: u16 = 5;

/// How many rows rendering `text` at `text_width` columns costs. `1`
/// outright when `wrap` is off or there is no column count yet.
///
/// Wrapped, walks `text`'s own characters with [`char_columns`], not
/// [`str::chars`]'s count: a double-width character that would overflow the
/// current row starts a new one, the same walk [`wrap_spans`] draws by.
pub(super) fn row_height(text: &str, text_width: u16, wrap: bool) -> usize {
    if !wrap || text_width == 0 {
        return 1;
    }
    let width = usize::from(text_width);
    let mut rows = 1usize;
    let mut col = 0usize;
    for c in text.chars() {
        let w = char_columns(c);
        if col > 0 && col + w > width {
            rows += 1;
            col = 0;
        }
        col += w;
    }
    rows
}

/// The half-open range of `survivors`' indices [`super::lines`] draws, oldest
/// first, for a window of `body_rows` rows ending on the line
/// `scroll_offset` counts back from the tail, `row_height` naming each
/// index's own row cost.
///
/// Walks backward from the tail spending each line's row cost, and shifts the
/// far edge by `scroll_offset` lines, not rows. Both walks keep one line.
pub(super) fn window_range(
    len: usize,
    scroll_offset: usize,
    body_rows: usize,
    row_height: impl Fn(usize) -> usize,
) -> std::ops::Range<usize> {
    if len == 0 || body_rows == 0 {
        return 0..0;
    }
    let base = tail_window_start(len, body_rows, &row_height);
    let skip = base.saturating_sub(scroll_offset);
    let mut end = skip;
    let mut used = 0usize;
    while end < len {
        let cost = row_height(end);
        if used > 0 && used + cost > body_rows {
            break;
        }
        end += 1;
        used += cost;
    }
    skip..end
}

/// Where the tail window starts: walks back from `len` spending each line's
/// row cost until `body_rows` is gone, and always keeps one line once `len`
/// is non-zero.
///
/// Shared by [`window_range`], [`max_scroll_offset`] and [`page_lines_back`]
/// because the ceiling is the window's own start index, and a backward page
/// is the distance back to it.
fn tail_window_start(len: usize, body_rows: usize, cost: impl Fn(usize) -> usize) -> usize {
    let mut base = len;
    let mut used = 0usize;
    while base > 0 {
        let row = cost(base - 1);
        if used > 0 && used + row > body_rows {
            break;
        }
        base -= 1;
        used += row;
    }
    base
}

/// The largest scroll offset that still changes what the pane draws.
///
/// [`window_range`] floors its skip at zero, so an offset past the oldest
/// surviving line renders the same frame as the one before it. Without a
/// ceiling the stored offset keeps climbing, and `j` does nothing until it
/// unwinds.
#[must_use]
pub(crate) fn max_scroll_offset(app: &App, pane: &BleatsPane) -> usize {
    let survivors = pane.visible(&app.feed().lines, &app.feed_classifier());
    let body_rows = pane.body_rows();
    if body_rows == 0 || survivors.is_empty() {
        return 0;
    }
    let text_width = pane.width().saturating_sub(TAG_PREFIX_WIDTH);
    let wrap = pane.wrapped() && pane.width() > 0;
    let cost = |index: usize| row_height(&survivors[index].text, text_width, wrap);

    tail_window_start(survivors.len(), body_rows, cost)
}

/// Lines one backward page covers: the distance from `start` back to where
/// [`tail_window_start`] stops spending `body_rows`, never below one.
///
/// Pure, and separated from [`page_amount_up`] so the invariant that matters
/// can be tested over cost profiles rather than hunted for through a fixture.
fn page_lines_back(start: usize, body_rows: usize, cost: impl Fn(usize) -> usize) -> usize {
    (start - tail_window_start(start, body_rows, &cost)).max(1)
}

/// Lines one forward page covers: the lines the window is showing.
///
/// Pure and named, mirroring [`page_lines_back`], so the property test can
/// call the same code the key does. Its first version re-implemented this
/// expression inline, which pinned the arithmetic and left the wiring free:
/// `page_amount_down` could return a constant and every test still passed.
fn page_lines_forward(shown: &std::ops::Range<usize>) -> usize {
    shown.len().max(1)
}

/// How many lines one `ctrl-u` moves [`BleatsPane::scroll_offset`] by: what
/// fits walking backward from the line the pane is currently showing first.
///
/// The two page keys need different arithmetic: a page has to land the next
/// window's last line on this one's first, so the backward step is sized by
/// what fits ending where the view starts. [`page_amount_down`] is the mirror.
#[must_use]
pub(crate) fn page_amount_up(app: &App, pane: &BleatsPane) -> usize {
    let body_rows = pane.body_rows();
    if !pane.wrapped() || pane.width() == 0 {
        return body_rows.max(1);
    }
    let survivors = pane.visible(&app.feed().lines, &app.feed_classifier());
    let text_width = pane.width().saturating_sub(TAG_PREFIX_WIDTH);
    let cost = |index: usize| row_height(&survivors[index].text, text_width, true);
    let shown = window_range(survivors.len(), pane.scroll_offset(), body_rows, cost);
    page_lines_back(shown.start, body_rows, cost)
}

/// How many lines one `ctrl-d` moves [`BleatsPane::scroll_offset`] by: the
/// lines the pane is showing right now.
///
/// Going forward the next window should *begin* where this one ended, and
/// stepping by the current window's own length puts it there exactly. That
/// is the mirror of [`page_amount_up`]'s backward count, not the same
/// number: see its doc for why one figure cannot serve both keys.
#[must_use]
pub(crate) fn page_amount_down(app: &App, pane: &BleatsPane) -> usize {
    let body_rows = pane.body_rows();
    if !pane.wrapped() || pane.width() == 0 {
        return body_rows.max(1);
    }
    let survivors = pane.visible(&app.feed().lines, &app.feed_classifier());
    let text_width = pane.width().saturating_sub(TAG_PREFIX_WIDTH);
    let shown = window_range(survivors.len(), pane.scroll_offset(), body_rows, |index| {
        row_height(&survivors[index].text, text_width, true)
    });
    page_lines_forward(&shown)
}

/// One feed line, as the rows it actually draws: one row when
/// [`BleatsPane::wrapped`] is off, unchanged from before this task, or as
/// many as `text_width` columns force otherwise. Every row after the first
/// carries a blank indent the width of the stream tag rather than repeating
/// it, so a wrapped line's continuation reads as one entry, not several.
pub(super) fn feed_line_rows(
    app: &App,
    filters: &Filters,
    line: &TailLine,
    width: u16,
    wrap: bool,
) -> Vec<Line<'static>> {
    let palette = app.palette();
    let tag = match line.stream {
        Stream::Out => "out",
        Stream::Err => "err",
    };
    // Muted, both of them: the word carries the meaning, and a red `err`
    // would say a stderr line is damage. See `view::bleats::feed_lines` for
    // the same choice.
    let prefix = format!("{tag}  ");
    let text_width = width.saturating_sub(TAG_PREFIX_WIDTH);
    let ranges = filters.match_ranges(&line.text);
    let ink = feed_ink::Ink::filtered(app.data_palette(), &ranges, palette.band(Role::Butter));
    if !wrap {
        let mut spans = vec![Span::styled(prefix, palette.muted())];
        spans.extend(feed_ink::fitted_spans(line, ink, text_width));
        return vec![Line::from(spans)];
    }
    let spans = feed_ink::text_spans(line, ink);
    let indent = " ".repeat(usize::from(TAG_PREFIX_WIDTH));
    wrap_spans(spans, usize::from(text_width))
        .into_iter()
        .enumerate()
        .map(|(i, row_spans)| {
            let lead = if i == 0 { &prefix } else { &indent };
            let mut all = vec![Span::styled(lead.clone(), palette.muted())];
            all.extend(row_spans);
            Line::from(all)
        })
        .collect()
}

/// Splits `spans` into rows of at most `width` columns, breaking mid-span
/// where a span crosses the boundary rather than moving the whole span to
/// the next row: a match highlighted across a wrap point keeps its style on
/// both halves instead of jumping there whole. Measures with
/// [`char_columns`], the same walk [`row_height`] counts rows by, so the
/// two never disagree on how many rows one line takes. `width == 0` returns
/// every span on one row, since there is no boundary to wrap against.
fn wrap_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Vec<Span<'static>>> {
    if width == 0 {
        return vec![spans];
    }
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut col = 0usize;
    for span in spans {
        let style = span.style;
        let mut buf = String::new();
        for c in span.content.chars() {
            let w = char_columns(c);
            if col > 0 && col + w > width {
                if !buf.is_empty() {
                    rows.last_mut()
                        .expect("rows always has a current row")
                        .push(Span::styled(std::mem::take(&mut buf), style));
                }
                rows.push(Vec::new());
                col = 0;
            }
            buf.push(c);
            col += w;
        }
        if !buf.is_empty() {
            rows.last_mut()
                .expect("rows always has a current row")
                .push(Span::styled(buf, style));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `row_height` counts display columns, and `wrap_spans` has to agree
    /// with it or the page arithmetic drifts from what is drawn.
    ///
    /// `row_height` feeds every page and window calculation while
    /// `wrap_spans` produces the rows themselves. Mutating `char_columns` to
    /// `1` inside `row_height` passed the whole suite before this test.
    #[test]
    fn row_height_and_the_rendered_rows_agree_on_a_wide_line() {
        // Ten double-width characters are 20 columns. At a text width of 8
        // that is 3 rows; counting `char`s would say 2.
        let wide = "\u{5e83}".repeat(10);
        assert_eq!(row_height(&wide, 8, true), 3, "counted by columns");
        assert_eq!(
            row_height(&wide, 8, true),
            wrap_spans(vec![Span::raw(wide.clone())], 8).len(),
            "the counter and the renderer agree"
        );

        // And unwrapped it is always one row, whatever the width.
        assert_eq!(row_height(&wide, 8, false), 1);
    }

    /// Over the arithmetic rather than through a fixture: a feed of uniform
    /// row heights cannot show this at all, because a backward count and a
    /// window's own length are the same number there.
    ///
    /// The invariant: after a page, the next window must start no later than
    /// one past where the last one ended, and end no earlier than one before
    /// where it began. Anything else is a line no screen drew.
    #[test]
    fn neither_page_direction_steps_over_a_line() {
        // A cheap deterministic spread of heights: no rng, and the profile
        // is reproducible from the seed in a failure message.
        let profile = |seed: usize, index: usize| -> usize {
            1 + (seed.wrapping_mul(31).wrapping_add(index.wrapping_mul(17))) % 4
        };

        for seed in 0..200usize {
            for len in [3usize, 7, 12, 25] {
                for body_rows in [2usize, 3, 5, 8] {
                    let cost = |index: usize| profile(seed, index);
                    for offset in 0..len {
                        let shown = window_range(len, offset, body_rows, cost);
                        if shown.is_empty() {
                            continue;
                        }

                        let back = page_lines_back(shown.start, body_rows, cost);
                        let up = window_range(len, offset + back, body_rows, cost);
                        assert!(
                            up.end + 1 >= shown.start,
                            "seed {seed} len {len} rows {body_rows} offset {offset}: \
                             ctrl-u went from {shown:?} to {up:?}"
                        );

                        let forward = page_lines_forward(&shown);
                        let down =
                            window_range(len, offset.saturating_sub(forward), body_rows, cost);
                        assert!(
                            down.start <= shown.end,
                            "seed {seed} len {len} rows {body_rows} offset {offset}: \
                             ctrl-d went from {shown:?} to {down:?}"
                        );
                    }
                }
            }
        }
    }

    /// A backward page is never zero lines wide, so `ctrl-u` always moves
    /// [`BleatsPane::scroll_offset`]: at the oldest survivor, and over a
    /// first line taller than the body.
    #[test]
    fn page_lines_back_steps_at_least_one_line() {
        assert_eq!(page_lines_back(0, 4, |_| 1), 1);
        assert_eq!(page_lines_back(3, 2, |_| 5), 1);
        assert_eq!(page_lines_back(6, 4, |_| 2), 2);
        assert_eq!(page_lines_back(5, 4, |i| if i == 4 { 3 } else { 1 }), 2);
    }
}
