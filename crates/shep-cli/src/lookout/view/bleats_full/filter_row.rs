//! The bleats pane's filter row: one chip per axis an operator has set,
//! and the sentence naming how many of the window's lines survived and how
//! the axes compose.

use ratatui::text::{Line, Span};

use super::super::super::app::App;
use super::super::super::pane_bleats::{Filters, MatchKind};
use super::super::super::tail::Stream;
use super::super::flock::fit;
use crate::output::width::text_columns;

/// The row under the title once any filter axis is set: a chip per set
/// axis, then the sentence naming how many of the window's lines survived
/// and how the axes compose. `None` while every axis is clear: with nothing
/// set, a row saying so would repeat what the plain feed already shows.
pub(super) fn filter_row_line(
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
        // the sentence would overrun the line. Saturating, because three
        // capped chips plus their separators can overflow a `u16`.
        let chip_columns = text_columns(&text);
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
        let suffix = filters.match_kind().map_or("", MatchKind::chip_suffix);
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

#[cfg(test)]
mod tests {
    use ratatui::style::Style;
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use super::super::super::super::app::{KeyPress, Msg};
    use super::super::super::super::level::Level;
    use super::super::super::super::pane_bleats::compiles;
    use super::super::super::super::tail::{Stream, Tail, TailLine};
    use super::super::super::fixtures::{
        bleats_pane_with_filters, render_all, rendered, with_selection,
    };
    use super::super::draw_lines;
    use super::*;

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
        // The row's own composition sentence always says "level", so the
        // level check reads the chip's `level ≥` rather than the bare word.
        // The match chip is `match ` plus typed text and the sentence never
        // says "match": the space keeps this check on the chip.
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

    /// A literal matcher's chip is the typed text and nothing after it. The
    /// assertion above reads `match pool` out of the whole row, which a
    /// chip carrying a suffix satisfies just as well, so this one compares
    /// the chip's own span: `chip_labels` writes the text and
    /// `filter_row_line` pads it, and anything appended in either breaks
    /// the comparison.
    #[test]
    fn a_literal_matcher_chip_carries_nothing_after_the_typed_text() {
        let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        app.update(Msg::Key(KeyPress::Bleats));
        app.bleats_pane_mut_for_tests()
            .expect("the key above opened the pane")
            .set_match("pool".to_string());
        let lines = draw_lines(&app, 160, 40);
        let chips: Vec<&str> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .filter(|text| text.contains("match pool"))
            .collect();
        assert_eq!(chips, vec![" match pool "]);
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
        let rest: Vec<&Span<'static>> = survivor
            .spans
            .iter()
            .filter(|span| span.content.as_ref() != "pool")
            .collect();
        assert!(
            !rest.is_empty() && rest.iter().all(|span| span.style != hit.style),
            "the rest of the line must not wear the highlight, for contrast: {survivor:?}"
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

    /// A redraw compiles the match axis's pattern no times at all.
    ///
    /// `highlighted` runs once per rendered line, so an uncompiled pattern
    /// costs one `Regex::new` per line per frame against
    /// `lookout::MIN_REDRAW`'s 33 ms budget. The count pins it: a timing
    /// assertion is flaky.
    #[test]
    fn a_redraw_compiles_the_match_axis_no_times() {
        let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        app.update(Msg::Bleats {
            tail: Tail {
                lines: (0..80)
                    .map(|i| {
                        TailLine::new(
                            if i % 2 == 0 { Stream::Out } else { Stream::Err },
                            &format!("worker[{i}] pool acquire ok user=ops{i}@example.com"),
                        )
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
        let drawn_rows = draw_lines(&app, 160, 50);
        assert_eq!(compiles(), before, "a redraw compiles nothing");

        // Without these the count above would hold over a pane drawing no
        // lines at all: a matcher that survives no line is never run. Body
        // rows are picked out by their own text rather than by skipping a
        // header count. The span count is the other half of the assertion.
        let body: Vec<_> = drawn_rows
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
