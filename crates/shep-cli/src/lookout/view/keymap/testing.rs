//! Fixtures and helpers shared by this module's tests.

use super::super::super::app::{App, Control};
use super::super::super::keymap::Group;
use super::super::super::theme::Palette;
use super::super::overlay;
use super::keymap_rows::INTERIOR;
use crate::lookout::app::{KeyPress, Msg};
use crate::lookout::frames::render_text;
use crate::lookout::view::fixtures;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use shep_core::protocol::ProcessInfo;
use shep_core::status::ProcStatus;

/// The boxed form's own top border row: the top-left corner, `INTERIOR`
/// copies of the top glyph, and the top-right corner, run together with
/// no gap. A full-width run, not a single glyph: five of the eight
/// border glyphs (`▛ ▜ ▙ ▟ ▀`) also appear in the sheep's own art
/// (`SHEEP[2]` alone carries `▜`, `▀` and `▛`), so a check for any one
/// of them in isolation would pass or fail on the sheep's presence
/// rather than the border's.
pub(super) fn top_border_row() -> String {
    format!(
        "{}{}{}",
        overlay::BOX_TOP_LEFT,
        overlay::BOX_TOP.to_string().repeat(usize::from(INTERIOR)),
        overlay::BOX_TOP_RIGHT
    )
}

/// Renders one overlay and returns the screen as text.
pub(super) fn render_overlay(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| super::super::draw(app, frame))
        .expect("draw");
    render_text(terminal.backend().buffer())
}

/// Every drawn row's own sentence is in `rendered`, `Group::Closing`
/// excepted since it draws on its own line rather than in a column.
/// `context` names what the caller was checking, for the panic.
pub(super) fn assert_every_visible_row_drawn(rendered: &str, context: &str) {
    for row in crate::lookout::keymap::rows() {
        if row.group == Group::Closing {
            continue;
        }
        assert!(
            rendered.contains(row.does),
            "{context}, but `{}` is missing: {rendered}",
            row.does
        );
    }
}

/// A healthy dashboard, control open, nothing frozen.
pub(super) fn healthy_app() -> App {
    healthy_app_with_palette(fixtures::plain())
}

/// The same, at `palette`: what the ground test reads, since
/// [`Palette::ground`] is a no-op under [`fixtures::plain`].
pub(super) fn healthy_app_with_palette(palette: Palette) -> App {
    let mut app = fixtures::app_with(
        vec![
            ProcessInfo::builder(1, "web", ProcStatus::Online)
                .pid(Some(48_001))
                .build(),
        ],
        palette,
    );
    app.set_control_for_tests(Control::Allowed);
    app
}

/// A healthy dashboard with the overlay up.
pub(super) fn app_with_overlay() -> App {
    let mut app = healthy_app();
    let _ = app.update(Msg::Key(KeyPress::Help));
    // Asserted here, on all five of these helpers, not left to the
    // caller: a dozen tests render from this and assert on headings, so
    // an overlay that failed to open would reach each one as "no
    // heading row" instead of naming the actual cause.
    assert!(app.keymap_open(), "the overlay did not open");
    app
}

/// The same, in colour, for the one test that needs an actual
/// background to check against.
pub(super) fn coloured_app_with_overlay() -> App {
    let mut app = healthy_app_with_palette(fixtures::coloured());
    let _ = app.update(Msg::Key(KeyPress::Help));
    assert!(app.keymap_open(), "the overlay did not open");
    app
}

/// The same, `--read-only`, with the overlay up.
pub(super) fn read_only_app_with_overlay() -> App {
    let mut app = healthy_app();
    app.set_control_for_tests(Control::ReadOnly);
    let _ = app.update(Msg::Key(KeyPress::Help));
    assert!(app.keymap_open(), "the overlay did not open");
    app
}

/// The same, with the link gone, with the overlay up. `Msg::Frozen` is
/// how `frames.rs`'s own `Scene::Frozen` raises the same state.
pub(super) fn frozen_app_with_overlay() -> App {
    let mut app = healthy_app();
    let _ = app.update(Msg::Frozen {
        at_local: "2026-08-14 14:32:07".to_string(),
        why: fixtures::FROZEN_WHY.to_string(),
    });
    let _ = app.update(Msg::Key(KeyPress::Help));
    assert!(app.keymap_open(), "the overlay did not open");
    app
}

/// The same, `--read-only` as well: the design's own table gives the
/// link-lost text for `Link::Lost` under either `Control`, so this is
/// what tells `Link::Lost` outranking `Control::Allowed` apart from
/// `Link::Lost` outranking `Control` altogether.
pub(super) fn frozen_read_only_app_with_overlay() -> App {
    let mut app = healthy_app();
    app.set_control_for_tests(Control::ReadOnly);
    let _ = app.update(Msg::Frozen {
        at_local: "2026-08-14 14:32:07".to_string(),
        why: fixtures::FROZEN_WHY.to_string(),
    });
    let _ = app.update(Msg::Key(KeyPress::Help));
    assert!(app.keymap_open(), "the overlay did not open");
    app
}
