//! The link panel: what the dashboard tried, when it stopped, and what is
//! left to do about it.
//!
//! Drawn only once the link is [`Link::Lost`], where it takes the rows the
//! detail band and the bleats feed had. Those two are readings the shepherd
//! gave, and a dead shepherd's readings are history; this is the one region
//! of a frozen dashboard still describing the present.

use core::time::Duration;

use ratatui::text::{Line, Span};

use super::super::app::{App, Link};
use super::super::link::{RECONNECT_ATTEMPTS, RECONNECT_FIRST_WAIT, RECONNECT_MAX_WAIT};
use super::detail::chip_text;
use super::flock::fit;
use crate::output::human_duration;
use crate::output::width::char_columns;
use crate::vocabulary::Role;

/// The panel's rows: one rule and five lines.
///
/// Six against the feed's seven, so the frozen bottom stack is never taller
/// than the live one it replaces and no height tier has to move.
pub const LINK_ROWS: u16 = 6;

/// The narrowest terminal the dashboard draws into at all
/// ([`super::MIN_TERM_WIDTH`]), repeated here so the panel's own width
/// sweep starts where the pane's does.
#[cfg(test)]
const MIN_PANEL_WIDTH: u16 = super::MIN_TERM_WIDTH;

/// The label gutter, in cells: the width of [`chip_text`]'s own
/// `" \u{2588}\u{2588} THE LINK"`.
///
/// One space, two blocks, one space, eight letters. The three label rows pad
/// to it so their values start in the same column the chip's sentence does.
const LABEL_WIDTH: usize = 12;

/// The four lines under the panel's rule.
///
/// Empty when the link is not [`Link::Lost`]. `view::draw` claims these rows
/// only on a frozen frame, so that case does not arise; it draws blank rows
/// rather than panicking if it ever does.
#[must_use]
pub fn panel_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let Link::Lost { at_local, why } = app.link() else {
        return Vec::new();
    };
    let palette = app.palette();
    let rungs = ladder();
    let total: Duration = rungs.iter().sum();

    let chip = chip_text("THE LINK");
    let head = format!(
        "  {} redials over {}, all refused   ·   lookout has stopped polling and stopped re-dialling",
        rungs.len(),
        seconds(total),
    );

    let mut ladder_spans = vec![Span::styled(label("ladder"), palette.muted())];
    for rung in &rungs {
        ladder_spans.push(Span::styled("\u{2588}", palette.alarm()));
        ladder_spans.push(Span::styled(
            format!(" {}  ", rung_label(*rung)),
            palette.muted(),
        ));
    }

    let since = format!(
        "{at_local}, {} ago",
        human_duration(millis(app.frozen_for())),
    );

    vec![
        Line::from(vec![
            Span::styled(chip, palette.band(Role::Bark)),
            Span::styled(head, palette.muted()),
        ]),
        Line::from(ladder_spans),
        // Its own row, not the ladder's tail. The design drew a
        // forty-character paraphrase there; the real sentence is a hundred
        // and twenty, since `LinkError::Unreachable` names the socket it
        // dialled as well as what the OS said, and both halves earn their
        // cells. Sharing the ladder's row would truncate the reason and
        // keep the path, which is the wrong half to lose.
        Line::from(vec![
            Span::styled(label("refused"), palette.muted()),
            Span::styled(why.clone(), palette.muted()),
        ]),
        Line::from(vec![
            Span::styled(label("since"), palette.muted()),
            Span::styled(
                format!(
                    "{since}      the sheep themselves may well still be running; lookout cannot see them to say"
                ),
                palette.muted(),
            ),
        ]),
        // Not the design's `r dials again now`. `r` is refused once the
        // link is lost (`App::on_key`'s `KeyPress::Refresh` arm), and
        // nothing is left to answer a redial anyway: `run_link` returns
        // after sending `Msg::Frozen`, taking the poll channel's receiver
        // with it. A row promising a key that does nothing is worse than
        // no row.
        Line::from(vec![
            Span::styled(label("try"), palette.attention()),
            Span::styled(
                "shep muster, from another shell      then reopen lookout: it does not reconnect on its own"
                    .to_string(),
                palette.muted(),
            ),
        ]),
    ]
    .into_iter()
    .map(|line| clamp(line, width))
    .collect()
}

/// `"  <name>"` padded to [`LABEL_WIDTH`].
fn label(name: &str) -> String {
    let text = format!("  {name}");
    let used: usize = text.chars().map(char_columns).sum();
    format!("{text}{}", " ".repeat(LABEL_WIDTH.saturating_sub(used)))
}

/// Every rung the ladder climbs, in the order `super::super::link::run_link`
/// climbs them.
///
/// Derived rather than written out, so moving a constant moves the panel
/// with it: the doubling and its ceiling are that function's own.
fn ladder() -> Vec<Duration> {
    let mut wait = RECONNECT_FIRST_WAIT;
    let mut out = Vec::new();
    for _ in 0..RECONNECT_ATTEMPTS {
        out.push(wait);
        wait = (wait * 2).min(RECONNECT_MAX_WAIT);
    }
    out
}

