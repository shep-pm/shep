//! The full-screen bleats pane: the same feed [`super::bleats`] draws in its
//! five-line strip, given the whole body instead, plus the filter row
//! stacked on it once an operator has set an axis.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::app::{App, RowKey};
use super::super::pane_bleats::{BleatsPane, Filters, MatchKind};
use super::super::tail::{FEED_WINDOW_BYTES, Stream, TailLine};
use super::cell;
use super::flock::fit;
use crate::output::human_bytes;
use crate::output::width::char_columns;
use crate::vocabulary::Role;

/// The stream tag's own width, shared by [`feed_line_rows`] (what it
/// indents a wrapped line's continuation rows under) and [`page_amount_up`]
/// (what it reserves before measuring a line's row cost): `"out  "` and
/// `"err  "` are both three letters and two trailing spaces.
const TAG_PREFIX_WIDTH: u16 = 5;

/// Draws the pane over the whole body: a title band naming the sheep and
/// both log paths, then, once any filter axis is set, the filter row, then
/// either the window's newest surviving lines, each tagged `out` or `err`
/// with the newest at the bottom, or, when there are none, [`Tail::note`]
/// naming why.
///
/// [`Tail::note`]: super::super::tail::Tail::note
pub fn draw(app: &App, pane: &BleatsPane, area: Rect, buffer: &mut Buffer) {
    let width = area.width;
    let rows = usize::from(area.height);
    for (offset, line) in lines(app, pane, width, rows).into_iter().enumerate() {
        let Ok(offset) = u16::try_from(offset) else {
            break;
        };
        let y = area.y + offset;
        if y >= area.y + area.height {
            break;
        }
        buffer.set_line(area.x, y, &line, width);
    }
}

/// [`draw`]'s own lines, without a [`Buffer`] round trip: what
/// [`super::frames::render_text`] would read back off the buffer `draw`
/// writes, built directly instead. Reads the pane through
/// [`App::bleats_pane`] rather than taking one as a parameter, the same
/// shape [`super::detail::detail_lines`] uses for the pane it draws.
///
/// # Panics
///
/// Panics if `app`'s body is not the bleats pane. Every caller only reaches
/// this after opening it.
///
/// `#[cfg(test)]`: reached from this module's tests, from
/// `view::fixtures::draw_lines`, and from roughly a dozen tests in `app.rs`
/// through that. It said "this module's own tests are the only caller" until
/// the pane grew keys, and the tests that drive them live in `app.rs`.
#[cfg(test)]
#[must_use]
pub(crate) fn draw_lines(app: &App, width: u16, rows: usize) -> Vec<Line<'static>> {
    let pane = app
        .bleats_pane()
        .expect("draw_lines is only called while the bleats pane is open");
    lines(app, pane, width, rows)
}

/// Shared by [`draw`] and [`draw_lines`], so the two never drift.
fn lines(app: &App, pane: &BleatsPane, width: u16, rows: usize) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(rows);
    out.push(title_line(app, pane, width));

    let filters = pane.filters();
    let feed = app.feed();
    let survivors = pane.visible(&feed.lines, &app.feed_classifier());

    if let Some(row) = filter_row_line(app, filters, survivors.len(), feed.lines.len(), width) {
        out.push(row);
    }

    if feed.lines.is_empty() {
        // The same rule `view::bleats::feed_lines` follows: the note names
        // why there is nothing, and only shows when there is nothing to
        // show instead of it, and only when a row remains for it.
        if rows > out.len()
            && let Some(note) = feed.note.as_deref()
        {
            out.push(Line::from(Span::styled(
                fit(note, width),
                app.palette().muted(),
            )));
        }
        return out;
    }

    let body_rows = rows.saturating_sub(out.len());
    let wrap = pane.wrapped();
    let text_width = width.saturating_sub(TAG_PREFIX_WIDTH);
    let window = window_range(survivors.len(), pane.scroll_offset(), body_rows, |i| {
        row_height(&survivors[i].text, text_width, wrap)
    });
    for line in &survivors[window] {
        out.extend(feed_line_rows(app, filters, line, width, wrap));
    }
    // A single line taller than the whole body (an extreme wrapped case, or
    // a body of zero rows) can still push `out` past `rows`: `window_range`
    // always includes at least one line so the pane never renders nothing,
    // even when that one line does not fit. This is the actual guarantee
    // `draw`'s own bounds check backs up for the terminal; `draw_lines`'s
    // tests call this directly, with no such check downstream.
    out.truncate(rows);
    out
}

