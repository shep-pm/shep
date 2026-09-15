//! The machine the flock runs on, as one line above a listing.
//!
//! Nothing here reads the machine. Three of the four numbers are rates, and
//! a rate needs two readings separated by time, so the shepherd holds the
//! baseline and serves the difference off its own tick
//! (`shep_daemon::host`). A one-shot `shep flock` and a followed one ask the
//! same question and get the same answer, which is the only way the two can
//! be made to agree.
//!
//! The strip is `lookout`'s, ported rather than reinvented: self-labelled
//! segments joined in a drop order, truncated from the right so truncation
//! is the drop order with no second mechanism, and muted except for the two
//! gauges. See `lookout::view::host`, which argues each of those.
//!
//! One difference, and it is the reason rather than the rule that carries
//! over. Lookout labels every segment `host` or `flock` because it mixes
//! both halves on one line and a truncated `mem 12.4G` beside a bare
//! `mem 706.0M` says nothing. This line has one half, so `host` is said once
//! at the front, as it always has been, and each segment names only which
//! number it is.

use shep_core::protocol::HostUsage;

use crate::lookout::view::cell;
use crate::output::human_bytes;
use crate::output::width::char_columns;
use crate::style::Presentation;
use crate::vocabulary::Role;

/// How many cells each gauge draws, matching `lookout`'s own two.
const GAUGE_CELLS: usize = 10;

/// What separates one segment from the next, matching `lookout`'s.
const SEPARATOR: &str = "   ";

/// The strip, fitted to `width` and painted for `style`.
///
/// `None` is a platform `sysinfo` cannot read at all, which is not the same
/// claim as a rate with no window behind it yet: that one renders `-`, in
/// its own segment, beside numbers that did arrive.
///
/// Two forms, on [`crate::style::StyleLevel::boxes`], the same dial that
/// decides whether the table under this line is box-drawn. Drawn, it carries
/// two gauges and truncates to `width` with a `…`. Plain, it drops both
/// gauges and is not truncated at all, exactly as `output::render_table`
/// drops its rules and ignores the width: a pipe has no window, and `width`
/// there is the 80 [`crate::output::terminal_width`] answers when there is
/// nothing to measure. Cutting real numbers to fit a guess would be the
/// worst of both.
pub(crate) fn strip(usage: Option<HostUsage>, style: Presentation, width: usize) -> String {
    let drawn = style.level.boxes();
    let runs = runs(usage, drawn);
    if drawn {
        return fit(&runs, style, width);
    }
    runs.iter().map(|run| run.text.as_str()).collect()
}

/// One run of the strip and the role it renders in.
struct Run {
    text: String,
    ink: Ink,
}

/// Which colour a run wears.
///
/// Most of the line is words and numbers with no status behind them, so it
/// stays muted. The two gauges are the exception, for
/// `lookout::view::host`'s reason: two bars on one line are unreadable
/// without something to tell them apart by. The roles are lookout's own,
/// `attention` for the busy-ness bar and `sky` for the memory one, so the
/// same number wears the same colour in both places.
#[derive(Clone, Copy)]
enum Ink {
    Muted,
    Cpu,
    Mem,
}

impl Ink {
    /// The role this ink paints through, or `None` where colour is off.
    fn role(self, style: Presentation) -> Option<Role> {
        if !style.colour {
            return None;
        }
        Some(match self {
            Ink::Muted => Role::Ink3,
            Ink::Cpu => Role::Butter,
            Ink::Mem => Role::Sky,
        })
    }

    /// `text` wrapped in this ink's escape span, or returned untouched.
    fn paint(self, text: &str, style: Presentation) -> String {
        match self.role(style) {
            None => text.to_owned(),
            Some(role) => {
                let painted = crate::output::paint::style_for(role, style.deep_colour);
                format!("{painted}{text}{painted:#}")
            }
        }
    }
}

/// `runs` truncated to at most `width` columns.
///
/// `lookout::view::host`'s `Ink::fit` without the padding: a line written to
/// a stream ends where it ends, where a ratatui buffer has to be filled to
/// its own width. Single-width-char truncation with one column held back for
/// the trailing `…`, so the operator can see that something was cut.
fn fit(runs: &[Run], style: Presentation, width: usize) -> String {
    let total: usize = runs
        .iter()
        .flat_map(|run| run.text.chars())
        .map(char_columns)
        .sum();
    if total <= width {
        return runs
            .iter()
            .map(|run| run.ink.paint(&run.text, style))
            .collect();
    }
    if width == 0 {
        return String::new();
    }
    let budget = width - 1;
    let mut out = String::new();
    let mut used = 0;
    for run in runs {
        if used >= budget {
            break;
        }
        let mut kept = String::new();
        for c in run.text.chars() {
            let columns = char_columns(c);
            if used + columns > budget {
                break;
            }
            kept.push(c);
            used += columns;
        }
        if !kept.is_empty() {
            out.push_str(&run.ink.paint(&kept, style));
        }
    }
    out.push_str(&Ink::Muted.paint("…", style));
    out
}

