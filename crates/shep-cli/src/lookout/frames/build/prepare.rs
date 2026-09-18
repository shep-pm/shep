//! What the operator had already done when the frame was taken: a row
//! picked, a pane opened, an overlay raised, a filter typed.
//!
//! Everything here runs BEFORE `scene_with`'s tick. State whose own
//! expiry is shorter than the 600s the gallery renders at has to wait
//! until after it, and lives in [`super::armed`] instead.

use shep_core::protocol::Response;

use crate::lookout::app::{App, KeyPress, Msg, Sent};
use crate::lookout::frames::fixtures::{
    select_fold, select_group, select_id, sheep_pane_config_view,
};
use crate::lookout::frames::scene::Scene;

/// Replays the operator's own keys and selections onto `app`.
pub(super) fn prepare_scene(app: &mut App, which: Scene) {
    // Every `Keymap*` scene uses the healthy flock fixture `super::flock`
    // falls through to, and raises the overlay right after: `h`
    // toggles `App::keymap_open` and nothing else, so it does not matter
    // that this runs ahead of the selection, the host sample or (for
    // `KeymapFrozen`) the freeze `super::link` applies.
    if which.is_keymap() {
        app.update(Msg::Key(KeyPress::Help));
    }

    // Selects `api` (id 2) so the panes below describe a fixed sheep,
    // walked by id since the table sorts by name. Skipped where there is
    // no flock, no pane below the table, or the cursor belongs elsewhere
    // (`Grouped`, `Folds`, and the three settings scenes with no id 2).
    if !matches!(
        which,
        Scene::Empty
            | Scene::Narrow
            | Scene::TooNarrow
            | Scene::TableOnly
            | Scene::Grouped
            | Scene::Folds
            | Scene::WithDogs
            | Scene::MemCeiling
            | Scene::CfgDrift
            | Scene::SettingsDogs
            | Scene::SettingsNarrow
            | Scene::SettingsShort
            | Scene::SheepPane
            | Scene::SheepPaneCpuOnly
            | Scene::SheepPaneSparklines
            | Scene::SheepPaneShort
    ) {
        select_id(app, 2);
    }

    // Onto `web`'s group header, which is the one row in the gallery that is
    // not a sheep. Every pane below the table renders its own group state
    // from it: the rollup line, the per-instance lamb sentence, and the
    // feed's refusal to pick an instance to tail.
    if which == Scene::Grouped {
        select_group(app, "web");
    }

    // `Folds` presses `F` to switch the table into the fold view, collapses
    // `batch` (the smallest fold, so `z`'s effect is visible without hiding
    // much), and parks the cursor on `edge`'s own header, the fold that
    // carries the grouped app.
    if which == Scene::Folds {
        app.update(Msg::Key(KeyPress::FoldView));
        select_fold(app, "batch");
        app.update(Msg::Key(KeyPress::Collapse));
        select_fold(app, "edge");
    }

    // `LambsUnknown` wants `cron`, id 4, instead.
    if which == Scene::LambsUnknown {
        select_id(app, 4);
    }

    // `WithDogs` parks on the silent adopted dog, id 91, the row this
    // scene exists to show.
    if which == Scene::WithDogs {
        select_id(app, 91);
    }

    // `MemCeiling` parks on `web-hot`, id 1, the row at 94% of its
    // ceiling: the butter warning this scene exists to show.
    if which == Scene::MemCeiling {
        select_id(app, 1);
    }

    // `CfgDrift` parks on `web`, id 10, the row carrying the pending
    // fields: the detail pane's own `cfg !2 pending` cell only renders
    // for the selected sheep.
    if which == Scene::CfgDrift {
        select_id(app, 10);
    }

    // Every sheep-pane scene parks on `web`, id 20 (the only row in its
    // own flock), opens the pane the same way the event loop does (`Enter`
    // on the selected row), and answers the config request it fires so the
    // config column has real fields to draw rather than "reading config…".
    if matches!(
        which,
        Scene::SheepPane
            | Scene::SheepPaneCpuOnly
            | Scene::SheepPaneSparklines
            | Scene::SheepPaneShort
    ) {
        select_id(app, 20);
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(sheep_pane_config_view()))),
        });
    }

    match which {
        Scene::FilterEditing | Scene::FilterNoMatch | Scene::FilterActive => {
            app.update(Msg::Key(KeyPress::FilterStart));
            let query = if which == Scene::FilterNoMatch {
                "zzz"
            } else {
                "web"
            };
            for typed in query.chars() {
                app.update(Msg::Key(KeyPress::TextChar(typed)));
            }
            if which == Scene::FilterActive {
                app.update(Msg::Key(KeyPress::TextApply));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use crate::lookout::frames::render::render_text;
    use crate::lookout::frames::scene;
    use crate::lookout::keymap::Group;

    use super::*;

    /// Each filter scene shows the query it is named for, at the stage of
    /// typing it is named for.
    #[test]
    #[cfg(unix)] // inherited, not measured per test: see the `build` module's docs
    fn every_filter_scene_shows_the_query_it_is_named_for() {
        // FilterEditing: mid-type, table already narrowed.
        let editing = render_text(&scene(Scene::FilterEditing).1);
        assert_eq!(
            editing
                .lines()
                .filter(|line| line.contains("  web  "))
                .count(),
            2,
            "two rows survived the query"
        );
        assert!(!editing.contains("billing"), "and the rest did not");
        assert!(editing.contains("2 of 6 in the flock"), "got {editing:?}");
        assert!(
            editing.contains("filter  web\u{258f}"),
            "the query and the cursor"
        );
        for named in ["enter applies", "esc cancels", "ctrl-c quits"] {
            assert!(editing.contains(named), "the box names {named}");
        }

        // FilterActive: the same query, box closed.
        let active = render_text(&scene(Scene::FilterActive).1);
        assert!(active.contains("filter \"web\""), "the box is closed");
        assert!(!active.contains("enter applies"), "and its keys are gone");
        assert!(active.contains("2 of 6 in the flock"), "still narrowed");
        assert!(active.contains("/ edit") && active.contains("esc clear"));

        // FilterNoMatch: names the query rather than claiming empty.
        let none = render_text(&scene(Scene::FilterNoMatch).1);
        assert!(none.contains("no sheep's name contains \"zzz\""));
        assert!(!none.contains("the flock is empty"));
        assert!(none.contains("0 of 6 in the flock"));
    }

    /// Each sheep-pane scene draws the columns its width leaves room for,
    /// down to the pair that share a row at 99.
    #[test]
    #[cfg(unix)] // inherited, not measured per test: see the `build` module's docs
    fn every_sheep_pane_scene_draws_the_columns_it_is_named_for() {
        // SheepPane: both charts, the config column, and the feed, all at
        // 160x48.
        let sheep_pane = render_text(&scene(Scene::SheepPane).1);
        assert!(
            sheep_pane.contains("\u{2588}\u{2588} CPU")
                && sheep_pane.contains("\u{2588}\u{2588} MEM"),
            "both charts draw: {sheep_pane:?}"
        );
        assert!(
            sheep_pane.contains("\u{2588}\u{2588} CONFIG & ENV") && sheep_pane.contains("script"),
            "the config column lists web's own fields: {sheep_pane:?}"
        );
        assert!(
            sheep_pane.contains("\u{2588}\u{2588} BLEATS"),
            "the embedded feed carries its own header: {sheep_pane:?}"
        );

        // SheepPaneCpuOnly: 139 columns, the CPU chart alone plus a
        // one-line memory summary.
        let cpu_only = render_text(&scene(Scene::SheepPaneCpuOnly).1);
        assert!(
            cpu_only.contains("\u{2588}\u{2588} CPU"),
            "the CPU chart still draws: {cpu_only:?}"
        );
        assert!(
            !cpu_only.contains("\u{2588}\u{2588} MEM"),
            "the memory chart is gone: {cpu_only:?}"
        );
        assert!(
            cpu_only.contains("rss "),
            "replaced by a one-line rss summary: {cpu_only:?}"
        );

        // SheepPaneSparklines: 99 columns, 1a's own pair on one row.
        let sparklines = render_text(&scene(Scene::SheepPaneSparklines).1);
        assert!(
            !sparklines.contains("\u{2588}\u{2588} CPU")
                && !sparklines.contains("\u{2588}\u{2588} MEM"),
            "both charts are gone: {sparklines:?}"
        );
        assert!(
            sparklines.contains("CPU 20s") && sparklines.contains("MEM/CEIL"),
            "1a's own pair draws instead: {sparklines:?}"
        );

        // SheepPaneShort: 160x25, the memory chart gone, the CPU chart and
        // the config column still up.
        let short_pane = render_text(&scene(Scene::SheepPaneShort).1);
        assert!(
            short_pane.contains("\u{2588}\u{2588} CPU"),
            "the CPU chart still draws: {short_pane:?}"
        );
        assert!(
            !short_pane.contains("\u{2588}\u{2588} MEM"),
            "the memory chart is gone under 26 rows: {short_pane:?}"
        );
        assert!(
            short_pane.contains("\u{2588}\u{2588} CONFIG & ENV"),
            "the config column, which gives ground last, is still up: {short_pane:?}"
        );
    }

    /// Each width scene draws the column count it exists to show, counted
    /// off the heading row's own occupied starts rather than inferred from
    /// a word being present. A test that only looked for `MOVING` would
    /// pass at every width in the table, since `MOVING` draws at every
    /// width the overlay ever renders.
    #[test]
    fn each_keymap_width_scene_draws_its_own_column_count() {
        for (which, wanted) in [
            (Scene::Keymap, 4),
            (Scene::KeymapFloor, 4),
            (Scene::KeymapBorderlessWide, 4),
            (Scene::KeymapNarrow, 3),
            (Scene::KeymapTwoColumn, 2),
        ] {
            let rendered = render_text(&scene(which).1);
            let first_bank = rendered
                .lines()
                // `Group::Moving.heading()`, not the literal "MOVING": the
                // count below derives from `heading()`, so a rename would
                // leave this `find` hunting a word nothing draws and the
                // failure would read as a missing heading row rather than as
                // a rename.
                .find(|row| row.contains(Group::Moving.heading()))
                .expect("no heading row");
            let drawn = Group::DRAWN
                .iter()
                .filter(|group| first_bank.contains(group.heading()))
                .count();
            assert_eq!(drawn, wanted, "{}: {first_bank:?}", which.label());
        }
    }

    /// Every `Keymap*` scene actually raises the overlay.
    ///
    /// `scene`'s builder raises it from a `matches!` list of variants, and
    /// the comment over that list says "every `Keymap*` scene". Nothing held
    /// that claim. The per-scene checks in this module are a hand-written
    /// list, `each_keymap_width_scene_draws_its_own_column_count` names five
    /// of the eight, and a snapshot is accepted for whatever a scene
    /// rendered, so a ninth keymap scene left out of the `matches!` list
    /// would draw the dashboard and be pinned that way.
    ///
    /// Driven off the label rather than a list, so a new scene joins this
    /// test by being named. Dropping `KeymapNarrow` from the builder's list
    /// fails it, and so does adding a scene the list does not cover.
    ///
    /// The count is the other half: it catches a keymap scene whose label
    /// does not start with `keymap`, which this test would otherwise skip in
    /// silence, and it is the same eight the gallery preamble promises.
    #[test]
    fn every_keymap_scene_raises_the_overlay() {
        let mut checked = 0;
        for which in Scene::ALL {
            if !which.label().starts_with("keymap") {
                continue;
            }
            checked += 1;
            let rendered = render_text(&scene(*which).1);
            assert!(
                rendered.contains(Group::Moving.heading()),
                "{}: the overlay did not draw",
                which.label()
            );
        }
        assert_eq!(checked, 8, "the keymap scenes, counted by label");
    }
}
