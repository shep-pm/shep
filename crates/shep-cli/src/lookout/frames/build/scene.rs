//! [`scene_with`]: the walk from a bare [`App`] to a drawn buffer.

use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::lookout::app::{App, Msg};
use crate::lookout::frames::scene::Scene;
use crate::lookout::theme::Palette;
use crate::lookout::view::{body_rows, draw};

use super::{armed, flock, link, prepare};

/// One scene, `age` after its opening snapshot, drawn through `palette`.
///
/// Deterministic: a forced palette, an explicit `Instant` advanced by exact
/// `Duration`s, and a literal frozen timestamp, so the gallery never
/// depends on this machine's clock or environment.
///
/// `palette` exists so the gallery can render the same scene twice: once
/// through [`super::super::coloured_palette`] for `docs/lookout/frames.ansi` and the
/// pinned snapshots, and once through [`super::super::no_color_palette`] for
/// `docs/lookout/frames.txt`.
///
/// `age` exists for
/// `the_frozen_frame_does_not_move_however_long_the_link_stays_gone`,
/// which renders the frozen scene at two ages and checks for identical
/// frames.
#[must_use]
pub fn scene_with(which: Scene, age: Duration, palette: Palette) -> Buffer {
    let t0 = Instant::now();
    let mut app = App::new(palette, which.control(), "/home/ada/.shep".to_string(), t0);

    let rows = flock::build_flock(&mut app, which, t0);
    flock::initialize_flock(&mut app, which, rows, t0);

    prepare::prepare_scene(&mut app, which);
    link::apply_live_updates(&mut app, which);
    link::apply_connection_state(&mut app, which, age);

    app.update(Msg::Tick { now: t0 + age });

    armed::apply_actions(&mut app, which, t0);
    armed::apply_post_tick_scene(&mut app, which);

    render_scene(&mut app, which)
}

/// Draws `app` at the size `which` asks for, the way `run_ui` does.
fn render_scene(app: &mut App, which: Scene) -> Buffer {
    let (width, height) = which.size();
    // The same call `run_ui` makes before every draw. Without it
    // `Viewport::rows` stays zero, which means unlimited, so a guard on a
    // scrolled screen never triggers.
    app.note_body_rows(body_rows(Rect::new(0, 0, width, height)));
    app.note_body_width(width);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(app, frame)).unwrap();
    terminal.backend().buffer().clone()
}

#[cfg(test)]
mod tests {
    use crate::lookout::frames::render::render_text;
    use crate::lookout::frames::scene;

    use super::*;

    /// Each scene whose whole point is its geometry drops the panes that
    /// geometry cannot hold, and says so rather than clipping.
    #[test]
    #[cfg(unix)] // inherited, not measured per test: see the `build` module's docs
    fn every_size_scene_drops_the_panes_its_geometry_cannot_hold() {
        // Empty: each of the three panes gives its own reason.
        let empty = render_text(&scene(Scene::Empty).1);
        assert!(
            empty.contains("the flock is empty"),
            "the table's own sentence"
        );
        assert!(
            empty.contains("no sheep selected: the flock is empty"),
            "the detail pane's"
        );
        assert!(empty.contains("BLEATS no sheep is selected"), "the feed's");
        // The summary sits after both host readings now, which puts it past
        // the cut at this scene's 100 columns. Asserted as absent rather
        // than dropped: this is the visible cost of grouping the machine's
        // two numbers together, and a later reorder that brings it back
        // should have to come through here and say so. The empty-flock
        // behaviour it used to check is pinned at the unit level, in
        // `host::tests::a_flock_with_no_readings_shows_a_dash_and_not_a_zero`.
        assert!(
            !empty.contains("errored"),
            "the summary is past a 100-column cut once host memory precedes it"
        );

        // Narrow: 51 columns drops FOLD, EXIT, RESTARTS, PID and MEM but
        // keeps CPU and UPTIME.
        let narrow = render_text(&scene(Scene::Narrow).1);
        assert!(narrow.contains("CPU") && narrow.contains("UPTIME"));
        for gone in ["FOLD", "EXIT", "RESTARTS", "PID", "MEM"] {
            assert!(!narrow.contains(gone), "the narrow tier dropped {gone}");
        }
        assert!(narrow.contains("host  load"), "the strip is up at 14 rows");
        assert!(!narrow.contains("BLEATS"), "the feed is not");
        assert!(
            !narrow.contains("SHEEP 0  "),
            "and neither is the detail pane"
        );

        // TooNarrow: below the floor, refuses rather than overlapping.
        let too_narrow = render_text(&scene(Scene::TooNarrow).1);
        let mut lines = too_narrow.lines();
        assert_eq!(lines.next().unwrap().trim_end(), "too small");
        assert_eq!(lines.next().unwrap().trim_end(), "need 33x6");

        // NoDetail: the detail pane is the first to go at 20 rows.
        let no_detail = render_text(&scene(Scene::NoDetail).1);
        assert!(
            no_detail.contains("BLEATS api"),
            "the feed stayed, on the selection"
        );
        assert!(no_detail.contains("host  load"), "and so did the strip");
        // The log-path prefix is the detail pane's alone: the feed's body
        // lines are tagged `out  ` too, but carry log text, not a path.
        assert!(
            !no_detail.contains("out  /home/ada/.shep/logs/"),
            "the detail pane went"
        );

        // TableOnly: 12 rows, no optional panes.
        let table_only = render_text(&scene(Scene::TableOnly).1);
        assert!(!table_only.contains("host  load"));
        assert!(!table_only.contains("BLEATS"));
        assert!(table_only.contains("STATUS"), "the table is still there");

        // Cramped: 33 columns, the narrowest terminal that draws.
        let cramped = render_text(&scene(Scene::Cramped).1);
        assert!(cramped.contains('…'), "something truncated, visibly");
        // Not a row-width check, which `render_text` satisfies trivially:
        // "nothing overlaps" means each pane's marker appears exactly once.
        // `contains`, not `starts_with`: the `BLEATS` chip now leads that
        // row.
        for marker in ["host  ", "BLEATS"] {
            assert_eq!(
                cramped.lines().filter(|line| line.contains(marker)).count(),
                1,
                "{marker:?} appears once at 33 columns"
            );
        }
        // The detail pane's merged log row carries no chip of its own, so
        // `starts_with` still works, but the path itself can truncate away
        // at 33 columns; the divider is what survives to identify the row
        // instead.
        assert_eq!(
            cramped
                .lines()
                .filter(|line| line.starts_with("out  ") && line.contains('\u{2502}'))
                .count(),
            1,
            "the detail pane's merged log row appears once at 33 columns"
        );
        assert!(
            cramped.lines().last().unwrap().contains("control enabled"),
            "and the status bar is still the last row"
        );
    }

    /// A pinned width whose own arithmetic no longer holds drops a column
    /// in silence, and this is the scene whose whole point is what the
    /// columns look like once the shepherd is gone.
    ///
    /// The floor is `ALL`'s own tier threshold plus the gutter, not the
    /// design's 160: the frame is drawn twelve columns wider than it has
    /// to be, and pinning the wider number would fail for a scene that was
    /// still drawing everything.
    #[test]
    fn the_frozen_scene_draws_every_column() {
        use crate::lookout::view::flock::{GUTTER, columns_for};

        let (width, _) = Scene::Frozen.size();
        assert_eq!(
            columns_for(width - GUTTER).len(),
            columns_for(u16::MAX).len(),
            "the frozen scene is {width} columns, which drops a column from the widest set"
        );
    }
}
