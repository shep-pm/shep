//! The sheep pane: one sheep given the whole screen.
//!
//! [`super::mod`]'s own `draw` spends this module's whole area on the
//! identity band and leaves the rest blank: Tasks 8 through 10 draw the
//! histories, the read-only config column and the feed there.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Span;

use super::super::app::App;
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
    let mut spans = detail::detail_lines(app, area.width)
        .into_iter()
        .next()
        .map(|line| line.spans)
        .unwrap_or_default();
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
