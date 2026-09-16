//! Fixtures and helpers shared by this module's tests.

use super::super::app::App;
use super::paint_frame::draw;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

pub(super) fn draw_to(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(app, frame)).unwrap();
    crate::lookout::frames::render_text(terminal.backend().buffer())
}