/// The busy-ness bar.
///
/// A window too short to divide by draws an empty gauge rather than no
/// gauge, which is `lookout::view::host`'s answer for its own load bar: the
/// segment's text says `-`, and the bar holds the offsets of everything
/// after it still while the first reading arrives.
fn cpu_gauge(percent: Option<f32>) -> String {
    match percent {
        Some(percent) => cell::gauge(
            percent.clamp(0.0, 100.0).round() as u64,
            Some(100),
            GAUGE_CELLS,
        ),
        None => cell::gauge(0, None, GAUGE_CELLS),
    }
}

/// One rate rendered, or the dash that means it has not been measured.
///
/// Never a zero. A machine doing nothing and a machine not yet measured are
/// different claims, and the first reading after a shepherd boots is always
/// the second one.
fn rate(bytes_per_second: Option<u64>) -> String {
    match bytes_per_second {
        Some(bytes) => format!("{}/s", human_bytes(bytes)),
        None => "-".to_owned(),
    }
}

/// The runs, widest set first. `gauges` draws the two bars.
fn runs(usage: Option<HostUsage>, gauges: bool) -> Vec<Run> {
    let muted = |text: String| Run {
        text,
        ink: Ink::Muted,
    };
    let Some(usage) = usage else {
        // `lookout::view::host`'s own words for the same state, which is a
        // platform answer rather than a reading that has not landed yet.
        return vec![muted(
            "host  usage is not available on this platform".to_owned(),
        )];
    };

    let (read, written) = usage
        .disk_bytes_per_second
        .map_or((None, None), |(read, written)| (Some(read), Some(written)));
    let (received, transmitted) = usage
        .network_bytes_per_second
        .map_or((None, None), |(received, transmitted)| {
            (Some(received), Some(transmitted))
        });
    let cpu = match usage.cpu_percent {
        Some(percent) => format!("{percent:.0}%"),
        None => "-".to_owned(),
    };
    // A gauge and the number it draws are one segment, so the space between
    // them belongs to the gauge and goes when it does.
    let mut out = vec![
        // Said once, at the front: everything after it is this machine's.
        muted("host  cpu  ".to_owned()),
    ];
    if gauges {
        out.push(Run {
            text: cpu_gauge(usage.cpu_percent),
            ink: Ink::Cpu,
        });
        out.push(muted(" ".to_owned()));
    }
    out.push(muted(cpu));
    out.push(muted(format!("{SEPARATOR}mem  ")));
    if gauges {
        out.push(Run {
            text: cell::gauge(
                usage.memory_used_bytes,
                Some(usage.memory_total_bytes),
                GAUGE_CELLS,
            ),
            ink: Ink::Mem,
        });
        out.push(muted(" ".to_owned()));
    }
    out.extend([
        muted(format!(
            "{} / {}",
            human_bytes(usage.memory_used_bytes),
            human_bytes(usage.memory_total_bytes)
        )),
        // The two gauges first, then the detail: an operator scanning for
        // "is this machine in trouble" reads a bar, and truncation takes
        // the far end of the line.
        muted(format!(
            "{SEPARATOR}disk  r {} w {}",
            rate(read),
            rate(written)
        )),
        muted(format!(
            "{SEPARATOR}net  rx {} tx {}",
            rate(received),
            rate(transmitted)
        )),
    ]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::StyleLevel;

    /// The reading behind every example here, so one number tells the cases
    /// apart: `rates` on or off is the whole difference between a machine
    /// that has been measured and one that has not.
    fn usage(rates: bool) -> HostUsage {
        HostUsage {
            cpu_percent: rates.then_some(11.459_433),
            memory_used_bytes: 39_963_869_184,
            memory_total_bytes: 51_539_607_552,
            disk_bytes_per_second: rates.then_some((1_258_291, 491_520)),
            network_bytes_per_second: rates.then_some((24_594, 9_260)),
        }
    }

    /// Boxes on, colour off: the drawn strip, in a form an exact string can
    /// pin. `Presentation::new` would turn colour on with the level, and
    /// the two are separate dials everywhere else.
    fn drawn() -> Presentation {
        Presentation {
            level: StyleLevel::Plain,
            colour: false,
            deep_colour: false,
            width: 200,
        }
    }

    /// Colour on too, at the 16-colour tier, so an escape in the output is a
    /// deliberate one rather than a terminal-detection accident.
    fn coloured() -> Presentation {
        Presentation::new(StyleLevel::Full, None, None, None, 200)
    }

    /// fails if the strip loses a number, a unit or a gauge. Pinned whole
    /// rather than by `contains`, because the spacing is the structure: a
    /// reader scans the two bars at fixed offsets, which a lost space or an
    /// extra one breaks without losing any information.
    #[test]
    fn a_measured_reading_names_all_four() {
        assert_eq!(
            strip(Some(usage(true)), drawn(), 200),
            "host  cpu  █░░░░░░░░░ 11%   mem  ████████░░ 37.2G / 48.0G   \
             disk  r 1.2M/s w 480.0K/s   net  rx 24.0K/s tx 9.0K/s"
        );
    }

    /// The plain form, which is what a pipe gets: no bars, and no cut to a
    /// width nothing measured. `output::render_table` answers a pipe the
    /// same way, with no rules and no column dropping.
    ///
    /// fails if a piped listing starts carrying block-drawing characters, or
    /// starts losing the far end of the line to a guessed 80 columns.
    #[test]
    fn a_plain_strip_drops_both_bars_and_is_never_cut() {
        let plain = strip(Some(usage(true)), Presentation::BARE, 80);

        assert_eq!(
            plain,
            "host  cpu  11%   mem  37.2G / 48.0G   \
             disk  r 1.2M/s w 480.0K/s   net  rx 24.0K/s tx 9.0K/s"
        );
        assert!(!plain.contains('█') && !plain.contains('░'));
        assert!(!plain.contains('…'), "80 columns is a guess, not a window");
    }

    /// fails if a shepherd with no window yet starts printing zeroes. A rate
    /// with no window is absent, and absent is not idle.
    ///
    /// The CPU bar draws empty rather than not drawing at all, which is
    /// `lookout::view::host`'s answer for its own load bar: the `-` beside
    /// it carries the claim, and the bar holds the offsets of everything
    /// after it still.
    #[test]
    fn a_reading_with_no_window_dashes_every_rate_and_still_prints_memory() {
        assert_eq!(
            strip(Some(usage(false)), drawn(), 200),
            "host  cpu  ░░░░░░░░░░ -   mem  ████████░░ 37.2G / 48.0G   \
             disk  r - w -   net  rx - tx -"
        );
    }

    /// fails if a platform `sysinfo` cannot read starts rendering as a
    /// machine that is merely idle. Lookout's own words, so the two views
    /// say one thing about one state.
    #[test]
    fn a_platform_that_cannot_be_read_says_so() {
        assert_eq!(
            strip(None, Presentation::BARE, 200),
            "host  usage is not available on this platform"
        );
    }

    /// There is no drop loop and no width table: [`fit`] truncates from the
    /// right, so truncating is the drop order.
    ///
    /// fails if the order stops putting the two gauges first, or if a cut
    /// stops being visible.
    #[test]
    fn a_narrow_strip_truncates_visibly_and_keeps_the_gauges() {
        let narrow = strip(Some(usage(true)), drawn(), 40);

        assert!(narrow.starts_with("host  cpu  "), "got {narrow:?}");
        assert!(
            narrow.ends_with('…'),
            "a truncation the operator can see: {narrow:?}"
        );
        assert!(!narrow.contains("net  rx"), "net is the first thing off");
        assert_eq!(
            narrow.chars().map(char_columns).sum::<usize>(),
            40,
            "the cut lands on the budget, ellipsis included"
        );

        // And where it fits, nothing is cut.
        let full = strip(Some(usage(true)), drawn(), 200);
        assert!(!full.contains('…'));
        assert!(full.contains("net  rx 24.0K/s"));
    }

    /// fails if a window of no columns panics rather than printing nothing.
    /// A pty that has never been told how big it is reports zero, and
    /// `script(1)` hands `--follow` exactly that.
    #[test]
    fn a_strip_with_no_columns_is_empty_rather_than_a_panic() {
        assert_eq!(strip(Some(usage(true)), drawn(), 0), "");
        assert_eq!(strip(Some(usage(true)), drawn(), 1), "…");
    }

    /// fails if a second colour joins the two gauges, or if one of them
    /// loses its own. Two bars on one line are unreadable without something
    /// to tell them apart by, and everything else on the line is words and
    /// numbers with no status behind them.
    #[test]
    fn only_the_two_gauges_wear_a_colour_of_their_own() {
        let style = coloured();
        let painted = strip(Some(usage(true)), style, 200);

        let span = |role| {
            format!(
                "{}",
                crate::output::paint::style_for(role, style.deep_colour)
            )
        };
        assert_eq!(
            painted.matches(&span(Role::Butter)).count(),
            1,
            "the CPU bar and nothing else: {painted:?}"
        );
        assert_eq!(
            painted.matches(&span(Role::Sky)).count(),
            1,
            "the memory bar and nothing else: {painted:?}"
        );
        // Butter and sky each open exactly one run, and that run is the bar
        // rather than the label beside it.
        assert!(painted.contains(&format!("{}█░░░░░░░░░", span(Role::Butter))));
        assert!(painted.contains(&format!("{}████████░░", span(Role::Sky))));
    }

    /// fails if colour leaks into a bare listing, which is what a pipe gets.
    #[test]
    fn a_bare_strip_carries_no_escapes() {
        let bare = strip(Some(usage(true)), Presentation::BARE, 200);

        assert!(!bare.contains('\u{1b}'), "got {bare:?}");
    }

    /// fails if a truncated coloured strip leaves a colour open past its own
    /// run. Every span closes where its run does, cut or not, so the table
    /// under the strip is never painted by it.
    #[test]
    fn a_truncated_coloured_strip_closes_every_span_it_opens() {
        let painted = strip(Some(usage(true)), coloured(), 25);

        let opens = painted.matches("\u{1b}[").count();
        let closes = painted.matches("\u{1b}[0m").count();
        assert_eq!(
            opens,
            closes * 2,
            "one open and one reset per run: {painted:?}"
        );
    }
}
