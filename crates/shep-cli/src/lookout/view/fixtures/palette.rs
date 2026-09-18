//! The palettes and styles the other fixtures draw through.

use std::ffi::OsStr;

use ratatui::style::Style;

use crate::lookout::theme::Palette;

/// No colour at all: the palette every fixture uses unless the test is about
/// colour.
pub fn plain() -> Palette {
    Palette::detect(None, None, None)
}

/// The 256-colour palette, for the two tests that assert on a specific
/// foreground.
pub fn coloured() -> Palette {
    Palette::detect(None, Some(OsStr::new("xterm-256color")), None)
}

/// The palette `NO_COLOR` selects: no ink anywhere, so the mute pass's own
/// second call (`palette.muted()`) is a no-op.
pub fn no_color() -> Palette {
    Palette::detect(Some(OsStr::new("1")), None, None)
}

/// The style [`plain`]'s ink leaves a cell in once the mute pass has run:
/// the pane's own reset-then-muted sequence, replayed here so a fixture
/// never has to agree with a colour literal in `theme.rs` by coincidence.
pub fn plain_dimmed() -> Style {
    Style::reset().patch(plain().muted())
}