/// How many rows rendering `text` at `text_width` columns costs. `1`
/// outright when `wrap` is off or there is no column count yet: one line,
/// one row, the pane's whole behavior before this task. Wrapped, walks
/// `text`'s own characters with [`char_columns`], not [`str::chars`]'s
/// count: a double-width character that would overflow the current row
/// starts a new one instead of being charged half a column, the same
/// walk [`wrap_spans`] draws by, so the two can never disagree on how many
/// rows one line takes.
fn row_height(text: &str, text_width: u16, wrap: bool) -> usize {
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

/// The half-open range of `survivors`' indices [`lines`] draws, oldest
/// first, for a window of `body_rows` rows ending on the line
/// `scroll_offset` counts back from the tail, `row_height` naming each
/// index's own row cost.
///
/// Generalizes the single formula this pane always used before wrap
/// existed (`skip = len - body_rows - offset`, `take = body_rows`) to a
/// line that may cost more than one row: walks backward from the tail
/// accumulating each line's cost until the budget is spent, which is
/// exactly that formula's arithmetic once every line costs one row (wrap
/// off). `scroll_offset` then shifts the far edge of that walk toward the
/// tail by `scroll_offset` *lines*, not rows — an offset counts filtered
/// lines regardless of how tall any of them draws — saturating at the
/// oldest survivor exactly the way the original subtraction did, so a
/// stale offset a shrunk feed or a narrowing filter has outgrown still
/// clamps rather than panicking or reading past the end. Both walks always
/// include at least one line once `len > 0`, even one whose own cost alone
/// exceeds `body_rows`: a body of one very long wrapped line is still one
/// line to show, not zero.
/// Where the tail window starts: walks back from `len` spending each line's
/// row cost until `body_rows` is gone, and always keeps one line once `len`
/// is non-zero.
///
/// Shared by [`window_range`] and [`max_scroll_offset`] because the ceiling
/// is defined as the window's own start index. Two copies of this walk would
/// have to be kept in step by hand, and the day they drifted the pane would
/// let an operator scroll somewhere it never draws.
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

fn window_range(
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

/// How many lines one `ctrl-u`/`ctrl-d` should move [`BleatsPane::scroll_offset`]
/// by, so a page under wrap moves by roughly the room `pane`'s own area
/// draws rather than a raw row count that assumes one row per line.
///
/// The largest scroll offset that still changes what the pane draws.
///
/// [`window_range`] floors its skip at zero, so an offset past the oldest
/// surviving line renders the same frame as the one before it. Without a
/// ceiling the stored offset keeps climbing: 40 `k` presses over a 20-line
/// feed leave it at 39 where 15 is the most that does anything, and the
/// operator then presses `j` 25 times before the window moves. `G` and `f`
/// escape that, but `j` is the reflex and it would do nothing.
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

    // Where the tail window starts. Scrolling past it only re-renders the
    // oldest frame, so that index is the ceiling.
    tail_window_start(survivors.len(), body_rows, cost)
}

/// Lines one backward page covers: what fits ending at `start`.
///
/// Pure, and separated from [`page_amount_up`] so the invariant that matters
/// can be tested directly over cost profiles rather than hunted for through a
/// fixture. A feed of uniform heights cannot expose a direction-mismatched
/// page size at all, because a backward count and a window's own length agree
/// there, which is how the `ctrl-d` gap survived its first fix.
fn page_lines_back(start: usize, body_rows: usize, cost: impl Fn(usize) -> usize) -> usize {
    let mut used = 0usize;
    let mut count = 0usize;
    let mut at = start;
    while at > 0 {
        let row = cost(at - 1);
        if count > 0 && used + row > body_rows {
            break;
        }
        at -= 1;
        used += row;
        count += 1;
    }
    count.max(1)
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

/// How many lines one `ctrl-u` moves [`BleatsPane::scroll_offset`] by:
/// what fits walking backward from the line the pane is currently showing
/// first.
///
/// Unwrapped that is `body_rows`, the arithmetic this pane always used.
///
/// **The two page keys need different arithmetic and it is not obvious.** A
/// page has to land the next window's last line on this one's first, so
/// going backward the step is sized by what fits *ending* where the view
/// starts. Sizing it from the feed's tail instead is wrong the moment
/// wrapped-line density differs anywhere else, and sizing it by the current
/// window's own length is wrong whenever the older stretch packs fewer lines
/// into the same rows. Both skip lines rather than overshooting them, so
/// [`window_range`]'s clamp does not save it.
///
/// [`page_amount_down`] is the mirror, and reusing this one for both
/// directions leaves gaps: measured over 279,274 simulated steps, 48,476 of
/// them dropped a line. `wrapped_pages_leave_no_line_unseen` and
/// `wrapped_pages_down_leave_no_line_unseen` walk one direction each,
/// because paging back is symmetric and hides the gap either way.
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
fn feed_line_rows(
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
    if !wrap {
        let text = fit(&line.text, text_width);
        let mut spans = vec![Span::styled(prefix, palette.muted())];
        spans.extend(highlighted(&text, filters, palette.band(Role::Butter)));
        return vec![Line::from(spans)];
    }
    let spans = highlighted(&line.text, filters, palette.band(Role::Butter));
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

/// Splits `text` into spans, styling every byte range [`Filters::match_ranges`]
/// reports in `highlight` and leaving the rest at the default style.
///
/// A test asserting only that the line renders (its text is present) cannot
/// tell this function apart from one that never highlights anything: see
/// `bleats_full::tests::a_match_axis_highlights_its_hit_and_only_its_hit`,
/// which inspects the spans themselves.
fn highlighted(text: &str, filters: &Filters, highlight: Style) -> Vec<Span<'static>> {
    let ranges = filters.match_ranges(text);
    if ranges.is_empty() {
        return vec![Span::raw(text.to_string())];
    }
    let mut spans = Vec::with_capacity(ranges.len() * 2 + 1);
    let mut cursor = 0;
    for (start, end) in ranges {
        if start > cursor {
            spans.push(Span::raw(text[cursor..start].to_string()));
        }
        spans.push(Span::styled(text[start..end].to_string(), highlight));
        cursor = end;
    }
    if cursor < text.len() {
        spans.push(Span::raw(text[cursor..].to_string()));
    }
    spans
}

/// The row under the title once any filter axis is set: a chip per set
/// axis, then the sentence naming how many of the window's lines survived
/// and how the axes compose. `None` while every axis is clear: with nothing
/// set, a row saying so would repeat what the plain feed already shows.
fn filter_row_line(
    app: &App,
    filters: &Filters,
    survivors: usize,
    total: usize,
    width: u16,
) -> Option<Line<'static>> {
    if filters.is_empty() {
        return None;
    }
    let palette = app.palette();
    let ground = palette.ground();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used: u16 = 0;
    for chip in chip_labels(filters) {
        let text = format!(" {chip} ");
        // Columns, not `char`s, because `fit` below budgets columns: a match
        // chip carrying wide characters would otherwise under-reserve and
        // the sentence would overrun the line. Saturating, because the match
        // text is whatever an operator typed and three capped chips plus
        // their separators can still overflow a `u16`.
        let chip_columns = text.chars().map(char_columns).sum::<usize>();
        used = used.saturating_add(u16::try_from(chip_columns).unwrap_or(width));
        spans.push(Span::styled(text, ground));
        spans.push(Span::raw(" "));
        used = used.saturating_add(1);
    }
    // Three clauses, the third of which the design states and this row used
    // to omit: `esc` is the one non-obvious key in the pane, and nothing else
    // on screen says it spends a chip rather than closing.
    let sentence = format!(
        "{} of {} lines in the window: all three must hold, a line with no \
         detectable level always shows, esc drops the newest chip",
        group_thousands(survivors),
        group_thousands(total),
    );
    spans.push(Span::styled(
        fit(&sentence, width.saturating_sub(used)),
        palette.muted(),
    ));
    Some(Line::from(spans))
}

/// One chip's text per axis currently set, in field order (not
/// [`Filters`]'s own newest-last `order`: the row is a snapshot, not a
/// history).
fn chip_labels(filters: &Filters) -> Vec<String> {
    let mut chips = Vec::new();
    if let Some(stream) = filters.stream {
        let name = match stream {
            Stream::Out => "out",
            Stream::Err => "err",
        };
        chips.push(format!("stream {name}"));
    }
    if let Some(min) = filters.min_level {
        chips.push(format!("level ≥ {min}"));
    }
    if let Some(text) = filters.match_text() {
        let suffix = match filters.match_kind() {
            Some(MatchKind::Literal) | None => String::new(),
            Some(MatchKind::Regex) => " (regex)".to_string(),
            Some(MatchKind::Invalid) => " (invalid regex, matches nothing)".to_string(),
        };
        chips.push(format!("match {text}{suffix}"));
    }
    chips
}

/// `2847` as `2,847`. Grouped from the right in threes; a count in this pane
/// never carries a fractional part or a sign.
fn group_thousands(mut n: usize) -> String {
    let mut groups = Vec::new();
    loop {
        let rest = n / 1000;
        if rest == 0 {
            groups.push(n.to_string());
            break;
        }
        groups.push(format!("{:03}", n % 1000));
        n = rest;
    }
    groups.reverse();
    groups.join(",")
}

/// The pane's title: the sheep's name and both log paths, banded in
/// [`Role::Meadow`].
fn title_line(app: &App, pane: &BleatsPane, width: u16) -> Line<'static> {
    let text = match sheep_id(pane).and_then(|id| app.row(id)) {
        Some(row) => {
            let feed = app.feed();
            // The window that was read, and what fell below it. `rulings.md`
            // dropped the whole-file line count, the absolute line numbers
            // and the density gutter, all three of which need reading the
            // file to say anything. Bytes below the window are not that:
            // `tail.rs` already knows them exactly because it seeked past
            // them, and the four-line corner strip this pane replaces
            // already surfaces the same figures.
            let window = format!(
                "{} window, {} read",
                human_bytes(FEED_WINDOW_BYTES),
                human_bytes(feed.read_bytes),
            );
            let below = match feed.missed_bytes {
                0 => String::new(),
                bytes => format!("  {} below the window, unread", human_bytes(bytes)),
            };
            format!(
                "{}  out {}  err {}  {window}{below}",
                row.info.name,
                row.info.out_file.as_deref().unwrap_or("-"),
                row.info.err_file.as_deref().unwrap_or("-"),
            )
        }
        None => match sheep_id(pane) {
            Some(id) => format!("sheep {id}: it is no longer in the flock"),
            None => "no sheep is selected".to_string(),
        },
    };
    Line::from(Span::styled(
        cell::band(&text, usize::from(width)),
        app.palette().band(Role::Meadow),
    ))
}

