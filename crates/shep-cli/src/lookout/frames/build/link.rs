//! What arrived over the link and from the machine: the host sample, the
//! bleats feed, the lamb walk, and whether the shepherd is still there.

use std::time::Duration;

use shep_core::protocol::{Lamb, ProcessInfo, Response};
use shep_core::status::ProcStatus;

use crate::lookout::app::{ActionVerb, App, KeyPress, Msg, Sent};
use crate::lookout::frames::fixtures::feed_for;
use crate::lookout::frames::scene::Scene;
use crate::lookout::source::HostSample;
use crate::lookout::view::fixtures::FROZEN_WHY;

/// Feeds `app` everything that reached it while the link was live.
pub(super) fn apply_live_updates(app: &mut App, which: Scene) {
    // Every live scene gets a host sample, frozen included: without a
    // baseline sample the strip would read "not read yet" regardless of
    // the freeze guard. `Scene::Frozen` below sends a second, older
    // sample that exercises the guard itself.
    if which == Scene::HostUnknown {
        app.update(Msg::Host { sample: None });
    } else {
        app.update(Msg::Host {
            sample: Some(HostSample {
                load: (2.31, 4.10, 3.88),
                cores: Some(10),
                memory_total_bytes: 32 << 30,
                memory_used_bytes: 12 * (1 << 30) + (410 << 20),
                uptime_seconds: 6 * 86_400 + 3 * 3_600,
            }),
        });
    }

    app.update(Msg::Bleats {
        tail: feed_for(which),
    });

    // Opens the pane on `api` (`super::prepare` selected it) and stacks all
    // three filter axes: `o` once for `out`, `m` four times for `None ->
    // Trace -> Debug -> Info -> Warn`, then a regex typed into the match
    // box. `w` last, since it toggles independently of the filters and
    // this is the one fixture line worth wrapping.
    if which == Scene::Bleats {
        app.update(Msg::Key(KeyPress::Bleats));
        app.update(Msg::Key(KeyPress::StreamCycle));
        for _ in 0..4 {
            app.update(Msg::Key(KeyPress::LevelCycle));
        }
        app.update(Msg::Key(KeyPress::FilterStart));
        for typed in "/retry|jitter/".chars() {
            app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
        app.update(Msg::Key(KeyPress::WrapToggle));
    }

    // Applied while the link is still `Live`: `on_lambs` refuses once it
    // is `Lost`, the same guard `Msg::Bleats` carries.
    if matches!(which, Scene::Lambs | Scene::Frozen) {
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 2 },
            result: Ok(Response::Described(vec![
                ProcessInfo::builder(2, "api", ProcStatus::Online)
                    .pid(Some(48_219))
                    .lambs(Some(vec![
                        Lamb::new(48_220, "node"),
                        Lamb::new(48_221, "node"),
                        Lamb::new(48_222, "node"),
                    ]))
                    .build(),
            ])),
        });
    }

    // `cron` (id 4) has no pid, so the shepherd's walk never ran and
    // `lambs_for(4)` stays `None`.
    if which == Scene::LambsUnknown {
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 4 },
            result: Ok(Response::Described(vec![
                ProcessInfo::builder(4, "cron", ProcStatus::Stopped)
                    .lambs(None)
                    .build(),
            ])),
        });
    }
}

