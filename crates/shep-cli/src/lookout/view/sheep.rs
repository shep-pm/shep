//! The sheep pane: one sheep given the whole screen.
//!
//! [`super::mod`]'s own `draw` spends this module's whole area on the
//! identity band and leaves the rest blank: Tasks 8 through 10 draw the
//! histories, the read-only config column and the feed there.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Span;

use super::super::app::{App, RowKey};
use super::super::pane_sheep::SheepPane;
use super::detail;

/// Draws the pane into `area`: the identity band on its first row, and
/// nothing else yet.
///
/// `area` is the whole pane body, under the title band [`super::mod`]'s own
/// `draw` already painted and over the status bar it paints after this
/// returns — not a sub-rect of either, the way every other full-screen
/// pane's own `draw` is handed one.
pub fn draw(app: &App, pane: &SheepPane, area: Rect, buffer: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let palette = app.palette();
    // `App::sheep_pane_row`, not `App::selected_row`: the pinned sheep, not
    // the selection. `Msg::Snapshot` reseats the selection whatever screen
    // is showing, so a pane pinned to a sheep that then leaves the flock
    // would otherwise draw its neighbour's facts under a band still naming
    // the first.
    let mut spans = match app.sheep_pane_row() {
        Some(row) => detail::identity_line(app, row, area.width, palette).spans,
        None => {
            let RowKey::Sheep(id) = pane.sheep() else {
                unreachable!("a sheep pane is only ever opened on `RowKey::Sheep`")
            };
            vec![Span::styled(
                // Same phrasing `view::bleats_full`'s own title uses for the
                // same case: one sentence for "this sheep is gone", not two.
                format!("sheep {id}: it is no longer in the flock"),
                palette.muted(),
            )]
        }
    };
    // Only the read-only pane's own pending count, not
    // `ProcessInfo::pending`'s: `detail_lines` already folds that one into
    // its own `cfg` cell, and this is the confirmation that the config this
    // pane is showing agrees with it, once the read has landed.
    if let Some(pending) = pane
        .config()
        .map(|view| view.pending.len())
        .filter(|count| *count > 0)
    {
        spans.push(Span::styled(
            format!("   !{pending} pending"),
            app.palette().attention(),
        ));
    }
    let line = ratatui::text::Line::from(spans);
    buffer.set_line(area.x, area.y, &line, area.width);
    // Rows 2 to 46 (the identity band's own row plus one) stay blank here;
    // Tasks 8 through 10 fill them.
}

#[cfg(test)]
mod tests {
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use super::super::super::app::{Body, KeyPress, Msg};
    use super::super::super::frames::render_text;
    use super::super::fixtures;
    use super::*;

    /// The bug the reviewer reproduced: pane pinned to sheep 1, sheep 1
    /// deleted, `pane.sheep()` still reads `Sheep(1)` while `app.selected()`
    /// has moved to `Sheep(2)` (`alpha`'s alphabetical neighbour, `bravo`,
    /// the only row left once the reseat runs). Reading `App::selected_row`
    /// here would draw `bravo`'s facts under a band still naming `alpha`.
    #[test]
    fn the_band_does_not_draw_the_sheep_that_replaced_the_pinned_one() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(1, "alpha", ProcStatus::Online).build(),
                ProcessInfo::builder(2, "bravo", ProcStatus::Online).build(),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "setup: the pane opened on alpha"
        );
        let _ = app.update(Msg::Snapshot {
            rows: vec![ProcessInfo::builder(2, "bravo", ProcStatus::Online).build()],
            at: std::time::Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(2)),
            "setup: the reseat moved the selection to bravo"
        );

        let Body::Sheep(pane) = app.body() else {
            panic!("the pane is still open");
        };
        let area = Rect::new(0, 0, 80, 3);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        let text = render_text(&buffer);
        assert!(
            !text.contains("bravo"),
            "must not draw the sheep that replaced the pinned one: {text:?}"
        );
        assert!(
            text.contains("sheep 1: it is no longer in the flock"),
            "got {text:?}"
        );
    }
}
