use super::flock::MIN_HEIGHT;
use ratatui::layout::Rect;

/// Rows the chrome always takes: title, column header, rule, status bar.
///
/// The banner is not in this count: it is one row only when the link is
/// not live, and callers that need the worst case add it separately.
///
/// `#[cfg(test)]`: `draw` lays these out one `y += 1` at a time rather than
/// summing them, so this constant has no production call site.
#[cfg(test)]
pub(super) const CHROME_ROWS: u16 = 4;

/// The host strip is one line.
/// The shortest terminal that gets the design's two blank chrome rows, one
/// under the title band and one under the rule.
///
/// Not a taste threshold. `the_flock_table_keeps_the_middle_of_the_screen`
/// pins the table at five data rows on a 24-row terminal, and at that height
/// there is exactly no slack: spending two rows on air there takes the table
/// to three, which is the pane stopping being the point of the screen. Six
/// rows above that floor is where the air costs nothing that matters.
pub(super) const ROOMY_HEIGHT: u16 = 30;

pub(super) const HOST_ROWS: u16 = 1;

/// The detail pane: one rule and four lines.
pub(super) const DETAIL_ROWS: u16 = 5;

/// The bleats feed: one rule, one header, five lines.
pub(super) const FEED_ROWS: u16 = 7;

/// Which optional panes a terminal of a given height gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Panes {
    /// The host-usage strip, under the title.
    pub host: bool,
    /// The sheep detail pane, under the table.
    pub detail: bool,
    /// The bleats feed, under that.
    pub feed: bool,
}

/// Height thresholds, tallest first. Each entry is the shortest terminal that
/// still gets that pane set.
///
/// The drop order is least-diagnostic first. 24 is the classic terminal
/// height, chosen so a plain 80x24 gets all three panes with a flock table
/// worth reading.
pub(super) const PANE_TIERS: &[(u16, Panes)] = &[
    (
        24,
        Panes {
            host: true,
            detail: true,
            feed: true,
        },
    ),
    (
        18,
        Panes {
            host: true,
            detail: false,
            feed: true,
        },
    ),
    (
        14,
        Panes {
            host: true,
            detail: false,
            feed: false,
        },
    ),
    (MIN_HEIGHT, Panes::NONE),
];

/// The widest pane set that fits `height`.
#[must_use]
pub fn panes_for(height: u16) -> Panes {
    PANE_TIERS
        .iter()
        .find(|(threshold, _)| height >= *threshold)
        .map_or(Panes::NONE, |(_, panes)| *panes)
}

/// Renders the whole dashboard.
///
/// Synchronous, and every branch draws something except a zero-area frame,
/// which returns without drawing. A degenerate case draws a sentence rather
/// than nothing, since a blank pane cannot say whether the shepherd has
/// nothing to run or the dashboard is broken.
///
/// How tall the settings screen's body is for a terminal of `area`, between
/// the title line and the status bar. Zero for a terminal too small to draw
/// at all, so a caller that asks before checking size gets a viewport that
/// never scrolls rather than an underflowed height.
///
/// `run_ui` calls this before each draw, so [`App::note_body_rows`](crate::lookout::app::App::note_body_rows) always
/// reflects the terminal about to be drawn to. `draw` builds the same four
/// full-screen panes' own `Rect`s off [`title_gap_rows`], so a body that did
/// not know about the rows spent there would get a `Rect` taller than the
/// space actually left before the status bar, and its last row would be
/// drawn only to be overwritten.
#[must_use]
pub fn body_rows(area: Rect) -> u16 {
    if area.width < super::MIN_TERM_WIDTH || area.height < MIN_HEIGHT {
        return 0;
    }
    // The status bar's own row, plus everything `title_gap_rows` spends
    // before a full-screen pane's body starts.
    area.height - 1 - title_gap_rows(area.height)
}

/// Rows `draw` spends between the top of the frame and a full-screen pane's
/// body: the title band, plus a blank row under it on a roomy terminal
/// ([`ROOMY_HEIGHT`]).
///
/// The one function both `draw` and [`body_rows`] call for this, so the two
/// can't drift the way they once did: a row added here reaches both without
/// a second edit.
#[must_use]
pub(super) fn title_gap_rows(height: u16) -> u16 {
    1 + u16::from(height >= ROOMY_HEIGHT)
}

impl Panes {
    /// The flock table alone, what every terminal shorter than
    /// [`PANE_TIERS`]' last threshold gets.
    pub const NONE: Self = Self {
        host: false,
        detail: false,
        feed: false,
    };

