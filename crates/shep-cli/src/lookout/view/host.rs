//! The host-usage strip: one line, several self-labelled segments.
//!
//! Half is read from this machine ([`super::super::source::HostSample`])
//! and half is summed from [`super::super::app::App::all_rows`], the whole
//! flock, never the filtered [`super::super::app::App::rows`]: a name
//! filter must not narrow what this strip claims. Every segment names its
//! half, so a truncated strip never leaves a bare `mem 12.4G` beside a bare
//! `mem 706.0M`.
//!
//! Segments join in the drop order (`up` last, load average first) and are
//! truncated from the right, so truncation is the drop order with no second
//! mechanism.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use shep_core::status::ProcStatus;

use super::super::app::App;
use super::super::theme::Palette;
use super::cell;
use crate::output::width::char_columns;
use crate::output::{human_bytes, human_duration};

/// The strip, fitted to `width`.
#[must_use]
pub fn strip_line(app: &App, width: u16) -> Line<'static> {
    Line::from(Ink::fit(&runs(app), width, app.palette()))
}

// One run of the strip and the role it renders in.
struct Run {
    text: String,
    ink: Ink,
}

// Which colour a run wears. Most of the line is words and numbers with no
// status behind them, so it stays muted. The load and memory gauges are the
// exception: two bars on one line are unreadable without something to tell
// them apart by, which is meaning rather than the plain decoration the rest
// of the line has none of.
#[derive(Clone, Copy)]
enum Ink {
    Muted,
    Load,
    Mem,
}

impl Ink {
    fn style(self, palette: Palette) -> Style {
        match self {
            Ink::Muted => palette.muted(),
            Ink::Load => palette.attention(),
            Ink::Mem => palette.sky(),
        }
    }

    // `runs` truncated to exactly `width` columns, one `Span` per surviving
    // run plus the tail. Mirrors `super::flock::fit`'s own algorithm
    // (single-width-char truncation, one column reserved for a trailing
    // `…`) but walks a list of coloured runs instead of one string, so a
    // caller with several roles on one line keeps the same
    // drop-from-the-right behaviour.
    fn fit(runs: &[Run], width: u16, palette: Palette) -> Vec<Span<'static>> {
        let width = usize::from(width);
        let total: usize = runs
            .iter()
            .flat_map(|run| run.text.chars())
            .map(char_columns)
            .sum();
        if total <= width {
            let mut spans: Vec<Span<'static>> = runs
                .iter()
                .map(|run| Span::styled(run.text.clone(), run.ink.style(palette)))
                .collect();
            spans.push(Span::styled(" ".repeat(width - total), palette.muted()));
            return spans;
        }
        if width == 0 {
            return vec![Span::raw(String::new())];
        }
        let budget = width - 1;
        let mut spans = Vec::new();
        let mut used = 0;
        for run in runs {
            if used >= budget {
                break;
            }
            let mut kept = String::new();
            for c in run.text.chars() {
                let c_width = char_columns(c);
                if used + c_width > budget {
                    break;
                }
                kept.push(c);
                used += c_width;
            }
            if !kept.is_empty() {
                spans.push(Span::styled(kept, run.ink.style(palette)));
            }
        }
        spans.push(Span::styled("…", palette.muted()));
        if budget > used {
            spans.push(Span::styled(" ".repeat(budget - used), palette.muted()));
        }
        spans
    }
}

// The load gauge: ten cells, filled by the one-minute average as a
// percentage of `cores`. `None`, and a reported zero cores, draw an empty
// gauge rather than a division by zero: a platform that cannot say how many
// cores it has cannot say what fraction of them is busy either.
fn load_gauge(load: f64, cores: Option<usize>) -> String {
    match cores.filter(|&count| count > 0) {
        Some(cores) => {
            let percent = (load / cores as f64 * 100.0).round() as u64;
            cell::gauge(percent, Some(100), 10)
        }
        None => cell::gauge(0, None, 10),
    }
}

// `N errored · N parked`, summed over the whole flock. Errored filters
// `ProcStatus::Errored`; parked counts a non-empty `pending`, the same
// field `crate::output::cfg_cell` reads for the `CFG` column.
fn summary(app: &App) -> String {
    let rows = app.all_rows();
    let errored = rows
        .iter()
        .filter(|row| row.info.status == ProcStatus::Errored)
        .count();
    let parked = rows
        .iter()
        .filter(|row| {
            row.info
                .pending
                .as_deref()
                .is_some_and(|fields| !fields.is_empty())
        })
        .count();
    format!("{errored} errored · {parked} parked")
}

