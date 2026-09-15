//! The gallery text `docs/lookout/frames.txt` and `docs/lookout/frames.ansi`
//! pin, and the writer that regenerates both.

use std::time::Duration;

use super::Scene;
use super::{coloured_palette, no_color_palette, render_ansi, scene_with};

/// The header both gallery files open with.
///
/// Not a doc comment on the test: this text is read by a person opening
/// `docs/lookout/frames.txt` with no context at all, and it is the only
/// place that says where those frames came from.
pub(super) const GALLERY_PREAMBLE: &str = "shep lookout frames
===================

These are real frames, rendered headlessly through ratatui's TestBackend by

    cargo test -p shep --lib --all-features -- --ignored write_the_gallery

Nothing here is a mockup.

frames.ansi renders all fifty-eight scenes through the same coloured
palette the pinned `.snap` tests use; read it with `less -R`. frames.txt
renders the same fifty-eight scenes through the flattened NO_COLOR palette
instead, the one an operator with $NO_COLOR set or a 16-colour terminal
actually gets. The two files are deliberately different pictures of the
same dashboard, not one file with the colour removed.

All four panes are here: the flock table (the spine), the host-usage strip,
the sheep detail pane and the bleats feed. The selected row is a painted
gutter in frames.ansi; in frames.txt it falls back to a `>` marker, since
the NO_COLOR palette has no ground to paint with. Every pane below the
table describes whatever that row is: one sheep usually, and a rollup with
no single log where the cursor sits on a group or a fold header.

The feed reads the selected sheep's log files from disk and re-reads them with
each flock listing. It is not a live subscription, and it says so on its own
header line: `out then err` because the two files are shown end to end with no
interleaving, and `re-read with each listing` because a two-second gap in this
pane is the refresh, not the sheep.

When the pane cannot show everything, the header says what went instead. Lines
it read and dropped are counted exactly; bytes below its 64 KiB window were
never read at all, so those are reported in bytes, because nothing counted the
lines in them and guessing would be worse than saying so.

Four frames show the editing pane, `e` from the dashboard, on a sheep's own
row: fresh at the 160x48 design target, the same pane with two edits filed
(one of them needing a respawn), and the same fresh pane at 120 and at 88
columns, where the explanation panel and the LANDS column trade places as
the width falls.

The last eight are the keymap overlay, `h` or `?` from any body. Boxed and
centred at the 160x48 target, at 130 where the border only just fits,
borderless at 129 one column below that floor, at 100 where DOING drops to a
bank of its own, and at 70 where two columns are all that fit. Three more
change what the bottom line says rather than the layout: a frozen dashboard,
a read-only one, and a terminal too short for the box at all.

Before those, four show the close dialog `esc` raises over the editing pane
when a change is filed the running child cannot take. Boxed and centred at the 160x48
target, the same at 90 where the border only just fits, borderless at 89 one
column below its floor, and the parked-only heading on a sheep whose fields
were written before the pane was opened.

Before those are the four sheep-pane scenes, `↵` on a sheep, then the
secrets pane, `S`, and before them the full-screen bleats pane, `b` from
the dashboard. The seven before
that are the settings screen, `s` from the dashboard. It owns the whole body
between the title and the status bar rather than sharing it
with the flock table, so a fresh $SHEP_HOME, some scalars declared, an armed
confirm, the socket editor mid-type, the dogs table's own drift, the same
screen at 45 columns and the same screen too short to hold every row each get
a frame of their own.
";

#[cfg(test)]
mod tests {
    use super::super::render_text;
    use super::*;

    /// The two gallery files' text: plain, then ANSI.
    ///
    /// Separate from the writer so a non-ignored test can read it.
    fn gallery_text() -> (String, String) {
        let mut plain = String::from(GALLERY_PREAMBLE);
        let mut ansi = String::from(GALLERY_PREAMBLE);
        for which in Scene::ALL {
            let (width, height) = which.size();
            let heading = format!(
                "\n\n=== {}  ({width}x{height}) ===\n{}\n\n",
                which.label(),
                which.caption()
            );
            // Two separate renders, not one buffer read twice: `frames.txt`
            // is what a `NO_COLOR` operator sees, and that palette has no
            // painted ground for the gutter to fall back from, so the
            // buffer itself differs, not just how it prints.
            let plain_buffer = scene_with(*which, Duration::from_secs(600), no_color_palette());
            plain.push_str(&heading);
            plain.push_str(&render_text(&plain_buffer));
            let ansi_buffer = scene_with(*which, Duration::from_secs(600), coloured_palette());
            ansi.push_str(&heading);
            ansi.push_str(&render_ansi(&ansi_buffer));
        }
        (plain, ansi)
    }

