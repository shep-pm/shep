//! The full-screen bleats pane: the same feed [`super::bleats`] draws in its
//! five-line strip, given the whole body instead, plus the filter row
//! stacked on it once an operator has set an axis.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::app::{App, RowKey};
use super::super::pane_bleats::{BleatsPane, Filters, MatchKind};
use super::super::tail::{Stream, TailLine};
use super::cell;
use super::flock::fit;
use crate::vocabulary::Role;

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
/// `#[cfg(test)]`: this module's own tests are the only caller today.
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
    let survivors = pane.visible(&feed.lines);

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
    // The last surviving lines that fit, oldest first, so the newest one
    // lands on the bottom row: the same order `view::bleats::feed_lines`
    // renders in.
    let skip = survivors.len().saturating_sub(body_rows);
    for line in survivors.iter().skip(skip).take(body_rows) {
        out.push(feed_line(app, filters, line, width));
    }
    out
}

/// One feed line: its stream tag, then its text with every match-axis hit
/// highlighted.
fn feed_line(app: &App, filters: &Filters, line: &TailLine, width: u16) -> Line<'static> {
    let palette = app.palette();
    let tag = match line.stream {
        Stream::Out => "out",
        Stream::Err => "err",
    };
    let mut spans = vec![
        // Muted, both of them: the word carries the meaning, and a red
        // `err` would say a stderr line is damage. See
        // `view::bleats::feed_lines` for the same choice.
        Span::styled(format!("{tag}  "), palette.muted()),
    ];
    let text = fit(&line.text, width.saturating_sub(5));
    spans.extend(highlighted(&text, filters, palette.attention()));
    Line::from(spans)
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
        used += u16::try_from(text.chars().count()).unwrap_or(width);
        spans.push(Span::styled(text, ground));
        spans.push(Span::raw(" "));
        used += 1;
    }
    let sentence = format!(
        "{} of {} lines in the window — all three must hold, and a line with no \
         detectable level always shows",
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
        chips.push(format!("level ≥{}", format!("{min:?}").to_lowercase()));
    }
    if let Some(text) = &filters.matcher {
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
        Some(row) => format!(
            "{}  out {}  err {}",
            row.info.name,
            row.info.out_file.as_deref().unwrap_or("-"),
            row.info.err_file.as_deref().unwrap_or("-"),
        ),
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
        RowKey::Group(_) | RowKey::Section(_) => None,
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
    use super::super::super::pane_bleats::MatchKind;
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
}
