//! Drawing a dashboard and reading rows back off it.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::text::Line;

use crate::lookout::app::App;

/// One rendered line, styles discarded.
pub fn rendered(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// Several rendered lines, newline-joined. Newline-joined and not
/// concatenated, so an assertion can anchor on a line boundary.
pub fn render_all(lines: &[Line<'static>]) -> String {
    lines.iter().map(rendered).collect::<Vec<_>>().join("\n")
}

/// The one line in `lines` starting with `prefix`, after trimming leading
/// whitespace: what a close dialog's own option-row tests read, so a test
/// for the reload row does not pass off the first row that merely
/// contains the letter somewhere in its sentence.
///
/// # Panics
///
/// Panics if no line starts with `prefix`, which is a fixture bug rather
/// than a failure the test is about.
#[track_caller]
pub fn row_starting_with(lines: &[Line<'static>], prefix: &str) -> String {
    lines
        .iter()
        .map(rendered)
        .find(|line| line.trim_start().starts_with(prefix))
        .unwrap_or_else(|| panic!("no row starts with {prefix:?}"))
}

/// The row in `buffer` containing `needle`.
///
/// # Panics
///
/// Panics if no row contains `needle`, a fixture bug rather than a failure
/// the test is about.
#[track_caller]
pub fn row_containing(buffer: &Buffer, needle: &str) -> String {
    rows_of(buffer)
        .into_iter()
        .find(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("no row contains {needle:?}"))
}

/// A frame already drawn, at `width` x `height`: every secrets-pane test
/// reads cells straight off this rather than the `String` `render_text`
/// gives, since a column offset only means something against the buffer it
/// came from.
pub fn render(app: &App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::lookout::view::draw(app, frame))
        .unwrap();
    terminal.backend().buffer().clone()
}

/// `buffer` as one `String` per row, for a `contains` search across whatever
/// line carries the text (a group header, say) rather than one column.
pub fn rows_of(buffer: &Buffer) -> Vec<String> {
    crate::lookout::frames::render_text(buffer)
        .lines()
        .map(str::to_string)
        .collect()
}