    /// How many rows these panes take together.
    ///
    /// `#[cfg(test)]`: `draw` claims each pane's rows off `floor` one
    /// constant at a time, so only
    /// `every_pane_tier_fits_the_height_it_claims` needs the sum.
    #[cfg(test)]
    #[must_use]
    pub const fn rows(self) -> u16 {
        let mut rows = 0;
        if self.host {
            rows += HOST_ROWS;
        }
        if self.detail {
            rows += DETAIL_ROWS;
        }
        if self.feed {
            rows += FEED_ROWS;
        }
        rows
    }
}

#[cfg(test)]
mod tests {

    use super::super::super::app::App;
    use super::super::super::theme::Palette;

    use std::time::Instant;

    use super::super::testing::*;
    use super::*;
    use crate::lookout::app::{Control, KeyPress, Msg};
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    /// The refusal itself must fit the narrow terminal it is refusing
    /// about: `Buffer::set_line` truncates in silence, so a refusal written
    /// as one long sentence could lose its own numbers. Asserted on the
    /// whole line, trimmed, since `contains` passes on a truncated line as
    /// happily as a whole one.
    #[test]
    fn a_terminal_below_the_floor_says_so_instead_of_drawing() {
        let app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            Instant::now(),
        );
        let frame = draw_to(&app, 28, 8);
        let mut lines = frame.lines();
        assert_eq!(lines.next().unwrap().trim_end(), "too small");
        assert_eq!(lines.next().unwrap().trim_end(), "need 33x6");
        assert!(!frame.contains("STATUS"), "no header was drawn");

        // Narrower still, and taller than the floor in rows: the numbers
        // must survive here too, because a 12-column terminal is precisely
        // the case this message exists for.
        let cramped = draw_to(&app, 12, 8);
        assert!(
            cramped.lines().nth(1).unwrap().trim_end() == "need 33x6",
            "the dimensions were cut off in the terminal that needed them"
        );