// The runs, widest set first.
fn runs(app: &App) -> Vec<Run> {
    let mut out = Vec::with_capacity(9);
    let sep = || Run {
        text: "   ".to_string(),
        ink: Ink::Muted,
    };
    match app.host() {
        Some(host) => {
            let (one, five, fifteen) = host.load;
            let load_text = match host.cores {
                Some(cores) => {
                    format!("host  load {one:.2} {five:.2} {fifteen:.2} / {cores} cores  ")
                }
                // No denominator: the numbers alone are not readable, so they
                // are shown without a claim about how many cores they are
                // spread over rather than with a guessed one.
                None => format!("host  load {one:.2} {five:.2} {fifteen:.2}  "),
            };
            out.push(Run {
                text: load_text,
                ink: Ink::Muted,
            });
            out.push(Run {
                text: load_gauge(one, host.cores),
                ink: Ink::Load,
            });
            out.push(sep());
            out.push(Run {
                text: format!(
                    "host mem {} / {}  ",
                    human_bytes(host.memory_used_bytes),
                    human_bytes(host.memory_total_bytes)
                ),
                ink: Ink::Muted,
            });
            out.push(Run {
                text: cell::gauge(host.memory_used_bytes, Some(host.memory_total_bytes), 10),
                ink: Ink::Mem,
            });
        }
        None if app.host_unsupported() => out.push(Run {
            text: "host  usage is not available on this platform".to_string(),
            ink: Ink::Muted,
        }),
        // Reachable for at most one redraw: `tokio::time::interval`'s first
        // tick is immediate, so the heartbeat samples before the second
        // frame.
        None => out.push(Run {
            text: "host  not read yet".to_string(),
            ink: Ink::Muted,
        }),
    }

    // Summed from the whole flock, `all_rows`, not the filtered `rows`, so
    // `-` here always means no reading, never "the filter matched nothing".
    // `-`, not `0.0%`: `ProcessInfo::cpu_percent`'s `None` is unknown, and
    // zero would claim a measurement the shepherd never made.
    let rows = app.all_rows();
    let cpu: Option<f32> = rows
        .iter()
        .filter_map(|row| row.info.cpu_percent)
        .fold(None, |sum, value| Some(sum.unwrap_or(0.0) + value));
    let mem: Option<u64> = rows
        .iter()
        .filter_map(|row| row.info.memory_bytes)
        .fold(None, |sum, value| Some(sum.unwrap_or(0) + value));
    let cpu_text = cpu.map_or_else(
        || "flock cpu -".to_string(),
        |cpu| format!("flock cpu {cpu:.1}%"),
    );
    out.push(sep());
    out.push(Run {
        text: format!("{cpu_text} {}", cell::sparkline(app.flock_cpu_history(), 8)),
        ink: Ink::Muted,
    });
    out.push(sep());
    out.push(Run {
        text: mem.map_or_else(
            || "flock mem -".to_string(),
            |mem| format!("flock mem {}", human_bytes(mem)),
        ),
        ink: Ink::Muted,
    });
    out.push(sep());
    out.push(Run {
        text: summary(app),
        ink: Ink::Muted,
    });

    // Last, and therefore the first thing a narrow terminal loses: a host that
    // has been up six days explains nothing about right now.
    if let Some(host) = app.host() {
        out.push(sep());
        out.push(Run {
            text: format!("up {}", human_duration(host.uptime_seconds * 1_000)),
            ink: Ink::Muted,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::MIN_TERM_WIDTH;
    use super::super::fixtures::{
        app_with, flock_of, plain, rendered, sample, with_host, with_host_none,
    };
    use super::*;
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    /// `fixture_with` in the brief's own words: an app over one flock and no
    /// host reading, since these tests exercise `summary` alone.
    fn fixture_with(flock: Vec<ProcessInfo>) -> App {
        app_with(flock, plain())
    }

    #[test]
    fn the_load_gauge_measures_against_the_core_count() {
        // 5.12 of 14 cores is 37%, four of ten cells.
        assert_eq!(load_gauge(5.12, Some(14)), "████░░░░░░");
    }

    #[test]
    fn a_host_that_cannot_report_cores_draws_no_load_gauge() {
        assert_eq!(load_gauge(5.12, None), "░░░░░░░░░░");
    }

    #[test]
    fn the_summary_counts_errored_and_parked_sheep() {
        let app = fixture_with(vec![
            ProcessInfo::builder(1, "flaky", ProcStatus::Errored).build(),
            ProcessInfo::builder(2, "catcher", ProcStatus::Online)
                .pending(Some(vec!["max_memory".to_string(), "err_file".to_string()]))
                .build(),
        ]);
        assert_eq!(summary(&app), "1 errored · 1 parked");
    }

    /// A truncated strip must never leave a bare `mem 12.4G` beside a bare
    /// `mem 706.0M`, one the host's and one the flock's.
    #[test]
    fn every_segment_names_whose_number_it_is() {
        let app = with_host(sample(), flock_of(4, 1));
        let line = rendered(&strip_line(&app, 200));
        for segment in ["host  load", "host mem", "flock cpu", "flock mem", "up "] {
            assert!(line.contains(segment), "missing {segment:?} in {line:?}");
        }
    }

    /// There is no drop loop and no width table: `Ink::fit` truncates from
    /// the right, so truncating is the drop order.
    #[test]
    fn a_narrow_strip_truncates_visibly_and_keeps_the_load_average() {
        let app = with_host(sample(), flock_of(4, 1));

        let narrow = rendered(&strip_line(&app, 40));
        assert!(narrow.starts_with("host  load"), "got {narrow:?}");
        assert!(
            narrow.ends_with('…'),
            "a truncation the operator can see: {narrow:?}"
        );
        assert!(
            !narrow.contains("up "),
            "`up` is the first thing off the end"
        );

        // At the floor the strip still says whose number it is quoting, which
        // is the reason every segment carries its own label.
        let floor = rendered(&strip_line(&app, MIN_TERM_WIDTH));
        assert!(floor.starts_with("host  load"), "got {floor:?}");

        // And where it fits, nothing is cut.
        let full = rendered(&strip_line(&app, 200));
        assert!(!full.contains('…'));
        assert!(
            full.contains("up 6d"),
            "the last segment is there: {full:?}"
        );
    }

    /// `ProcessInfo`'s `None` covers three cases (not running, under one
    /// sampling window, or a shepherd predating the field), and a reader
    /// renders all three as unknown, never as zero.
    #[test]
    fn a_flock_with_no_readings_shows_a_dash_and_not_a_zero() {
        let app = with_host(
            sample(),
            vec![ProcessInfo::builder(1, "web", ProcStatus::Errored).build()],
        );
        let line = rendered(&strip_line(&app, 200));
        assert!(line.contains("flock cpu -"), "got {line:?}");
        assert!(line.contains("flock mem -"), "got {line:?}");
        assert!(!line.contains("0.0%"));
    }

    /// A silently dropped host half would look identical to numbers that
    /// have not arrived yet.
    #[test]
    fn an_unread_host_says_which_of_the_two_reasons_it_is() {
        let unsupported = with_host_none(flock_of(4, 1), true);
        assert!(
            rendered(&strip_line(&unsupported, 200))
                .contains("host  usage is not available on this platform")
        );

        let not_yet = with_host_none(flock_of(4, 1), false);
        assert!(rendered(&strip_line(&not_yet, 200)).contains("host  not read yet"));

        // Both keep the flock half, which lookout can always compute.
        assert!(rendered(&strip_line(&unsupported, 200)).contains("flock cpu"));
    }

    /// A filter matching nothing would otherwise make a running flock's
    /// strip print `-`, the same cell reserved for "no reading arrived yet".
    #[test]
    fn the_flock_totals_ignore_the_filter() {
        let mut app = with_host(sample(), flock_of(4, 1));
        let full = rendered(&strip_line(&app, 200));
        assert!(full.contains("flock cpu 3.5%"), "sanity: got {full:?}");

        // A filter matching nothing empties the table (`rows()`) without
        // touching the flock itself (`all_rows()`, what the strip reads).
        app.set_filter_for_tests("zzz");
        assert!(app.rows().is_empty(), "sanity: the filter matched nothing");
        let filtered = rendered(&strip_line(&app, 200));

        assert_eq!(
            full, filtered,
            "the strip must not change when the table's filter does: {filtered:?}"
        );
        assert!(
            !filtered.contains("flock cpu -"),
            "a filter matching nothing is not the same as no reading arriving: {filtered:?}"
        );
    }
}
