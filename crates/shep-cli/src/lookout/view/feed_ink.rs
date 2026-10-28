//! The ink one feed line's text wears, in the two places the feed is drawn.
//!
//! A line carries two colours and they answer to different owners. The
//! process's own runs are its styling, kept as it wrote them. The stream
//! default is shep's, and it is spent only on a line that asked for nothing:
//! stdout is meadow and stderr is bark, the way `shep bleats` paints them.
//!
//! Both choices come out of [`Palette`], so `NO_COLOR` costs them the same
//! way it costs every other colour.

use ratatui::style::Style;
use ratatui::text::Span;

use super::super::tail::{Stream, TailLine};
use super::super::theme::Palette;
use super::flock::take_columns;
use crate::output::width::text_columns;
use crate::vocabulary::Role;

/// The ink a feed line's text is drawn through: the palette its stream colour
/// resolves against, and the filter highlight that wins over that colour on the
/// byte ranges it names.
#[derive(Clone, Copy)]
pub(crate) struct Ink<'a> {
    palette: Palette,
    highlight: &'a [(usize, usize)],
    highlight_style: Style,
}

impl<'a> Ink<'a> {
    /// Stream colour and nothing above it: the dashboard strip has no filter
    /// row to draw.
    #[must_use]
    pub(crate) fn plain(palette: Palette) -> Self {
        Self {
            palette,
            highlight: &[],
            highlight_style: Style::default(),
        }
    }

    /// Stream colour under `highlight`, which wins on the ranges it names.
    ///
    /// The ranges come from
    /// [`super::super::pane_bleats::Filters::match_ranges`], which reports them
    /// against the same `TailLine::text` the process's own runs cover, so the
    /// two need no translating to line up.
    #[must_use]
    pub(crate) fn filtered(
        palette: Palette,
        highlight: &'a [(usize, usize)],
        style: Style,
    ) -> Self {
        Self {
            palette,
            highlight,
            highlight_style: style,
        }
    }

    /// A process-owned run's style, as this ink draws it.
    ///
    /// Live, that is exactly what the sheep asked for. Under a frozen data
    /// palette it is the run's own modifiers over the one muted ink: a frozen
    /// screen renders no reading as live. What the sheep said survives the
    /// freeze, because bold, underline and reverse video are not colour.
    fn run_style(self, run: Style) -> Style {
        if !self.palette.is_frozen() {
            return run;
        }
        let mut collapsed = self.palette.muted();
        collapsed.add_modifier = run.add_modifier;
        collapsed
    }
}

