//! Renders a `Buffer` to plain text or to ANSI, for the gallery and for the
//! pinned snapshots.

use std::fmt::Write as _;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

/// One rendered buffer as plain text: one line per row, trailing spaces
/// kept, no escapes.
///
/// Trailing spaces stay because a frame is a fixed-size grid: trimming
/// would make a right-aligned cell look like it moved.
///
/// Cells are read by their rendered symbol, not by byte length, so a
/// multi-byte cell round-trips exactly as drawn. Indexed with
/// `Buffer[(x, y)]`, not the deprecated `Buffer::get`.
#[must_use]
pub fn render_text(buffer: &Buffer) -> String {
    let area = buffer.area;
    (0..area.height)
        .map(|row| {
            (0..area.width)
                .map(|col| buffer[(area.x + col, area.y + row)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The same buffer with SGR escapes, for reading through `less -R`.
///
/// Every line ends with a reset before its newline, so an unreset colour
/// does not bleed into the rest of the file. Reset is unconditional per
/// cell change, not incremental: a cell that sets nothing still clears
/// whatever the previous cell set, so a band or a painted ground cannot
/// bleed along the rest of the row.
#[must_use]
pub fn render_ansi(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for row in 0..area.height {
        let mut current = String::new();
        for col in 0..area.width {
            let cell = &buffer[(area.x + col, area.y + row)];
            let wanted = sgr(cell.fg, cell.bg, cell.modifier);
            if wanted != current {
                out.push_str("\u{1b}[0m");
                out.push_str(&wanted);
                current = wanted;
            }
            out.push_str(cell.symbol());
        }
        out.push_str("\u{1b}[0m");
        out.push('\n');
    }
    out
}

/// The SGR sequence for one cell's foreground, background and reverse
/// video.
///
/// Foreground, background and `REVERSED` are the only styles any scene
/// uses (`no_scene_uses_a_modifier_the_ansi_renderer_cannot_render` still
/// guards against a scene reaching for one this function does not draw).
/// This is what makes the selected row's painted ground and the title and
/// section bands' reverse video visible in `docs/lookout/frames.ansi`
/// rather than under-representing both.
fn sgr(fg: Color, bg: Color, modifier: Modifier) -> String {
    let mut out = String::new();
    if modifier.contains(Modifier::REVERSED) {
        out.push_str("\u{1b}[7m");
    }
    match fg {
        Color::Reset => {}
        Color::Indexed(index) => {
            let _ = write!(out, "\u{1b}[38;5;{index}m");
        }
        Color::Red => out.push_str("\u{1b}[31m"),
        Color::Green => out.push_str("\u{1b}[32m"),
        Color::Yellow => out.push_str("\u{1b}[33m"),
        Color::DarkGray => out.push_str("\u{1b}[90m"),
        _ => {}
    }
    match bg {
        Color::Reset => {}
        Color::Indexed(index) => {
            let _ = write!(out, "\u{1b}[48;5;{index}m");
        }
        Color::Red => out.push_str("\u{1b}[41m"),
        Color::Green => out.push_str("\u{1b}[42m"),
        Color::Yellow => out.push_str("\u{1b}[43m"),
        Color::DarkGray => out.push_str("\u{1b}[100m"),
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    use super::super::scene;
    use super::super::scene::Scene;
    use super::*;

    /// `sgr`/`render_ansi` were foreground-only before this task: a band's
    /// reverse video and a painted background both came out unstyled.
    #[test]
    fn the_ansi_dump_emits_a_bands_reverse_video_and_a_grounds_background() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
        buffer[(0, 0)].set_style(Style::default().add_modifier(Modifier::REVERSED));
        buffer[(1, 0)].set_style(Style::default().bg(Color::Indexed(235)));
        let dump = render_ansi(&buffer);
        assert!(dump.contains("\u{1b}[7m"), "reverse video");
        assert!(dump.contains("\u{1b}[48;5;235m"), "an indexed background");
    }

    /// Nine other tests read frames through this renderer: a regression
    /// here silently changes what they assert.
    #[test]
    fn the_plain_renderer_is_one_line_per_row_and_no_escapes() {
        let text = render_text(&scene(Scene::HealthyWide).1);
        assert_eq!(text.lines().count(), 30);
        assert!(!text.contains('\u{1b}'), "plain means plain");
        for line in text.lines() {
            assert_eq!(line.chars().count(), 120, "every row is the full width");
        }
    }

    /// An unreset colour bleeds into whatever prints next, which for a
    /// file read through `less -R` is the rest of the file.
    #[test]
    fn the_ansi_renderer_colours_the_errored_row_and_always_resets() {
        let ansi = render_ansi(&scene(Scene::Errored).1);
        assert!(
            ansi.contains("\u{1b}[38;5;166m"),
            "bark, on the errored status"
        );
        for line in ansi.lines() {
            assert!(
                line.is_empty() || line.ends_with("\u{1b}[0m"),
                "every line resets before its newline"
            );
        }
    }
}
