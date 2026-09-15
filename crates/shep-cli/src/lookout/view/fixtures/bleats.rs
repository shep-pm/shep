//! Feeds, the bleats pane, and the tail lines they carry.

use ratatui::text::Line;
use shep_core::protocol::ProcessInfo;
use shep_core::status::ProcStatus;

use crate::lookout::app::{App, KeyPress, Msg};
use crate::lookout::level::Level;
use crate::lookout::tail::{Stream, Tail, TailLine};
use crate::lookout::theme::Palette;

use super::flock::{app_with, flock_of, with_selection};
use super::palette::plain;

/// A dashboard with a flock of three sheep, the first one selected, and
/// `tail` applied as this refresh's feed.
pub fn with_feed(tail: Tail) -> App {
    let mut app = app_with(flock_of(3, 0), plain());
    app.update(Msg::Bleats { tail });
    app
}

/// Like [`with_feed`], but selects sheep `id` first, for the tests that need
/// the header to name a specific sheep.
pub fn with_feed_and_selection(tail: Tail, id: u32) -> App {
    let mut app = app_with(flock_of(3, 0), plain());
    for _ in 0..id {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
    app.update(Msg::Bleats { tail });
    app
}

/// Like [`with_feed`], but with an explicit palette, for the one test that
/// asserts on a specific foreground colour.
pub fn with_feed_and_palette(tail: Tail, palette: Palette) -> App {
    let mut app = app_with(flock_of(3, 0), palette);
    app.update(Msg::Bleats { tail });
    app
}

/// The full-screen bleats pane, open on `web`, with all three filter axes
/// set (stream `err`, level `warn`, match `pool`) over a feed mixing one
/// line that survives every axis with three that each fail exactly one, for
/// the filter row's own tests.
///
/// Filters are stacked through [`App::bleats_pane_mut_for_tests`] rather
/// than through `o`, `m` and `/`, so a test naming the axes it wants does not
/// have to walk each cycle to reach them.
pub fn bleats_pane_with_filters() -> App {
    let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    app.update(Msg::Bleats {
        tail: Tail {
            lines: vec![
                line(Stream::Err, "ERROR pool exhausted"), // all three hold
                line(Stream::Out, "ERROR pool exhausted"), // wrong stream
                line(Stream::Err, "INFO pool warming"),    // below the minimum
                line(Stream::Err, "ERROR disk full"),      // no match
            ],
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 128,
            note: None,
        },
    });
    app.update(Msg::Key(KeyPress::Bleats));
    let pane = app
        .bleats_pane_mut_for_tests()
        .expect("Msg::Key(KeyPress::Bleats) opened the pane on the sheep selected above");
    pane.set_stream(Some(Stream::Err));
    pane.set_min_level(Some(Level::Warn));
    pane.set_match("pool".to_string());
    app
}

/// The full-screen bleats pane, open on `web`, over a feed of `n` lines
/// numbered `line-0`..`line-{n-1}`, oldest first — enough to exceed any
/// test's body height, for the scrolling and follow tests.
pub fn bleats_pane_with_lines(n: u32) -> App {
    let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    app.update(Msg::Bleats {
        tail: Tail {
            lines: (0..n)
                .map(|i| line(Stream::Out, &format!("line-{i}")))
                .collect(),
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 1_024,
            note: None,
        },
    });
    app.update(Msg::Key(KeyPress::Bleats));
    app
}

/// A feed whose newest lines are short and whose older ones are long, so a
/// page sized from the tail is far too many lines once the view is scrolled
/// back into the long stretch.
///
/// The shape a wrap-aware page step has to survive: `page_amount_up` measures
/// from the tail, and a tail of one-row lines says "a page is N lines" while
/// the older region draws each of those lines as three rows.
#[must_use]
pub fn bleats_pane_with_mixed_line_lengths() -> App {
    let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    // Heights cycling 1, 2, 3, 4 rows rather than a uniform block. Uniform
    // costs make a backward count and a window's own length agree, which is
    // exactly the case that hides a direction-mismatched page size; the gap
    // only appears where consecutive lines wrap to different heights.
    let mut lines: Vec<TailLine> = (0..40)
        .map(|i| {
            let padding = "y".repeat(50 * (i % 4));
            line(Stream::Out, &format!("old-{i} {padding}"))
        })
        .collect();
    lines.extend((0..40).map(|i| line(Stream::Out, &format!("new-{i}"))));
    app.update(Msg::Bleats {
        tail: Tail {
            lines,
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 1_024,
            note: None,
        },
    });
    app.update(Msg::Key(KeyPress::Bleats));
    app
}

/// A feed whose long line is double-width characters, so its wrapped height
/// depends on display columns rather than `char` count.
///
/// Every other wrap fixture here is single-width ASCII, where `char_columns`
/// and a naive per-`char` count agree. That makes them blind to the exact
/// regression this repo has already fixed on two other branches: a
/// full-width character occupies two columns, so 60 of them wrap to twice
/// the rows 60 ASCII characters would.
#[must_use]
pub fn bleats_pane_with_a_wide_line() -> App {
    let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    app.update(Msg::Bleats {
        tail: Tail {
            lines: vec![line(Stream::Out, &"\u{5e83}".repeat(60))],
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 1_024,
            note: None,
        },
    });
    app.update(Msg::Key(KeyPress::Bleats));
    app
}

/// The full-screen bleats pane, open on `web`, over a feed with one line
/// comfortably wider than 80 columns, for the wrap tests.
pub fn bleats_pane_with_long_line() -> App {
    let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    app.update(Msg::Bleats {
        tail: Tail {
            lines: vec![
                line(Stream::Out, "short line"),
                line(Stream::Out, &"x".repeat(200)),
            ],
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 1_024,
            note: None,
        },
    });
    app.update(Msg::Key(KeyPress::Bleats));
    app
}

/// [`crate::lookout::view::bleats_full::draw`]'s own lines, for a test that needs the
/// bleats pane's rendered rows without a [`Buffer`] round trip. Thin
/// wrapper: [`crate::lookout::view::bleats_full::draw_lines`] is `pub(crate)` for exactly
/// this, but lives in a sibling module the top-level fixture callers in
/// `app.rs` do not otherwise reach.
///
/// [`Buffer`]: ratatui::buffer::Buffer
pub fn draw_lines(app: &App, width: u16, rows: usize) -> Vec<Line<'static>> {
    crate::lookout::view::bleats_full::draw_lines(app, width, rows)
}