/// One feed line's text, as spans: the process's own runs where it set a
/// colour, the stream default where it did not, and the highlight over both on
/// its own ranges.
#[must_use]
pub(crate) fn text_spans(line: &TailLine, ink: Ink<'_>) -> Vec<Span<'static>> {
    let base = if line.own_colour {
        Style::default()
    } else {
        ink.palette.ink(match line.stream {
            Stream::Out => Role::Meadow,
            Stream::Err => Role::Bark,
        })
    };

    let mut cuts = vec![0usize, line.text.len()];
    for run in &line.runs {
        cuts.extend([run.range.start, run.range.end]);
    }
    for &(start, end) in ink.highlight {
        cuts.extend([start.min(line.text.len()), end.min(line.text.len())]);
    }
    cuts.sort_unstable();
    cuts.dedup();

    cuts.windows(2)
        .filter(|pair| pair[0] < pair[1])
        .map(|pair| {
            let (start, end) = (pair[0], pair[1]);
            let style = if covered(ink.highlight, start, end) {
                ink.highlight_style
            } else {
                line.runs
                    .iter()
                    .find(|run| run.range.start <= start && end <= run.range.end)
                    .map_or(base, |run| ink.run_style(run.style))
            };
            Span::styled(line.text[start..end].to_string(), style)
        })
        .collect()
}

/// [`text_spans`] cut to `budget` display columns, the way [`super::flock::fit`]
/// cuts a plain string: padded to `budget` when it fits, and otherwise one
/// column short of `budget` plus a `…` wearing the style of the run it cut
/// into.
#[must_use]
pub(crate) fn fitted_spans(line: &TailLine, ink: Ink<'_>, budget: u16) -> Vec<Span<'static>> {
    let width = usize::from(budget);
    let spans = text_spans(line, ink);
    let used: usize = spans
        .iter()
        .map(|span| text_columns(span.content.as_ref()))
        .sum();
    if used <= width {
        let mut fitted = spans;
        fitted.push(Span::raw(" ".repeat(width - used)));
        return fitted;
    }
    if width == 0 {
        return Vec::new();
    }

    let mut fitted: Vec<Span<'static>> = Vec::new();
    let mut taken = 0usize;
    let mut last = Style::default();
    for span in spans {
        if taken >= width - 1 {
            break;
        }
        let (kept, columns) = take_columns(span.content.as_ref(), width - 1 - taken);
        taken += columns;
        if !kept.is_empty() {
            last = span.style;
            fitted.push(Span::styled(kept, span.style));
        }
        // A span this cut short is where the line ends. A double-width
        // character that would straddle the budget is dropped whole and leaves
        // a column spare, and filling that column from the *next* span would
        // show text that comes after the cut.
        if columns < text_columns(span.content.as_ref()) {
            break;
        }
    }
    fitted.push(Span::styled("…", last));
    if taken < width - 1 {
        fitted.push(Span::raw(" ".repeat(width - 1 - taken)));
    }
    fitted
}

/// Whether `[start..end)` sits whole inside one of `ranges`. Every range and
/// every span boundary is a cut point in [`text_spans`], so a range covers a
/// span whole or not at all and no span is split twice.
fn covered(ranges: &[(usize, usize)], start: usize, end: usize) -> bool {
    ranges
        .iter()
        .any(|&(range_start, range_end)| range_start <= start && end <= range_end)
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};

    use super::super::super::tail::Stream;
    use super::super::fixtures::{coloured, no_color};
    use super::super::flock::fit;
    use super::*;

    fn text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    #[test]
    fn a_plain_stdout_line_is_meadow_and_a_plain_stderr_line_is_bark() {
        let palette = coloured();
        let out = text_spans(
            &TailLine::new(Stream::Out, "listening on :8080"),
            Ink::plain(palette),
        );
        let err = text_spans(
            &TailLine::new(Stream::Err, "warn: retrying upstream"),
            Ink::plain(palette),
        );

        assert_eq!(text(&out), "listening on :8080");
        assert_eq!(out.len(), 1, "a plain line is one run");
        assert_eq!(out[0].style, palette.ink(Role::Meadow));
        assert_eq!(err[0].style, palette.ink(Role::Bark));
    }

    #[test]
    fn a_line_that_set_its_own_colour_is_not_repainted() {
        let palette = coloured();
        let line = TailLine::new(Stream::Err, "\u{1b}[35mviolet\u{1b}[0m plain");
        let spans = text_spans(&line, Ink::plain(palette));

        assert_eq!(text(&spans), "violet plain");
        assert_eq!(spans[0].style, Style::default().fg(Color::Magenta));
        assert_eq!(
            spans[1].style,
            Style::default(),
            "the stretch the sheep left unstyled stays unstyled, not bark"
        );

        let reset_only = TailLine::new(Stream::Err, "\u{1b}[39mwarn\u{1b}[49m");
        let spans = text_spans(&reset_only, Ink::plain(palette));
        assert_eq!(spans.len(), 1, "a colour reset leaves no run behind");
        assert_eq!(
            spans[0].style,
            palette.ink(Role::Bark),
            "and the line still wears its stream's colour"
        );
    }

    #[test]
    fn a_filter_highlight_wins_on_its_range_and_leaves_the_runs_either_side() {
        let palette = coloured();
        let line = TailLine::new(Stream::Out, "\u{1b}[31mred pool red\u{1b}[0m plain");
        let at = line
            .text
            .find("pool")
            .expect("the fixture line has the needle");
        let ranges = [(at, at + "pool".len())];
        let spans = text_spans(
            &line,
            Ink::filtered(palette, &ranges, palette.band(Role::Butter)),
        );

        assert_eq!(text(&spans), "red pool red plain");
        assert_eq!(spans[1].content, "pool");
        assert_eq!(spans[1].style, palette.band(Role::Butter));
        assert_eq!(spans[0].style, Style::default().fg(Color::Red));
        assert_eq!(spans[2].style, Style::default().fg(Color::Red));
        assert_eq!(spans[3].style, Style::default());
    }

    /// `NO_COLOR` is the operator asking for no colour, and a frozen dashboard
    /// is a screen that must not look live: both reach the feed through
    /// [`Palette`], and neither is a special case this module knows about.
    #[test]
    fn no_colour_adds_nothing_under_a_frozen_palette() {
        let line = TailLine::new(Stream::Err, "warn: retrying upstream");
        let spans = text_spans(&line, Ink::plain(no_color()));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style, Style::default(), "NO_COLOR adds nothing");

        let frozen = coloured().frozen();
        let spans = text_spans(&line, Ink::plain(frozen));
        assert_eq!(
            spans[0].style,
            frozen.muted(),
            "a frozen feed wears the muted ink, not bark"
        );

        // The feed stops re-reading the moment the link is lost, so a sheep's
        // own colour on that screen is a reading too, and a red two seconds
        // after the shepherd died is the lie `Palette::frozen` exists to stop.
        // What the sheep said survives: bold is not a colour.
        let styled = TailLine::new(Stream::Out, "\u{1b}[1;31mred bold\u{1b}[0m");
        let spans = text_spans(&styled, Ink::plain(frozen));
        assert_eq!(
            spans[0].style.fg,
            frozen.muted().fg,
            "a frozen feed collapses the sheep's own colour too"
        );
        assert_eq!(
            spans[0].style.add_modifier,
            Modifier::BOLD,
            "and keeps what the sheep said rather than what it wore"
        );

        let live = TailLine::new(Stream::Out, "\u{1b}[1;31mred bold\u{1b}[0m");
        let spans = text_spans(&live, Ink::plain(coloured()));
        assert_eq!(
            spans[0].style,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            "a live feed paints exactly what the sheep asked for"
        );
    }

    /// A line too wide for the strip is cut the way [`fit`] cuts it, and the
    /// `…` belongs to the run it landed in rather than to the line's default.
    #[test]
    fn a_fitted_line_keeps_the_colour_of_the_run_it_was_cut_inside() {
        let palette = coloured();
        let line = TailLine::new(Stream::Out, "\u{1b}[31mred stretch\u{1b}[0m plain");
        let fitted = fitted_spans(&line, Ink::plain(palette), 8);

        assert_eq!(text(&fitted), "red str…");
        assert_eq!(fitted[0].style, Style::default().fg(Color::Red));
        assert_eq!(fitted[1].style, Style::default().fg(Color::Red));

        // A line that fits is padded to the budget, the way `fit` pads a cell.
        let padded = fitted_spans(&TailLine::new(Stream::Out, "ok"), Ink::plain(palette), 6);
        assert_eq!(text(&padded), "ok    ");
        assert_eq!(padded[0].style, palette.ink(Role::Meadow));
        assert_eq!(fit("ok", 6), text(&padded), "and it matches `fit`");
    }

    /// A double-width character that would straddle the budget is dropped whole
    /// and leaves a column spare. Filling that column from the next span would
    /// show text that comes after the cut.
    #[test]
    fn a_cut_span_ends_the_fitted_line_and_hides_what_comes_after_it() {
        let line = TailLine::new(Stream::Out, "\u{1b}[31mabc界\u{1b}[0m z");
        let fitted = fitted_spans(&line, Ink::plain(coloured()), 5);
        let drawn = text(&fitted);

        assert!(drawn.starts_with("abc…"), "got {drawn:?}");
        assert!(!drawn.contains('z'), "nothing after the cut shows");
        assert_eq!(
            fitted[1].style,
            Style::default().fg(Color::Red),
            "the ellipsis wears the run it cut into"
        );
    }
}