        // One row to write into: the second line has nowhere to go, and the
        // draw must not reach past the buffer for it.
        let single = draw_to(&app, 20, 1);
        assert_eq!(single.lines().next().unwrap().trim_end(), "too small");
        assert_eq!(single.lines().count(), 1);
    }

    /// Asserted on the whole line rather than with `contains`, since a `>`
    /// somewhere in a log path would satisfy `contains` and prove nothing.
    #[test]
    fn the_marker_sits_in_the_gutter_of_the_selected_row_and_nowhere_else() {
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            Instant::now(),
        );
        app.update(Msg::Snapshot {
            rows: (0..4)
                .map(|id| {
                    ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Online).build()
                })
                .collect(),
            at: Instant::now(),
        });
        app.update(Msg::Key(KeyPress::SelectDown));

        let frame = draw_to(&app, 100, 12);
        let rows: Vec<&str> = frame.lines().skip(3).take(5).collect();
        assert!(
            rows[0].starts_with("   \u{2588}\u{2588} FLOCK "),
            "the section header keeps a blank gutter too: {:?}",
            rows[0]
        );
        assert!(
            rows[1].starts_with("  0 "),
            "unselected rows keep a blank gutter: {:?}",
            rows[1]
        );
        assert!(
            rows[2].starts_with("> 1 "),
            "the marker is on row 1: {:?}",
            rows[2]
        );
        assert!(
            rows[3].starts_with("  2 "),
            "and on no other row: {:?}",
            rows[3]
        );
        assert_eq!(
            frame.lines().filter(|line| line.starts_with('>')).count(),
            1,
            "exactly one marker on the frame"
        );
    }

    /// The failure mode this catches is an arithmetic underflow on
    /// `height - 1` in a one-row terminal.
    #[test]
    fn drawing_never_panics_across_the_size_sweep() {
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            Instant::now(),
        );
        app.update(Msg::Snapshot {
            rows: (0..200)
                .map(|id| {
                    ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Online).build()
                })
                .collect(),
            at: Instant::now(),
        });
        for (width, height) in [(1, 1), (20, 3), (31, 6), (80, 24), (250, 60), (400, 200)] {
            let _ = draw_to(&app, width, height);
        }
    }

    /// The pending confirm must survive even when the body is too short to
    /// show `content_lines`' own copy of it: `status_line`'s fixed row
    /// always draws, so an armed edit is never invisible.
    #[test]
    fn an_armed_settings_candidate_survives_a_body_too_short_to_hold_it() {
        let mut app = super::super::fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();

        // 10 rows, 8 of them body: nowhere near the ~20 rows
        // `settings_snapshot` needs, so the body's own echo of this line is
        // not on screen at this height.
        let rendered = draw_to(&app, 200, 10);
        let last_line = rendered.lines().last().expect("at least one row");
        assert!(
            last_line.contains(text.trim()),
            "the confirm must survive on the status bar's fixed row: {last_line:?}"
        );
    }

    #[test]
    fn every_pane_tier_fits_the_height_it_claims() {
        for height in super::super::flock::MIN_HEIGHT..=200 {
            let panes = panes_for(height);
            let fixed = CHROME_ROWS + 1 /* banner */ + panes.rows();
            // A tier that shows a pane must leave the table at least three
            // rows; the floor tier, which shows none, only has to leave one.
            let floor = if panes.rows() == 0 { 1 } else { 3 };
            assert!(
                fixed + floor <= height,
                "height {height} chose {panes:?}, needing {} rows",
                fixed + floor
            );
        }
    }

    /// `every_pane_tier_fits_the_height_it_claims` picks up `DETAIL_ROWS`
    /// automatically, so a failure there means the tier table is wrong, not
    /// this test.
    #[test]
    fn the_detail_pane_claims_the_rows_it_draws() {
        let app =
            super::super::fixtures::with_selection(super::super::fixtures::sheep_with_lambs());
        assert_eq!(
            super::super::detail::detail_lines(&app, 120).len(),
            usize::from(DETAIL_ROWS - 1),
            "one rule plus its content lines"
        );
    }

    /// Detail goes first, the most redundant pane on the screen. Feed goes
    /// second: its content exists nowhere else, but five lines of a busy
    /// log is thin. The host strip goes last, at one row.
    #[test]
    fn panes_drop_in_a_fixed_order_as_the_terminal_shortens() {
        assert_eq!(
            panes_for(60),
            Panes {
                host: true,
                detail: true,
                feed: true
            }
        );
        assert_eq!(
            panes_for(24),
            Panes {
                host: true,
                detail: true,
                feed: true
            }
        );
        assert_eq!(
            panes_for(23),
            Panes {
                host: true,
                detail: false,
                feed: true
            }
        );
        assert_eq!(
            panes_for(18),
            Panes {
                host: true,
                detail: false,
                feed: true
            }
        );
        assert_eq!(
            panes_for(17),
            Panes {
                host: true,
                detail: false,
                feed: false
            }
        );
        assert_eq!(
            panes_for(14),
            Panes {
                host: true,
                detail: false,
                feed: false
            }
        );
        assert_eq!(panes_for(13), Panes::NONE);
        assert_eq!(
            panes_for(super::super::flock::MIN_HEIGHT),
            Panes::NONE,
            "12a's frame, untouched"
        );
    }

    /// `Buffer::set_line` outside the area is a panic in debug and a silent
    /// no-op otherwise, and the arithmetic here has four moving parts.
    ///
    /// Swept in both link states, because they lay the bottom stack out
    /// differently: live gets the detail band and the feed, frozen gets the
    /// link panel in place of both.
    #[test]
    fn every_pane_lands_inside_its_own_rows_across_the_size_sweep() {
        let live = super::super::fixtures::full_app();
        let mut lost = super::super::fixtures::full_app();
        lost.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: super::super::fixtures::FROZEN_WHY.to_string(),
        });
        for (frozen, app) in [(false, &live), (true, &lost)] {
            for height in super::super::flock::MIN_HEIGHT..=60 {
                for width in [super::super::MIN_TERM_WIDTH, 40, 51, 80, 120, 200] {
                    let frame = draw_to(app, width, height);
                    let lines: Vec<&str> = frame.lines().collect();
                    let panes = panes_for(height);

                    // The table's own row band, recomputed independently of
                    // `draw`: title (1) + banner (1, both fixtures carry one) +
                    // host strip if up + header/rule (2) is where it starts;
                    // `floor`, walked the same way `draw` does, is where it ends.
                    let table_body_start = 2 + if panes.host { HOST_ROWS } else { 0 } + 2;
                    let mut floor = height - 1;
                    if frozen {
                        if panes.feed {
                            floor -= super::super::link_panel::LINK_ROWS;
                        }
                    } else {
                        if panes.feed {
                            floor -= FEED_ROWS;
                        }
                        if panes.detail {
                            floor -= DETAIL_ROWS;
                        }
                    }
                    let table_body_end = floor;
                    for (i, line) in lines.iter().enumerate() {
                        let i = u16::try_from(i).unwrap_or(u16::MAX);
                        if i < table_body_start || i >= table_body_end {
                            continue;
                        }
                        assert!(
                            !line.starts_with("bleats  "),
                            "the feed header sits inside the table's own rows at \
                             {width}x{height}, row {i}"
                        );
                        assert!(
                            !line.starts_with("out  /home/ada/.shep/logs/"),
                            "the detail pane's out path sits inside the table's \
                             own rows at {width}x{height}, row {i}"
                        );
                    }

                    // Not `lines.len() == height`: `frames::render_text` maps
                    // `(0..area.height)` by construction, so that holds even for
                    // a `draw` that drew nothing. It is a property of the
                    // renderer, not of this layout.
                    let last = lines.last().unwrap();
                    let mark = if frozen {
                        "\u{2588} frozen"
                    } else {
                        "read-only"
                    };
                    assert!(
                        last.contains(mark),
                        "the status bar survived at {width}x{height}: {last:?}"
                    );
                    // The row above the status bar belongs to the bottom-most
                    // pane that is up, so it is never blank: a blank one means
                    // the upward layout left a hole. One condition for both
                    // link states: `PANE_TIERS` never gives a terminal the
                    // detail band without the feed, and the link panel draws
                    // at the feed's own tier.
                    if panes.feed || panes.detail {
                        let above = lines[lines.len() - 2];
                        assert!(
                            !above.trim().is_empty(),
                            "a blank row above the status bar at {width}x{height}"
                        );
                    }
                    // Every pane that is up appears exactly once and sits in
                    // its own band.
                    if panes.host {
                        let positions: Vec<usize> = lines
                            .iter()
                            .enumerate()
                            .filter(|(_, l)| l.starts_with("host  "))
                            .map(|(i, _)| i)
                            .collect();
                        assert_eq!(positions.len(), 1, "the strip at {width}x{height}");
                        assert!(
                            u16::try_from(positions[0]).unwrap_or(u16::MAX) < table_body_start,
                            "the strip at {width}x{height} sits at row {}, at or below the table",
                            positions[0]
                        );
                    }
                    if panes.feed {
                        // `contains`, not `starts_with`: the chip leads the
                        // line, and nothing else on screen carries either word.
                        // A frozen frame's `BLEATS` is gone and `THE LINK` is
                        // in its slot.
                        let chip = if frozen { "THE LINK" } else { "BLEATS" };
                        let positions: Vec<usize> = lines
                            .iter()
                            .enumerate()
                            .filter(|(_, l)| l.contains(chip))
                            .map(|(i, _)| i)
                            .collect();
                        assert_eq!(positions.len(), 1, "the {chip} header at {width}x{height}");
                        assert!(
                            u16::try_from(positions[0]).unwrap_or(0) >= table_body_end,
                            "the {chip} header at {width}x{height} sits at row {}, inside or above the table",
                            positions[0]
                        );
                    }
                    if panes.detail && !frozen {
                        // The `\u{2502}` divider, not a bare `out  `: the feed's
                        // own body lines are tagged `out  ` too, and the merged
                        // log row's own path can truncate away at a narrow
                        // width, but its divider never does.
                        let positions: Vec<usize> = lines
                            .iter()
                            .enumerate()
                            .filter(|(_, l)| l.starts_with("out  ") && l.contains('\u{2502}'))
                            .map(|(i, _)| i)
                            .collect();
                        assert_eq!(
                            positions.len(),
                            1,
                            "the detail pane's out path at {width}x{height}"
                        );
                        assert!(
                            u16::try_from(positions[0]).unwrap_or(0) >= table_body_end,
                            "the detail pane's out path at {width}x{height} sits at row {}, inside or above the table",
                            positions[0]
                        );
                    }
                    // The two panes the link panel replaces are gone outright,
                    // not merely moved: both are readings a dead shepherd
                    // cannot refresh.
                    if frozen {
                        assert!(
                            !lines.iter().any(|l| l.contains("BLEATS")),
                            "the feed survived a freeze at {width}x{height}"
                        );
                        assert!(
                            !lines.iter().any(|l| l.starts_with("lambs  ")),
                            "the detail band survived a freeze at {width}x{height}"
                        );
                    }
                }
            }
        }
    }

    /// Whatever else is on screen, the table gets the remainder, and at the
    /// tier where all three panes are up it still has room for more than a
    /// couple of rows.
    /// The two blank chrome rows are spent only above [`ROOMY_HEIGHT`].
    ///
    /// Guards the trade the constant exists for: air on a tall terminal,
    /// none on a short one where every row is a sheep you cannot see.
    #[test]
    fn a_short_terminal_spends_no_rows_on_air() {
        let app = super::super::fixtures::full_app();
        let short = draw_to(&app, 120, ROOMY_HEIGHT - 1);
        let tall = draw_to(&app, 120, ROOMY_HEIGHT);

        let blanks = |frame: &str| {
            frame
                .lines()
                .take(6)
                .filter(|line| line.trim().is_empty())
                .count()
        };
        assert_eq!(blanks(&short), 0, "short:\n{short}");
        assert_eq!(blanks(&tall), 2, "tall:\n{tall}");
    }

    #[test]
    fn the_flock_table_keeps_the_middle_of_the_screen() {
        let app = super::super::fixtures::full_app(); // twelve sheep
        let frame = draw_to(&app, 120, 24);
        let data_rows = frame
            .lines()
            .filter(|line| line.starts_with("  ") || line.starts_with("> "))
            .filter(|line| line.trim_start().starts_with(|c: char| c.is_ascii_digit()))
            .count();
        assert!(data_rows >= 5, "the table got {data_rows} rows at 120x24");
    }
}