/// One rung: `250ms` under a second, `4s` at or over one.
///
/// Every value the ladder produces is a whole number of milliseconds or of
/// seconds, since it starts at a whole number of milliseconds and doubles.
fn rung_label(wait: Duration) -> String {
    let ms = millis(wait);
    if ms < 1_000 {
        format!("{ms}ms")
    } else {
        format!("{}s", ms / 1_000)
    }
}

/// The ladder's total, to two decimals with trailing zeros trimmed: `7.75s`.
fn seconds(total: Duration) -> String {
    let text = format!("{:.2}", total.as_secs_f64());
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    format!("{trimmed}s")
}

/// `Duration` as whole milliseconds, saturating rather than wrapping on a
/// span no dashboard will ever reach.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// `line` truncated to `width` columns.
///
/// Applied to every row: the panel's sentences are long by design, and a
/// terminal narrower than the design target must drop their tails rather
/// than have `Buffer::set_line` cut them mid-span in silence.
fn clamp(line: Line<'static>, width: u16) -> Line<'static> {
    let mut budget = usize::from(width);
    let mut spans = Vec::with_capacity(line.spans.len());
    for span in line.spans {
        if budget == 0 {
            break;
        }
        let used: usize = span.content.chars().map(char_columns).sum();
        if used <= budget {
            budget -= used;
            spans.push(span);
        } else {
            let cut = fit(&span.content, u16::try_from(budget).unwrap_or(u16::MAX));
            budget = 0;
            spans.push(Span::styled(cut, span.style));
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::super::super::app::Msg;
    use super::super::fixtures;
    use super::*;
    use crate::output::width::visible_width;

    fn frozen() -> App {
        let mut app = fixtures::full_app();
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        app
    }

    fn text(app: &App, width: u16) -> Vec<String> {
        panel_lines(app, width)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// The panel claims to say what the ladder did. Written out rather than
    /// derived a second way, so moving a constant without meaning to fails
    /// here instead of shipping a panel that describes a ladder shep no
    /// longer climbs.
    #[test]
    fn the_ladder_reads_off_the_reconnect_constants() {
        assert_eq!(RECONNECT_ATTEMPTS, 5);
        assert_eq!(RECONNECT_FIRST_WAIT, Duration::from_millis(250));
        assert_eq!(RECONNECT_MAX_WAIT, Duration::from_secs(4));

        let rungs = ladder();
        assert_eq!(
            rungs,
            vec![
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ]
        );
        assert_eq!(seconds(rungs.iter().sum()), "7.75s");

        let lines = text(&frozen(), 160);
        assert!(
            lines[0].contains("5 redials over 7.75s, all refused"),
            "{lines:?}"
        );
        assert!(
            lines[1]
                .contains("\u{2588} 250ms  \u{2588} 500ms  \u{2588} 1s  \u{2588} 2s  \u{2588} 4s"),
            "{lines:?}"
        );
    }

    /// The three labels and the chip share a column, so their values line
    /// up down the panel. Measured rather than counted by eye: the chip's
    /// width is `cell::band`'s to decide, not this module's.
    #[test]
    fn every_label_starts_its_value_in_the_chip_s_own_column() {
        assert_eq!(visible_width(&chip_text("THE LINK")), LABEL_WIDTH);
        for name in ["ladder", "since", "try", "refused"] {
            assert_eq!(visible_width(&label(name)), LABEL_WIDTH, "{name}");
        }
    }

    /// Five lines, whatever the terminal, and none of them wider than it.
    #[test]
    fn no_line_outruns_the_terminal_it_is_drawn_into() {
        let app = frozen();
        for width in [MIN_PANEL_WIDTH, 40, 80, 120, 160, 200] {
            let lines = panel_lines(&app, width);
            assert_eq!(lines.len(), usize::from(LINK_ROWS - 1), "at {width}");
            for line in &lines {
                let used: usize = line
                    .spans
                    .iter()
                    .flat_map(|span| span.content.chars())
                    .map(char_columns)
                    .sum();
                assert!(used <= usize::from(width), "{used} cells at {width}");
            }
        }
    }

    /// `view::draw` claims the panel's rows only on a frozen frame. If that
    /// ever stops being true, blank rows are the failure mode rather than a
    /// panic.
    #[test]
    fn a_live_link_draws_nothing() {
        assert!(panel_lines(&fixtures::full_app(), 160).is_empty());
    }

    /// The `since` line is the one thing here that moves, and it counts
    /// from the freeze rather than from the dashboard opening.
    ///
    /// Its own clock, not `fixtures::full_app`'s: the freeze instant is
    /// whatever `App::now` held when `Msg::Frozen` landed, so a test that
    /// ticked against a fresh `Instant::now()` would be measuring how long
    /// the fixture took to build as well.
    #[test]
    fn the_age_counts_from_the_freeze_and_says_when_it_was() {
        let t0 = std::time::Instant::now();
        let mut app = App::new(
            super::super::super::theme::Palette::detect(None, None, None),
            super::super::super::app::Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let before = text(&app, 160)[3].clone();
        assert!(before.contains("2026-08-14 14:32:07, 0s ago"), "{before}");

        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(252),
        });
        let after = text(&app, 160)[3].clone();
        assert!(after.contains("2026-08-14 14:32:07, 4m 12s ago"), "{after}");
    }
}
