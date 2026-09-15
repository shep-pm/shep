//! Renders a `Buffer` to plain text or ANSI, and holds the scene list the
//! pinned snapshots and the gallery share.
//!
//! `docs/lookout/frames.txt` and `docs/lookout/frames.ansi` are generated
//! from this module's output, doubling as a rendered layout reference.
//!
//! Gated at the `mod` declaration with `#[cfg(test)]` rather than
//! `pub mod`: `lib.rs` exposes only three entry points, so an ordinary
//! `pub mod` here is unreachable from outside the crate and fails
//! `dead_code`.

mod build;
mod fixtures;
mod gallery;
mod render;
mod scene;

// Re-exported so `crate::lookout::frames::render_text` keeps resolving for
// its callers elsewhere in `lookout` (dashboard snapshot tests, mostly)
// after this module split `render_text` out into its own file.
use render::render_ansi;
pub use render::render_text;
use scene::Scene;

use build::scene_with;

use std::time::Duration;

use ratatui::buffer::Buffer;

use super::theme::Palette;

/// The gallery's own coloured palette (`xterm-256color`, the deep tier).
/// Every pinned `.snap` test and `docs/lookout/frames.ansi` render through
/// this one, so a real terminal at that tier sees exactly what they pin.
#[must_use]
fn coloured_palette() -> Palette {
    Palette::detect(None, Some(std::ffi::OsStr::new("xterm-256color")), None)
}

/// The flattened `NO_COLOR` palette. `docs/lookout/frames.txt` renders
/// through this one, not the coloured one: a plain-text gallery cannot
/// carry a painted background, so rendering it through the palette an
/// operator with `$NO_COLOR` set actually gets is what makes it an honest
/// picture rather than a coloured frame with the color silently missing.
#[must_use]
fn no_color_palette() -> Palette {
    Palette::detect(Some(std::ffi::OsStr::new("1")), None, None)
}

/// Builds one scene and returns its label with the buffer it drew into.
///
/// Renders at ten minutes of dashboard age, the same age the pinned
/// snapshots and `docs/lookout/frames.ansi` use, through
/// [`coloured_palette`].
#[must_use]
pub fn scene(which: Scene) -> (&'static str, Buffer) {
    (
        which.label(),
        scene_with(which, Duration::from_secs(600), coloured_palette()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame pins, not wire fixtures: re-accepting these after a layout
    /// change is expected, unlike the rule for shep-core's protocol
    /// snapshots.
    ///
    /// `cfg(unix)`: one fixture carries a synthetic signalled exit, and
    /// `signal_label` resolves it against the running platform's table.
    /// Windows never sets a signal on `ExitOutcome`, so this only runs
    /// against a synthetic fixture; the pinned artifacts under
    /// `docs/lookout/` are unix renderings for the same reason.
    ///
    /// Stays in this module rather than travelling with the rest of the
    /// gallery machinery to `gallery.rs`: insta names a pinned snapshot's
    /// file after the call site's `module_path!()`, not its source file, so
    /// moving this test into any new submodule would rename all 58 of these
    /// from `shep__lookout__frames__tests__*` to
    /// `shep__lookout__frames__<mod>__tests__*`. Leaving it in the one
    /// module this whole split never touches, `frames::tests` itself, is
    /// the only placement that renames none of them.
    #[cfg(unix)]
    #[test]
    fn frames_are_pinned() {
        for which in Scene::ALL {
            let (label, buffer) = scene(*which);
            insta::assert_snapshot!(label, render_text(&buffer));
        }
    }
}