/// One sheep, `catcher`, selected, with a two-line feed applied and its log
/// paths pointing at real files in a leaked tempdir, so `fs::metadata` in
/// [`crate::lookout::view::detail::log_row`] succeeds the way it would against a live
/// sheep's own logs.
///
/// The tempdir is whatever [`tempfile`] resolves for the host: no attempt is
/// made here to force it short. A prior version tried, picking `/tmp` on
/// unix and `RUNNER_TEMP` on Windows, because `log_row`'s own tests once
/// asserted against hardcoded widths (160/70/60) that only produced the
/// intended three-tier behaviour when the path was short. `RUNNER_TEMP` is
/// unset outside GitHub Actions, so a real Windows box fell to the OS
/// default there — a ~35-column prefix under the user profile — and failed
/// the width-160 test for a reason that had nothing to do with the code
/// under test. Those tests now derive their widths from this fixture's own
/// rendered path lengths (see `detail::tests::log_row_thresholds`), so no
/// path-length assumption belongs here any more.
///
/// The tempdir is leaked (`TempDir::keep`) rather than dropped: dropping it
/// would delete the files before the test that calls this reads them, and
/// the OS reclaims a leaked temp directory on its own schedule regardless.
pub fn app_fixture() -> App {
    let dir = tempfile::Builder::new()
        .prefix("shep-fx-")
        .tempdir()
        .expect("a tempdir for the fixture's logs");
    let out_path = dir.path().join("catcher-out.log");
    let err_path = dir.path().join("catcher-err.log");
    std::fs::write(&out_path, b"listening on :8080\n").expect("write the out log");
    std::fs::write(&err_path, b"warn: retrying upstream\n").expect("write the err log");
    let _ = dir.keep();

    let info = ProcessInfo::builder(7, "catcher", ProcStatus::Online)
        .pid(Some(48_107))
        .uptime_ms(4_512_000)
        .out_file(Some(out_path.display().to_string()))
        .err_file(Some(err_path.display().to_string()))
        .build();
    let mut app = with_selection(info);
    app.update(Msg::Bleats {
        tail: Tail {
            lines: vec![
                line(Stream::Out, "listening on :8080"),
                line(Stream::Err, "warn: retrying upstream"),
            ],
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 1_024,
            note: None,
        },
    });
    app
}

/// One tail line, tagged with the stream it came from.
pub fn line(stream: Stream, text: &str) -> TailLine {
    TailLine {
        stream,
        text: text.to_string(),
    }
}