    /// The committed gallery matches what the generator would write today.
    ///
    /// `write_the_gallery` is `#[ignore]`d, so nothing in the ordinary
    /// suite catches a scene that landed without also running it: its own
    /// doc names the exact failure, both files still saying fifty scenes
    /// and holding none of the eight keymap scenes that shipped alongside
    /// them. Diffing against fresh output, rather than only counting
    /// headings, catches a mismatch a count would miss too, a caption
    /// edited without a corresponding regeneration.
    ///
    /// `cfg(unix)`: same reason as `frames_are_pinned` above. `signal_label`
    /// resolves a fixture's signal against the running platform's own
    /// table, and the committed artifacts under `docs/lookout/` are unix
    /// renderings, so a Windows-built binary regenerates the `cron` row
    /// reading a bare `15` where the committed file reads `SIGTERM`.
    #[cfg(unix)]
    #[test]
    fn the_committed_gallery_matches_what_the_generator_would_write() {
        let dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/lookout"));
        let (plain, ansi) = gallery_text();
        for (name, want) in [("frames.txt", &plain), ("frames.ansi", &ansi)] {
            let have = std::fs::read_to_string(dir.join(name)).unwrap_or_else(|e| {
                panic!(
                    "{name} is missing or unreadable ({e}); run \
                     cargo test -p shep --lib --all-features -- --ignored write_the_gallery"
                )
            });
            assert_eq!(
                &have, want,
                "{name} is stale against the current scenes; run \
                 cargo test -p shep --lib --all-features -- --ignored write_the_gallery"
            );
        }
    }

    #[test]
    fn the_gallery_carries_no_dashes() {
        let (plain, ansi) = gallery_text();
        for (file, text) in [("frames.txt", &plain), ("frames.ansi", &ansi)] {
            assert!(!text.contains('\u{2014}'), "em dash in {file}");
            assert!(!text.contains('\u{2013}'), "en dash in {file}");
        }
    }

    /// Writes `docs/lookout/frames.txt` and `docs/lookout/frames.ansi`.
    ///
    /// `#[ignore]`: writes into the repository, so it only runs on request.
    ///
    /// ```text
    /// cargo test -p shep --lib --all-features -- --ignored write_the_gallery
    /// ```
    ///
    /// A layout change cannot rot these files unnoticed: they render the
    /// same `Scene::ALL` the pinned snapshots read, so the ordinary suite
    /// reddens first and whoever fixes it comes back here.
    ///
    /// **Adding a scene without running this one is caught too, now.**
    /// `the_committed_gallery_matches_what_the_generator_would_write`
    /// diffs the committed files against fresh output, catching a scene
    /// missing from both while their own preamble still carries the old
    /// count. Run this anyway, in the same commit that adds a scene: a
    /// missing regeneration failing there is a one-line fix, failing in
    /// the other test means reading a full-file diff to find it.
    #[test]
    #[ignore = "writes into docs/lookout; run it deliberately"]
    fn write_the_gallery() {
        // Absolute, derived from the manifest, so it lands in the same
        // place whatever directory the run started in.
        let dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/lookout"));
        std::fs::create_dir_all(dir).unwrap();

        let (plain, ansi) = gallery_text();
        std::fs::write(dir.join("frames.txt"), &plain).unwrap();
        std::fs::write(dir.join("frames.ansi"), &ansi).unwrap();

        // A live assertion, not a `timeout`: this function is synchronous,
        // so a `tokio::time::timeout` around it would complete on its first
        // poll and bound nothing at all. What can actually go wrong here is
        // a scene rendering empty, and that is what these two check.
        assert!(
            plain.lines().count() > 100,
            "every scene in the gallery is more than a hundred lines together"
        );
        assert_eq!(plain.matches("=== ").count(), Scene::ALL.len());
    }
}