/// Puts the link into the state `which` is named for.
pub(super) fn apply_connection_state(app: &mut App, which: Scene, age: Duration) {
    // `Msg::Host` and the `SelectDown`s run before `Msg::Frozen`: the
    // reducer refuses both once frozen.
    match which {
        Scene::Retrying => {
            app.update(Msg::Retrying { attempt: 3 });
        }
        Scene::Frozen | Scene::KeymapFrozen => {
            app.update(Msg::Frozen {
                at_local: "2026-08-14 14:32:07".to_string(),
                why: FROZEN_WHY.to_string(),
            });
            // Sent after `Msg::Frozen`, with a load average that varies
            // with `age`: the guard's refusal keeps
            // `the_frozen_frame_does_not_move_however_long_the_link_stays_gone`
            // byte-identical across ages.
            app.update(Msg::Host {
                sample: Some(HostSample {
                    load: (2.31 + age.as_secs_f64(), 4.10, 3.88),
                    cores: Some(10),
                    memory_total_bytes: 32 << 30,
                    memory_used_bytes: 12 * (1 << 30) + (410 << 20),
                    uptime_seconds: 6 * 86_400 + 3 * 3_600,
                }),
            });
        }
        Scene::Refused => {
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use crate::lookout::frames::build::scene_with;
    use crate::lookout::frames::coloured_palette;
    use crate::lookout::frames::render::render_text;
    use crate::lookout::frames::scene;

    use super::*;

    /// Each link scene shows the state it is named for, and keeps
    /// describing the last listing rather than blanking.
    #[test]
    #[cfg(unix)] // inherited, not measured per test: see the `build` module's docs
    fn every_link_scene_shows_the_state_it_is_named_for() {
        // Retrying: every pane keeps describing the last listing.
        let retrying = render_text(&scene(Scene::Retrying).1);
        assert!(retrying.contains("reconnecting"));
        assert!(
            retrying.contains("SHEEP 2  api"),
            "the detail pane is still up"
        );
        assert!(retrying.contains("host  load"), "and so is the strip");

        // Frozen: last known values stay, nothing keeps ticking, and the
        // panel that replaced the two lower panes says what happened.
        let frozen = render_text(&scene(Scene::Frozen).1);
        assert!(frozen.contains("THE SHEPHERD HAS DIED"));
        assert!(
            frozen.contains("host  load  ██░░░░░░░░ 2.31 4.10 3.88 / 10 cores"),
            "the strip kept its LAST values rather than blanking"
        );
        assert!(
            frozen.contains("FROZEN"),
            "UPTIME renamed over a column that stopped advancing"
        );
        assert!(
            frozen.contains(
                "refused   the shepherd did not answer: could not connect to `/home/ada/.shep/run/shep.sock`: Connection refused (os error 61)"
            ),
            "the link panel quotes the last dial's own error, whole"
        );
        assert!(
            frozen.contains("█ 250ms  █ 500ms  █ 1s  █ 2s  █ 4s"),
            "and draws every rung the ladder climbed"
        );
        // The design's own copy offers `r` in both places. `r` is refused
        // once the link is lost, and `run_link` has already returned, so
        // the frame must not name it anywhere.
        assert!(
            frozen.contains("shep muster, from another shell"),
            "the panel sends the operator somewhere that works"
        );
        assert!(
            !frozen.contains("dials again") && !frozen.contains("retry the link"),
            "and offers no key a freeze has already refused"
        );

        // Refused: `x` with actions gated off.
        let refused = render_text(&scene(Scene::Refused).1);
        assert!(refused.contains("--read-only"), "{refused}");
        assert!(
            refused.contains("BLEATS api"),
            "a refusal does not blank the screen"
        );

        // HostUnknown: the strip keeps the flock's own totals.
        let unknown = render_text(&scene(Scene::HostUnknown).1);
        assert!(unknown.contains("host  usage is not available on this platform"));
        assert!(
            unknown.contains("flock cpu"),
            "the half lookout can compute survives"
        );
    }

    /// The two feed scenes name what they lost rather than sitting blank.
    #[test]
    #[cfg(unix)] // inherited, not measured per test: see the `build` module's docs
    fn the_feed_scenes_name_what_they_lost() {
        // FeedGap: dropped lines and never-read bytes, counted separately.
        let gap = render_text(&scene(Scene::FeedGap).1);
        assert!(
            gap.contains("earlier lines not shown"),
            "the lines it dropped"
        );
        assert!(gap.contains("3.8M"), "the exact figure, not a vague one");
        assert!(gap.contains("never read"), "and what it never looked at");
        assert!(
            !gap.contains("re-read with each listing"),
            "the gap replaces the header"
        );

        // FeedMissing: names the cause rather than sitting blank.
        let missing = render_text(&scene(Scene::FeedMissing).1);
        assert!(missing.contains("has not written a log in this $SHEP_HOME"));
    }

    /// The lamb scenes draw the walk's own stamp, and say `unset` rather
    /// than `empty` where there was no pid to walk from.
    #[test]
    #[cfg(unix)] // inherited, not measured per test: see the `build` module's docs
    fn the_lamb_scenes_show_the_walk_they_are_named_for() {
        // Lambs: the stamp sits before the list.
        let lambs = render_text(&scene(Scene::Lambs).1);
        assert!(lambs.contains("lambs  3 parent-pid descendants, read "));
        assert!(lambs.contains("48220 node"), "each lamb's pid and name");
        let line = lambs
            .lines()
            .find(|line| line.starts_with("lambs  "))
            .expect("the lamb line");
        assert!(
            line.find("read ").unwrap() < line.find("48220").unwrap(),
            "the stamp comes before the list"
        );

        // LambsUnknown: no pid to walk from, unset rather than empty.
        let unknown = render_text(&scene(Scene::LambsUnknown).1);
        assert!(unknown.contains("lambs  this sheep is not running, so there is no tree to walk"));
        assert!(
            !unknown.contains("none found"),
            "which is the other sentence"
        );
        assert!(unknown.contains("SHEEP 4  cron"), "on the stopped sheep");
    }

    /// The bleats scene stacks all three filter axes at once, with
    /// wrapping on.
    #[test]
    #[cfg(unix)] // inherited, not measured per test: see the `build` module's docs
    fn the_bleats_scene_stacks_all_three_axes() {
        // Bleats: all three axes stacked, wrapping on.
        let bleats = render_text(&scene(Scene::Bleats).1);
        for chip in ["stream out", "level ≥ warn", "match /retry|jitter/ (regex)"] {
            assert!(
                bleats.contains(chip),
                "the filter row names {chip}: {bleats:?}"
            );
        }
        assert!(
            // The sentence itself is longer than this 100-column frame, so
            // it fits and truncates with an ellipsis: this is the prefix
            // that survives the cut.
            bleats.contains("1 of 16 lines in the window: all three m…"),
            "the survivor count and the start of the composition sentence: {bleats:?}"
        );
        assert!(
            bleats.contains("out  WARN retrying upstream payment gateway"),
            "the one surviving line, tagged by its stream: {bleats:?}"
        );
        assert!(
            bleats
                .lines()
                .any(|line| line.starts_with("     ith jitter")),
            "wrapping is on, so the line's tail lands on its own row rather than truncating: {bleats:?}"
        );
        assert!(
            bleats.contains("esc back") && bleats.contains("\u{2588} following"),
            "the full key line, and the pane is still following the tail: {bleats:?}"
        );
    }

    /// Rule 3 of the design system: strip every colour and the frame still
    /// reads. Its corollary on this one frame is stronger — no cell above
    /// the link panel may carry a colour at all, since a meadow `online`
    /// two seconds after the shepherd died is the one lie this screen can
    /// tell.
    #[test]
    fn no_cell_above_the_link_panel_is_painted_live() {
        let palette = coloured_palette();
        let buffer = scene(Scene::Frozen).1;
        let muted = palette.muted().fg;
        let line = palette.line().fg;
        // Row 0 is the band, which is bark and says so; the link panel
        // below carries the ladder's own bark blocks and a butter `r`.
        let panel_at = render_text(&buffer)
            .lines()
            .position(|row| row.contains("THE LINK"))
            .expect("the link panel is on a frozen frame");
        let panel_at = u16::try_from(panel_at).expect("a row index fits");
        for y in 1..panel_at {
            for x in 0..buffer.area.width {
                let cell = &buffer[(buffer.area.x + x, buffer.area.y + y)];
                assert!(
                    cell.fg == Color::Reset || Some(cell.fg) == muted || Some(cell.fg) == line,
                    "a live-looking cell at {x},{y}: {:?} in {:?}",
                    cell.symbol(),
                    cell.fg
                );
            }
        }
    }

    /// Rendered twice at two different ages and compared to each other,
    /// not to the healthy scene, since a live-versus-frozen diff would
    /// pass either way. The live pair at the bottom catches a renderer
    /// that drops the uptime column entirely.
    ///
    /// Split at the link panel's own chip rather than at a row index. Every
    /// value above it came from the shepherd and stopped when the shepherd
    /// did; the panel below it describes the link, and how long ago the
    /// link died is a fact about now. One half must not move and the other
    /// must, so the test asserts both rather than narrowing to the half
    /// that is easier to pin.
    #[test]
    fn a_frozen_frame_moves_only_in_the_link_panel() {
        let ten_minutes = render_text(&scene_with(
            Scene::Frozen,
            Duration::from_secs(600),
            coloured_palette(),
        ));
        let sixteen_hours = render_text(&scene_with(
            Scene::Frozen,
            Duration::from_secs(60_000),
            coloured_palette(),
        ));
        let above_the_panel = |frame: &str| {
            frame
                .split("THE LINK")
                .next()
                .expect("split yields at least one part")
                .to_string()
        };
        assert_eq!(
            above_the_panel(&ten_minutes),
            above_the_panel(&sixteen_hours),
            "the frozen frame's uptime column advanced after the link was lost"
        );
        assert_ne!(
            ten_minutes, sixteen_hours,
            "the link panel's age is the one number a frozen frame still counts"
        );
        // Seven seconds short of each age, and deliberately: `scene_with`
        // ticks the clock forward by that much before it freezes anything,
        // so the freeze happens at `t0 + 7s` and the panel counts from
        // there. `9m 53s`, not `593s` — the same two-unit shape every other
        // duration on the screen uses.
        assert!(
            ten_minutes.contains("2026-08-14 14:32:07, 9m 53s ago"),
            "and it counts in the same words every other duration uses"
        );
        assert!(
            sixteen_hours.contains("2026-08-14 14:32:07, 16h 39m ago"),
            "at both ends of the sweep"
        );
        assert!(
            above_the_panel(&ten_minutes).contains("1h 18m"),
            "the frozen frame has an uptime cell for the comparison above to cover"
        );

        let live_ten = render_text(&scene_with(
            Scene::HealthyWide,
            Duration::from_secs(600),
            coloured_palette(),
        ));
        let live_sixteen = render_text(&scene_with(
            Scene::HealthyWide,
            Duration::from_secs(60_000),
            coloured_palette(),
        ));
        assert_ne!(
            live_ten, live_sixteen,
            "a LIVE frame's uptime column must advance, or the assertion above passes for the wrong reason"
        );
    }
}