/// The sheep id a [`BleatsPane`] describes, or `None` for the two `RowKey`
/// variants it is never opened on. `ask_for_bleats` only ever builds one on
/// a [`RowKey::Sheep`]; this stays exhaustive rather than assuming that
/// holds forever.
fn sheep_id(pane: &BleatsPane) -> Option<u32> {
    match pane.sheep() {
        RowKey::Sheep(id) => Some(*id),
        RowKey::Group(_) | RowKey::Section(_) | RowKey::Fold(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use super::super::super::app::{KeyPress, Msg};
    use super::super::super::frames::render_text;
    use super::super::super::level::Level;
    use super::super::super::pane_bleats::{MatchKind, compiles};
    use super::super::super::tail::{Stream, Tail, TailLine};
    use super::super::fixtures::{
        bleats_pane_with_filters, full_app, render_all, rendered, with_feed, with_no_selection,
        with_selection,
    };
    use super::*;

    /// Draws `pane` over `app` at `width`x`height` and renders it back to
    /// plain text, styles dropped: the same round trip `frames.rs`'s own
    /// tests use.
    fn drawn(app: &App, pane: &BleatsPane, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        draw(app, pane, area, &mut buffer);
        render_text(&buffer)
    }

    /// Mirrors `view::bleats::tests::an_empty_feed_prints_the_reason_rather_than_nothing`:
    /// the full-screen pane owes the operator the same explanation the
    /// five-line strip already gives.
    #[test]
    fn an_empty_feed_shows_the_note_in_the_body() {
        let app = with_feed(Tail {
            lines: Vec::new(),
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 0,
            note: Some("no log path is configured for this sheep".to_string()),
        });
        let pane = BleatsPane::new(app.selected().expect("with_feed selects a sheep"));
        let text = drawn(&app, &pane, 80, 6);
        assert!(
            text.contains("no log path is configured for this sheep"),
            "got {text:?}"
        );
    }

    /// The note names why there is nothing to show; once there is something,
    /// it has to go, not sit above the lines.
    #[test]
    fn a_feed_with_lines_never_shows_the_note() {
        let app = with_feed(Tail {
            lines: vec![TailLine {
                stream: Stream::Out,
                text: "listening".to_string(),
            }],
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 9,
            note: Some("should never reach the screen".to_string()),
        });
        let pane = BleatsPane::new(app.selected().expect("with_feed selects a sheep"));
        let text = drawn(&app, &pane, 80, 6);
        assert!(
            !text.contains("should never reach the screen"),
            "got {text:?}"
        );
        assert!(text.contains("listening"), "got {text:?}");
    }

    /// The ordinary case: a sheep still in the flock names itself and both
    /// its log paths.
    #[test]
    fn the_title_names_the_sheep_and_both_log_paths() {
        let info = ProcessInfo::builder(3, "catcher", ProcStatus::Online)
            .out_file(Some("/home/ada/.shep/logs/catcher-out.log".to_string()))
            .err_file(Some("/home/ada/.shep/logs/catcher-err.log".to_string()))
            .build();
        let app = with_selection(info);
        let pane = BleatsPane::new(RowKey::Sheep(3));
        let text = title_line(&app, &pane, 120);
        let rendered: String = text
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(rendered.contains("catcher"), "got {rendered:?}");
        assert!(
            rendered.contains("/home/ada/.shep/logs/catcher-out.log"),
            "got {rendered:?}"
        );
        assert!(
            rendered.contains("/home/ada/.shep/logs/catcher-err.log"),
            "got {rendered:?}"
        );
    }

    /// A pane pinned to a sheep that has since left the flock says so,
    /// rather than drawing a title with nothing behind it.
    #[test]
    fn the_title_says_when_the_pinned_sheep_left_the_flock() {
        let info = ProcessInfo::builder(3, "catcher", ProcStatus::Online).build();
        let app = with_selection(info);
        let pane = BleatsPane::new(RowKey::Sheep(404));
        let text = title_line(&app, &pane, 120);
        let rendered: String = text
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            rendered.contains("sheep 404: it is no longer in the flock"),
            "got {rendered:?}"
        );
    }

    /// A group row can never own this pane ([`ask_for_bleats`] refuses one),
    /// but `sheep_id` stays exhaustive, and the title has to say something
    /// sane if it is ever handed one anyway.
    #[test]
    fn the_title_says_no_sheep_is_selected_for_a_non_sheep_row() {
        let app = with_no_selection();
        let pane = BleatsPane::new(RowKey::Group("web".to_string()));
        let text = title_line(&app, &pane, 120);
        let rendered: String = text
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            rendered.contains("no sheep is selected"),
            "got {rendered:?}"
        );
    }

    /// Ten lines over a three-row window: the newest three land on screen,
    /// oldest first, so the newest sits on the bottom row.
    #[test]
    fn draw_keeps_the_newest_lines_when_more_exist_than_fit() {
        let app = full_app();
        let pane = BleatsPane::new(app.selected().expect("full_app selects a sheep"));
        // height 4: one title row plus three body rows.
        let text = drawn(&app, &pane, 80, 4);
        for n in 7..10 {
            assert!(text.contains(&format!("line-{n}")), "got {text:?}");
        }
        for n in 0..7 {
            assert!(!text.contains(&format!("line-{n} ")), "got {text:?}");
        }
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines.last().is_some_and(|line| line.contains("line-9")),
            "the newest line is on the bottom row: {text:?}"
        );
    }

    /// No axis set, no filter row at all: `draw_keeps_the_newest_lines_...`
    /// above depends on this, since it counts on three whole body rows.
    #[test]
    fn no_axis_set_draws_no_filter_row() {
        let mut app = full_app();
        app.update(Msg::Key(KeyPress::Bleats));
        let text = render_all(&draw_lines(&app, 80, 6));
        assert!(!text.contains("must hold"), "got {text:?}");
        assert!(!text.contains("lines in the window"), "got {text:?}");
    }

    /// The row states the composition rule, because three chips with no
    /// stated relationship read as alternatives.
    #[test]
    fn the_filter_row_says_the_axes_compose_with_and() {
        let app = bleats_pane_with_filters();
        let text = render_all(&draw_lines(&app, 160, 40));
        assert!(text.contains("all three must hold"), "got {text}");
    }

    /// Scoped to the window, because nothing counted the whole file.
    #[test]
    fn the_survivor_count_is_scoped_to_the_window() {
        let app = bleats_pane_with_filters();
        let text = render_all(&draw_lines(&app, 160, 40));
        assert!(
            text.contains("lines in the window"),
            "the count must not claim a whole-file total: {text}"
        );
    }

    /// One survivor out of four lines in the window: pins the actual
    /// numbers, not just the presence of the sentence.
    #[test]
    fn the_survivor_count_matches_what_visible_reports() {
        let app = bleats_pane_with_filters();
        let text = render_all(&draw_lines(&app, 160, 40));
        assert!(text.contains("1 of 4 lines in the window"), "got {text}");
    }

    /// Only a set axis gets a chip: with just `stream` set, `level` and
    /// `match` never appear on the row.
    #[test]
    fn only_the_set_axes_get_a_chip() {
        let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        app.update(Msg::Key(KeyPress::Bleats));
        app.bleats_pane_mut_for_tests()
            .expect("the key above opened the pane")
            .set_stream(Some(Stream::Err));
        let text = render_all(&draw_lines(&app, 160, 40));
        assert!(text.contains("stream err"), "got {text}");
        // The row's own composition sentence always says "level" and
        // "match" (an unclassifiable line's level, and the AND rule), so
        // this checks for the chips specifically rather than the bare
        // words.
        assert!(!text.contains("level ≥"), "got {text}");
        assert!(!text.contains("match "), "got {text}");
    }

    /// Each chip's literal text, exactly as the design draws it:
    /// `stream err`, `level \u{2265} warn`, `match \u{2026}`. Pinned to the
    /// character because nothing else asserts it and the spacing already
    /// drifted once: the level chip shipped as `level \u{2265}warn` against a
    /// design reading `level \u{2265} warn`, and every test passed.
    #[test]
    fn each_chip_reads_the_way_the_design_draws_it() {
        let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        app.update(Msg::Key(KeyPress::Bleats));
        let pane = app
            .bleats_pane_mut_for_tests()
            .expect("the key above opened the pane");
        pane.set_stream(Some(Stream::Err));
        pane.set_min_level(Some(Level::Warn));
        pane.set_match("pool".to_string());
        let text = render_all(&draw_lines(&app, 160, 40));
        assert!(text.contains("stream err"), "got {text}");
        assert!(text.contains("level \u{2265} warn"), "got {text}");
        assert!(text.contains("match pool"), "got {text}");
    }

    /// A `/…/`-delimited matcher's chip says it is a regex.
    #[test]
    fn a_regex_matcher_chip_names_itself_a_regex() {
        let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        app.update(Msg::Key(KeyPress::Bleats));
        app.bleats_pane_mut_for_tests()
            .expect("the key above opened the pane")
            .set_match("/po+l/".to_string());
        let text = render_all(&draw_lines(&app, 160, 40));
        assert!(text.contains("match /po+l/ (regex)"), "got {text}");
    }

    /// A pattern that fails to compile says so on the chip, rather than the
    /// feed just going quiet with no explanation.
    #[test]
    fn an_invalid_regex_chip_names_itself_invalid() {
        let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        app.update(Msg::Key(KeyPress::Bleats));
        app.bleats_pane_mut_for_tests()
            .expect("the key above opened the pane")
            .set_match("/pool(/".to_string());
        assert_eq!(
            app.bleats_pane().unwrap().filters().match_kind(),
            Some(MatchKind::Invalid)
        );
        let text = render_all(&draw_lines(&app, 160, 40));
        assert!(text.contains("invalid regex"), "got {text}");
    }

    /// The matched text is its own span, styled apart from the rest of the
    /// line. A test asserting only that the line's text renders (as every
    /// other test in this module does) cannot tell a highlighted match from
    /// a plain one: this reads the spans themselves instead.
    #[test]
    fn a_match_axis_highlights_its_hit_and_only_its_hit() {
        let app = bleats_pane_with_filters();
        let lines = draw_lines(&app, 160, 40);
        // Not `.contains("pool")`: the filter row's own `match pool` chip
        // contains that substring too, and sits before the feed in `lines`.
        // `"exhausted"` only ever appears in the surviving log line itself.
        let survivor = lines
            .iter()
            .find(|line| rendered(line).contains("exhausted"))
            .expect("the one surviving line contains \"exhausted\"");
        let hit = survivor
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "pool")
            .expect("the matched text is its own span, not merged into a longer one");
        assert_ne!(
            hit.style,
            Style::default(),
            "the match must carry a style distinct from plain text"
        );
        let plain = survivor
            .spans
            .iter()
            .find(|span| span.style == Style::default() && span.content.as_ref() != "pool");
        assert!(
            plain.is_some(),
            "the rest of the line must stay unstyled, for contrast: {survivor:?}"
        );
    }

    /// `group_thousands` groups, pads and orders.
    ///
    /// Every survivor count in every other test is under a thousand, so the
    /// separator branch never ran: dropping the `{:03}` pad passed, and so
    /// did dropping the `reverse()`. The spec's own example is `2,847`, which
    /// is exactly the untested path.
    #[test]
    fn group_thousands_pads_and_orders_its_groups() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(2_847), "2,847");
        // The pad: without `{:03}` this is `1,7`.
        assert_eq!(group_thousands(1_007), "1,007");
        // The order: reversed, this is `847,12`.
        assert_eq!(group_thousands(12_847), "12,847");
        assert_eq!(group_thousands(1_000_000), "1,000,000");
    }

    /// The filter row states all three clauses the design gives it.
    ///
    /// The third was missing: `esc` is the one non-obvious key in this pane,
    /// and with it absent nothing on screen said the key spends a chip
    /// rather than closing. The status bar says `esc back`, which is the
    /// design's own wording and true of a pane with no chips set.
    #[test]
    fn the_filter_row_states_all_three_of_its_clauses() {
        let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        app.update(Msg::Key(KeyPress::Bleats));
        app.bleats_pane_mut_for_tests()
            .expect("open")
            .set_min_level(Some(Level::Warn));
        let text = render_all(&draw_lines(&app, 200, 40));
        assert!(text.contains("all three must hold"), "got {text}");
        assert!(
            text.contains("no detectable level always shows"),
            "got {text}"
        );
        assert!(text.contains("esc drops the newest chip"), "got {text}");
    }

    /// The title names the window it read and what fell below it.
    ///
    /// `rulings.md` dropped the whole-file line count, the absolute line
    /// numbers and the density gutter, all three of which need reading the
    /// file. Bytes below the window are not in that set: `tail.rs` knows
    /// them exactly because it seeked past them, and the design asks row 1
    /// for them.
    #[test]
    fn the_title_names_the_window_and_what_fell_below_it() {
        let mut app = with_selection(
            ProcessInfo::builder(9, "web", ProcStatus::Online)
                .out_file(Some("/logs/web-out.log".to_string()))
                .build(),
        );
        app.update(Msg::Bleats {
            tail: Tail {
                lines: vec![TailLine {
                    stream: Stream::Out,
                    text: "listening".to_string(),
                }],
                missed_lines: 0,
                missed_bytes: 831 * 1024,
                read_bytes: 4096,
                note: None,
            },
        });
        app.update(Msg::Key(KeyPress::Bleats));
        let text = render_all(&draw_lines(&app, 200, 40));
        assert!(text.contains("64.0K window"), "the window it reads: {text}");
        assert!(text.contains("4.0K read"), "and what it read: {text}");
        assert!(
            text.contains("below the window, unread"),
            "and what it could not: {text}"
        );
    }

    /// `row_height` counts display columns, and `wrap_spans` has to agree
    /// with it or the page arithmetic drifts from what is drawn.
    ///
    /// `row_height` feeds every page and window calculation while
    /// `wrap_spans` produces the rows themselves, and `row_height`'s own doc
    /// says the two "can never disagree on how many rows one line takes".
    /// Only `wrap_spans` was held there: mutating `char_columns` to `1`
    /// inside `row_height` passed the whole suite, because every test that
    /// counted rows counted rendered ones.
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

    /// Neither page direction ever steps over a line, across every cost
    /// profile a wrapped feed can produce.
    ///
    /// Over the arithmetic rather than through a fixture, because a fixture
    /// is the wrong instrument here. A feed of uniform row heights cannot
    /// show this bug at all: a backward count and a window's own length are
    /// the same number when every line is the same height, so the two
    /// directions agree and a shared page size looks correct. Three separate
    /// fixtures failed to reproduce a defect that a stress test finds in
    /// roughly one step in six.
    ///
    /// The invariant: after a page, the next window must start no later than
    /// one past where the last one ended, going forward, and end no earlier
    /// than one before where it began, going back. Anything else is a line
    /// no screen drew.
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

    /// A redraw compiles the match axis's pattern no times at all.
    ///
    /// `highlighted` runs once per rendered line, so this used to be one
    /// `Regex::new` per line per frame. Measured over this feed at 160x50,
    /// `/\w+@\w+\.\w+/` cost 27.0 ms a redraw that way, against
    /// `lookout::MIN_REDRAW`'s 33 ms frame budget; it is 24.8 us now. The
    /// count is what pins it: a timing assertion would be flaky, and a test
    /// asserting only that the highlight renders passes just as well over a
    /// version that recompiles every line.
    #[test]
    fn a_redraw_compiles_the_match_axis_no_times() {
        let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        app.update(Msg::Bleats {
            tail: Tail {
                lines: (0..80)
                    .map(|i| TailLine {
                        stream: if i % 2 == 0 { Stream::Out } else { Stream::Err },
                        text: format!("worker[{i}] pool acquire ok user=ops{i}@example.com"),
                    })
                    .collect(),
                missed_lines: 0,
                missed_bytes: 0,
                read_bytes: 65_536,
                note: None,
            },
        });
        app.update(Msg::Key(KeyPress::Bleats));
        app.bleats_pane_mut_for_tests()
            .expect("the key above opened the pane")
            .set_match(r"/\w+@\w+\.\w+/".to_string());

        let before = compiles();
        let drawn = draw_lines(&app, 160, 50);
        assert_eq!(compiles(), before, "a redraw compiles nothing");

        // Without these the count above would hold over a pane drawing no
        // lines at all, which is how two of the first measurements of this
        // looked cheap: a matcher that survives no line is never run. Body
        // rows are picked out by their own text rather than by skipping a
        // header count, which would quietly review the wrong rows if the
        // pane ever grew another header. The span count is the second half:
        // a row whose match was highlighted is split into more spans than
        // the stream tag plus one run of plain text.
        let body: Vec<_> = drawn
            .iter()
            .filter(|line| render_all(std::slice::from_ref(*line)).contains("@example.com"))
            .collect();
        assert_eq!(body.len(), 48, "the feed filled every body row");
        assert!(
            body.iter().all(|line| line.spans.len() > 2),
            "every body row split its hit into its own span"
        );
    }
}
