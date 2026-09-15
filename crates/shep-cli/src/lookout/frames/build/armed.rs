//! Everything armed AFTER `scene_with`'s tick.
//!
//! The gallery renders at 600s, well past `CONFIRM_EXPIRY` and
//! `REVEAL_HOLDS`, so a confirm, a reveal or a pending action built
//! before the tick would already read as expired by the time the frame is
//! drawn. State with no expiry of its own goes in [`super::prepare`].

use std::time::{Duration, Instant};

use shep_client::RequestError;
use shep_core::protocol::{Response, RpcError, RpcErrorCode};

use crate::commands::settings::{SettingField, load_settings};
use crate::commands::shep_toml::ShepToml;
use crate::lookout::app::{ActionVerb, App, KeyPress, Msg, RevealedValue, RowKey, Sent};
use crate::lookout::frames::fixtures::{
    close_dialog_config_view, edit_pane_config_view, flock_without_api, move_settings_cursor_to,
    restarted_api, settings_snapshot_for_gallery, settings_snapshot_with_dog_drift,
};
use crate::lookout::frames::scene::Scene;
use crate::lookout::secrets::{SecretRow, SecretsModel, Source};
use crate::lookout::view::fixtures::select_field;
use crate::secret_readers::Reader;
use crate::style::{StyleLevel, StyleSource};

/// Presses the action key `which` is named for, and answers it where
/// the scene shows a reply.
pub(super) fn apply_actions(app: &mut App, which: Scene, t0: Instant) {
    // Applied after the last tick: `scene()` renders at `age` = 600s past
    // `CONFIRM_EXPIRY` (10s), so an armed confirm built before the tick
    // would already show expired.
    match which {
        Scene::ActionRefusedOffline => {
            // The link must stop being live before the key is pressed, or
            // `arm` would accept it.
            app.update(Msg::Retrying { attempt: 3 });
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        }
        Scene::Confirm | Scene::Acting | Scene::ActionRefused | Scene::ActionAccepted => {
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
            if which != Scene::Confirm {
                app.update(Msg::Key(KeyPress::Confirm));
            }
            if which == Scene::ActionAccepted {
                app.update(Msg::Replied {
                    sent: Sent::Action {
                        verb: ActionVerb::Restart,
                        target: RowKey::Sheep(2),
                        name: "api".to_string(),
                    },
                    result: Ok(Response::Restarted {
                        accepted: vec![restarted_api()],
                        refused: Vec::new(),
                    }),
                });
            }
            if which == Scene::ActionRefused {
                // The sheep leaves the flock while the request is out, which
                // is what makes the daemon's own sentence the true one.
                app.update(Msg::Snapshot {
                    rows: flock_without_api(),
                    at: t0,
                });
                app.update(Msg::Replied {
                    sent: Sent::Action {
                        verb: ActionVerb::Restart,
                        target: RowKey::Sheep(2),
                        name: "api".to_string(),
                    },
                    result: Err(RequestError::Rpc(RpcError {
                        code: RpcErrorCode::NotFound,
                        message: "selector matched no registered sheep".to_string(),
                        daemon_version: None,
                    })),
                });
            }
        }
        _ => {}
    }
}

/// Opens the overlay pane `which` is named for, and arms whatever it
/// holds.
pub(super) fn apply_post_tick_scene(app: &mut App, which: Scene) {
    // Applied last, for the same reason: `SettingsConfirm` and `Secrets`
    // each arm a candidate that expires (on `CONFIRM_EXPIRY` or
    // `REVEAL_HOLDS`), so both must be armed after the tick at `age`.
    match which {
        Scene::Secrets => {
            // Opens the secrets pane on `production`: an operator row
            // revealed, a provider group, and a row with a slot
            // elsewhere but not here. `Msg::Revealed` is handed the
            // value directly; the reducer only checks the gate below.
            app.update(Msg::Key(KeyPress::Secrets));
            app.update(Msg::Secrets {
                environment: "production".to_string(),
                result: Ok(Box::new(SecretsModel {
                    environments: vec![
                        "all".to_string(),
                        "ci".to_string(),
                        "production".to_string(),
                    ],
                    rows: vec![
                        SecretRow {
                            key: "DB_PASSWORD".to_string(),
                            source: Source::Operator,
                            in_force: Some("production".to_string()),
                            set_in: vec!["production".to_string()],
                            byte_len: Some("hunter2-not-really".len()),
                            readers: vec![Reader {
                                name: "catcher".to_string(),
                                environment: "production".to_string(),
                                online: true,
                            }],
                        },
                        SecretRow {
                            key: "ELSEWHERE_ONLY".to_string(),
                            source: Source::Operator,
                            in_force: None,
                            set_in: vec!["ci".to_string()],
                            byte_len: None,
                            readers: Vec::new(),
                        },
                        SecretRow {
                            key: "vercel/API_TOKEN".to_string(),
                            source: Source::Namespace("vercel".to_string()),
                            in_force: Some("production".to_string()),
                            set_in: vec!["production".to_string()],
                            byte_len: Some(6),
                            readers: Vec::new(),
                        },
                    ],
                    // Every row's `readers` comes off the muster roll, and
                    // so does this: a model carrying one without the other
                    // is a state `secrets::model` cannot produce, and the
                    // frame would name a reader under a line saying no roll
                    // was ever written.
                    roll_age: Some(Duration::from_secs(184)),
                    allow_read: true,
                    ..SecretsModel::default()
                })),
            });
            app.update(Msg::Key(KeyPress::Reveal));
            app.update(Msg::Revealed {
                key: "DB_PASSWORD".to_string(),
                environment: "production".to_string(),
                value: Some(RevealedValue("hunter2-not-really".to_string())),
            });
        }
        Scene::SettingsFresh => {
            // A fresh document, not a hand-edited snapshot: first run
            // leaves only `[interpreters]`, and `load_settings` is the
            // same reader `run_ui` calls.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("shep.toml");
            ShepToml::edit(&path, ShepToml::write_starter_interpreters).unwrap();
            let snapshot = load_settings(
                &path,
                std::path::Path::new("/home/ada/.shep/run/shep.sock"),
                (StyleLevel::Full, StyleSource::Default),
            )
            .unwrap();
            app.update(Msg::Key(KeyPress::Settings));
            app.update(Msg::Settings {
                result: Ok(snapshot),
            });
        }
        Scene::SettingsSet => {
            app.update(Msg::Key(KeyPress::Settings));
            app.update(Msg::Settings {
                result: Ok(settings_snapshot_for_gallery()),
            });
        }
        Scene::SettingsConfirm => {
            app.update(Msg::Key(KeyPress::Settings));
            app.update(Msg::Settings {
                result: Ok(settings_snapshot_for_gallery()),
            });
            // The cursor already sits on `log_level`, `Settings::rows`'s
            // first row, so arming it needs no `SelectDown` at all.
            app.update(Msg::Key(KeyPress::Cycle));
        }
        Scene::SettingsTyping => {
            app.update(Msg::Key(KeyPress::Settings));
            app.update(Msg::Settings {
                result: Ok(settings_snapshot_for_gallery()),
            });
            move_settings_cursor_to(app, SettingField::Socket);
            // Opens the editor, seeded with the on-disk value.
            app.update(Msg::Key(KeyPress::Confirm));
            // Trims the seeded value back to a partial path, so the frame
            // shows the editor genuinely mid-type rather than holding the
            // whole, untouched value it opened with.
            for _ in 0..8 {
                app.update(Msg::Key(KeyPress::TextBackspace));
            }
        }
        Scene::SettingsDogs | Scene::SettingsNarrow => {
            app.update(Msg::Key(KeyPress::Settings));
            app.update(Msg::Settings {
                result: Ok(settings_snapshot_with_dog_drift()),
            });
        }
        Scene::SettingsShort => {
            app.update(Msg::Key(KeyPress::Settings));
            app.update(Msg::Settings {
                result: Ok(settings_snapshot_with_dog_drift()),
            });
            // Onto the last row, which is the one a body this short cannot
            // reach without scrolling.
            app.update(Msg::Key(KeyPress::SelectLast));
        }
        Scene::EditPane
        | Scene::EditPaneEdited
        | Scene::EditPaneSqueezed
        | Scene::EditPaneNarrow => {
            // `e` on the selected row (`api`, id 2), the way `ask_for_config`
            // reaches it, then the shepherd's own reply.
            app.update(Msg::Key(KeyPress::Edit));
            app.update(Msg::Replied {
                sent: Sent::SheepConfig {
                    name: "api".to_string(),
                },
                result: Ok(Response::SheepConfig(Box::new(edit_pane_config_view()))),
            });
            // Two edits, driven by real key presses the way
            // `select_field` always is: `cwd`, which needs a respawn, and
            // `max_memory`, which lands at once, so the pending section
            // and the title's own count both have something to show.
            if which == Scene::EditPaneEdited {
                for (key, typed) in [("cwd", "/srv/api"), ("max_memory", "256")] {
                    select_field(app, key);
                    app.update(Msg::Key(KeyPress::Confirm));
                    for character in typed.chars() {
                        app.update(Msg::Key(KeyPress::TextChar(character)));
                    }
                    app.update(Msg::Key(KeyPress::TextApply));
                }
            }
        }
        Scene::CloseDialog
        | Scene::CloseDialogFloor
        | Scene::CloseDialogNarrow
        | Scene::CloseDialogParked => {
            // The same `e` on `api` (id 2) that opens every edit-pane
            // scene, but replied with a view that already carries one
            // parked field, `listen_timeout`, so the close dialog has
            // something to say about the parked half without any edit of
            // the operator's own.
            app.update(Msg::Key(KeyPress::Edit));
            app.update(Msg::Replied {
                sent: Sent::SheepConfig {
                    name: "api".to_string(),
                },
                result: Ok(Response::SheepConfig(Box::new(close_dialog_config_view()))),
            });
            // `cwd` and `err_file` both need a respawn, so the dialog's
            // heading names an unsent count alongside the parked one.
            // `CloseDialogParked` files neither, relying on the fixture's
            // own parked field alone.
            if which != Scene::CloseDialogParked {
                for (key, typed) in [("cwd", "/srv/api"), ("err_file", "/var/log/api/err.log")] {
                    select_field(app, key);
                    app.update(Msg::Key(KeyPress::Confirm));
                    for character in typed.chars() {
                        app.update(Msg::Key(KeyPress::TextChar(character)));
                    }
                    app.update(Msg::Key(KeyPress::TextApply));
                }
            }
            // Raises the dialog: `esc` stops writing on its own and asks
            // first, which is the whole point of this frame.
            app.update(Msg::Key(KeyPress::Escape));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use crate::lookout::frames::build::probe::{
        dog_row_for, marked_row_name_starts_with, row_for, selected_line,
    };
    use crate::lookout::frames::render::render_text;
    use crate::lookout::frames::scene;

    use super::*;

    /// Each action scene shows its own stage of the round trip: armed,
    /// sent, accepted, refused, and refused with the link already gone.
    #[test]
    #[cfg(unix)] // inherited, not measured per test: see the `build` module's docs
    fn every_action_scene_shows_its_own_stage_of_the_round_trip() {
        // Confirm: `R` pressed, nothing sent yet.
        let confirm = render_text(&scene(Scene::Confirm).1);
        assert!(confirm.contains("restart api (id 2)? enter confirms, any other key cancels"));
        assert!(confirm.contains("control enabled"), "the gate is open");
        assert!(
            row_for(&confirm, "api").is_some_and(|row| row.contains("online")),
            "nothing was sent, so api is still online: {confirm:?}"
        );

        // Acting: request out, table unchanged.
        let acting_buffer = scene(Scene::Acting).1;
        let acting = render_text(&acting_buffer);
        assert!(acting.contains("restart api (id 2): sent, waiting for the shepherd"));
        assert!(
            selected_line(&acting, &acting_buffer).is_some_and(|line| line.contains("api")),
            "the table is untouched: the selection is still on api"
        );
        assert!(
            row_for(&acting, "api").is_some_and(|row| row.contains("online")),
            "and the row still says what the shepherd last said"
        );

        // ActionAccepted: the reply's own row reaches the table at once.
        let accepted = render_text(&scene(Scene::ActionAccepted).1);
        assert!(accepted.contains("restart api (id 2): the shepherd restarted it"));
        assert!(
            row_for(&accepted, "api").is_some_and(|row| row.contains("48299")),
            "the reply's own row reached the table without waiting for a poll"
        );

        // ActionRefused: the shepherd's own sentence is forwarded as is.
        let action_refused_buffer = scene(Scene::ActionRefused).1;
        let refused = render_text(&action_refused_buffer);
        assert!(refused.contains("restart api (id 2): selector matched no registered sheep"));
        assert!(
            !refused.contains("NotFound"),
            "no Rust identifiers on the bar"
        );
        assert!(refused.contains("5 in the flock"), "one row shorter");
        assert!(
            row_for(&refused, "api").is_none(),
            "api is the row that went"
        );
        assert!(
            marked_row_name_starts_with(&refused, &action_refused_buffer, "billing"),
            "and the cursor has moved to the row below: {refused:?}"
        );

        // ActionRefusedOffline: names the same reconnect attempt as the
        // banner above it, not the exhausted-ladder sentence.
        let offline = render_text(&scene(Scene::ActionRefusedOffline).1);
        assert_eq!(
            offline.matches("reconnecting (attempt 3)").count(),
            2,
            "the banner and the refusal under it agree, rather than one \
             saying reconnecting and the other saying gone: {offline:?}"
        );
        assert!(
            !offline.contains("nothing left to ask"),
            "the ladder has not run out yet, so the refusal must not claim it has: {offline:?}"
        );

        // The left bar slot is empty here, same as on every ordinary
        // dashboard scene, so this bar carries the plain control hint.
        let lambs_bar = render_text(&scene(Scene::Lambs).1);
        for key in ["x stop", "R restart", "L reload"] {
            assert!(lambs_bar.contains(key), "the control hint names {key}");
        }
    }

    /// Each settings scene shows which layer every value came from, and
    /// drops a column rather than clipping when the terminal narrows.
    #[test]
    #[cfg(unix)] // inherited, not measured per test: see the `build` module's docs
    fn every_settings_scene_shows_the_layer_it_is_named_for() {
        // SettingsFresh: a bare `shep.toml`, every scalar reads the default.
        let fresh = render_text(&scene(Scene::SettingsFresh).1);
        assert_eq!(
            fresh.matches("the default").count(),
            6,
            "all six scalars read the default: {fresh:?}"
        );
        assert!(
            !fresh.contains("shep.toml  "),
            "a fresh home has declared nothing: {fresh:?}"
        );

        // SettingsSet: `shep.toml` and the default sit side by side.
        let set = render_text(&scene(Scene::SettingsSet).1);
        assert!(set.contains("shep.toml"), "some scalars are declared");
        assert!(set.contains("the default"), "and some are not: {set:?}");

        // SettingsConfirm: names the env var and flag it cannot see.
        let confirm = render_text(&scene(Scene::SettingsConfirm).1);
        assert!(confirm.contains("shep daemon reload"), "got: {confirm:?}");
        assert!(confirm.contains("SHEP_LOG_LEVEL"), "got: {confirm:?}");
        assert!(confirm.contains("--log-level"), "got: {confirm:?}");

        // SettingsTyping: names the field being typed, not the filter box.
        let typing = render_text(&scene(Scene::SettingsTyping).1);
        assert!(
            typing.contains("editing socket"),
            "names the field being typed: {typing:?}"
        );
        assert!(
            !typing.contains("filter "),
            "must not read as the dashboard's own filter box: {typing:?}"
        );

        // SettingsDogs: the drift the table exists to reveal.
        let dogs = render_text(&scene(Scene::SettingsDogs).1);
        assert!(
            dog_row_for(&dogs, "otel")
                .is_some_and(|row| row.contains("no") && row.contains("online")),
            "otel: disabled in the file, running: {dogs:?}"
        );
        assert!(
            dog_row_for(&dogs, "ledger")
                .is_some_and(|row| row.contains("yes") && row.contains("not running")),
            "ledger: enabled, absent from the flock: {dogs:?}"
        );
        assert!(
            dog_row_for(&dogs, "bark")
                .is_some_and(|row| row.contains("yes") && row.contains("silent")),
            "bark: enabled, running, never handshook: {dogs:?}"
        );

        // SettingsNarrow: both tables drop a column rather than clip.
        let narrow = render_text(&scene(Scene::SettingsNarrow).1);
        assert!(
            narrow.contains("shep.toml"),
            "the scalar rows keep SOURCE: {narrow:?}"
        );
        assert!(
            !narrow.contains("needs shep daemon reload"),
            "and lose the apply cost: {narrow:?}"
        );
        assert!(
            dog_row_for(&narrow, "otel").is_some_and(|row| row.contains("online")),
            "the dogs table keeps RUNNING: {narrow:?}"
        );
        assert!(!narrow.contains("built-in"), "and loses SOURCE: {narrow:?}");

        // "The same screen at 14 rows, which is fewer than it has to draw.
        //  The cursor is on the last dog, so the view has scrolled to reach
        //  it and `... 5 above` says how much is off the top. The scroll is
        //  counted in lines rather than in rows: a section header and the
        //  dogs caption cost the same height a row does."
        let short = render_text(&scene(Scene::SettingsShort).1);
        assert!(
            short.contains("... 5 above"),
            "the marker names how many rows are off the top: {short:?}"
        );
        assert!(
            short
                .lines()
                .any(|line| line.starts_with("> bark")
                    || line.starts_with('>') && line.contains("bark")),
            "the cursor's own row is drawn: {short:?}"
        );
        assert!(
            !short.contains("log_level"),
            "and the rows above it are the ones that went: {short:?}"
        );
        assert!(
            short.contains("[style]") && short.contains("[dogs]"),
            "what survives is whole sections, headers and all: {short:?}"
        );
    }

    /// Each edit-pane scene draws the panel its width leaves room for, and
    /// the title band carries the edit count only once there are edits.
    #[test]
    #[cfg(unix)] // inherited, not measured per test: see the `build` module's docs
    fn every_edit_pane_scene_draws_the_panel_it_is_named_for() {
        // EditPane: fresh, no edits filed, 160x48. LANDS draws in the
        // header and the explanation panel is on screen, and the title
        // band carries no edit count.
        let edit_pane = render_text(&scene(Scene::EditPane).1);
        let title = edit_pane
            .lines()
            .find(|line| line.contains("(sheep config)"))
            .expect("the title band");
        assert!(
            !title.contains("edit"),
            "fresh, so the title names no edit count: {title:?}"
        );
        let header = edit_pane
            .lines()
            .find(|line| line.contains("FIELD") && line.contains("VALUE"))
            .expect("the field list header");
        assert!(
            header.contains("LANDS"),
            "wide enough for the cost column: {header:?}"
        );
        assert!(
            edit_pane.contains("FOCUSED"),
            "the explanation panel names the focused field: {edit_pane:?}"
        );

        // EditPaneEdited: the same pane with cwd and max_memory filed. The
        // title counts them and the pending section lists both.
        let edit_pane_edited = render_text(&scene(Scene::EditPaneEdited).1);
        let edited_title = edit_pane_edited
            .lines()
            .find(|line| line.contains("(sheep config)"))
            .expect("the title band");
        assert!(
            edited_title.contains("2 edits"),
            "two edits filed: {edited_title:?}"
        );
        let pending_section = edit_pane_edited
            .lines()
            .skip_while(|line| !line.trim_start().starts_with("pending edits"))
            .take(3)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            pending_section.contains("cwd") && pending_section.contains("max_memory"),
            "both filed edits are named under the pending edits section: {pending_section:?}"
        );

        // EditPaneSqueezed: 120 columns. The panel still draws; LANDS
        // gives way to it, so the header carries FIELD and VALUE but not
        // LANDS.
        let squeezed = render_text(&scene(Scene::EditPaneSqueezed).1);
        let squeezed_header = squeezed
            .lines()
            .find(|line| line.contains("FIELD") && line.contains("VALUE"))
            .expect("the field list header");
        assert!(
            !squeezed_header.contains("LANDS"),
            "LANDS gives way to the panel at 120 columns: {squeezed_header:?}"
        );
        assert!(
            squeezed.contains("FOCUSED"),
            "the panel still draws at 120 columns: {squeezed:?}"
        );

        // EditPaneNarrow: 88 columns. The panel is gone, so LANDS returns.
        let narrow_edit = render_text(&scene(Scene::EditPaneNarrow).1);
        let narrow_edit_header = narrow_edit
            .lines()
            .find(|line| line.contains("FIELD") && line.contains("VALUE"))
            .expect("the field list header");
        assert!(
            narrow_edit_header.contains("LANDS"),
            "nothing else carries cost at 88 columns, so LANDS is back: {narrow_edit_header:?}"
        );
        assert!(
            !narrow_edit.contains("FOCUSED"),
            "the panel does not draw at 88 columns: {narrow_edit:?}"
        );
    }

    /// Each close-dialog scene draws the border its width allows, and the
    /// heading names only the halves that scene actually has.
    #[test]
    #[cfg(unix)] // inherited, not measured per test: see the `build` module's docs
    fn every_close_dialog_scene_draws_the_heading_it_is_named_for() {
        // CloseDialog: 160x48, boxed. The heading, the first row inside
        // the border, names both halves: two edits needing a respawn and
        // one field already parked.
        let close_dialog = render_text(&scene(Scene::CloseDialog).1);
        let close_dialog_lines: Vec<&str> = close_dialog.lines().collect();
        let border_row = close_dialog_lines
            .iter()
            .position(|line| line.contains('▛')) // BOX_TOP_LEFT
            .expect("the box border draws at 160 columns");
        let heading = close_dialog_lines[border_row + 1];
        assert!(
            heading.contains("EDITS NEED A RESPAWN") && heading.contains("FIELD ALREADY"),
            "the heading names both the unsent and the parked half: {heading:?}"
        );

        // CloseDialogFloor: 90x48, exactly the width the border needs.
        // Read off the border's own row, the way both heading assertions
        // here do: a frame-wide `contains` passes on the glyph turning up
        // anywhere, and it is only this dialog that draws one today.
        let close_dialog_floor = render_text(&scene(Scene::CloseDialogFloor).1);
        let floor_lines: Vec<&str> = close_dialog_floor.lines().collect();
        let floor_border_row = floor_lines
            .iter()
            .position(|line| line.contains('▛')) // BOX_TOP_LEFT
            .expect("the box border draws at the floor");
        assert!(
            floor_lines[floor_border_row].contains('▜'), // BOX_TOP_RIGHT
            "the border's top row closes at the floor: {:?}",
            floor_lines[floor_border_row]
        );

        // CloseDialogNarrow: 89x48, one column under the floor. No box
        // glyph anywhere in the frame; this is the assertion a wrong
        // overlay::floor_for(BOX_WIDTH) would fail.
        let close_dialog_narrow = render_text(&scene(Scene::CloseDialogNarrow).1);
        for glyph in ['▛', '▜', '▙', '▟', '▐', '▀', '▄', '▌'] {
            assert!(
                !close_dialog_narrow.contains(glyph),
                "no border glyph {glyph:?} draws one column under the floor: {close_dialog_narrow:?}"
            );
        }

        // CloseDialogParked: 160x48, no edit of the operator's own. The
        // heading names only the parked half, with no unsent count.
        let close_dialog_parked = render_text(&scene(Scene::CloseDialogParked).1);
        let parked_lines: Vec<&str> = close_dialog_parked.lines().collect();
        let parked_border_row = parked_lines
            .iter()
            .position(|line| line.contains('▛'))
            .expect("the box border draws at 160 columns");
        let parked_heading = parked_lines[parked_border_row + 1];
        assert!(
            parked_heading.contains("FIELD ALREADY") && !parked_heading.contains("EDIT"),
            "the heading names the parked half alone, with no unsent count: {parked_heading:?}"
        );
    }

    /// `Scene::Secrets` claims a revealed row in its own doc, caption and
    /// the hand-copied frame in `web/`. Assert the frame actually shows
    /// the plaintext and a countdown, not a mask: a snapshot alone would
    /// pass on an expired reveal, since nothing names what "revealed"
    /// means.
    #[test]
    fn the_secrets_scene_shows_a_revealed_row_not_a_mask() {
        let text = render_text(&scene(Scene::Secrets).1);
        let revealed = text
            .lines()
            .find(|line| line.contains("DB_PASSWORD"))
            .expect("the secrets scene draws a DB_PASSWORD row");

        assert!(
            revealed.contains("hunter2-not-really"),
            "DB_PASSWORD's row should show the revealed plaintext, not a mask: {revealed:?}"
        );
        assert!(
            !revealed.contains("bytes"),
            "the VALUE cell should show plaintext, not a masked byte count: {revealed:?}"
        );
        assert!(
            revealed.contains("visible"),
            "a revealed row should show a countdown, not the unrevealed dash: {revealed:?}"
        );
    }

    /// `secrets::model` reads `readers` and `roll_age` off the same muster
    /// roll, so a frame naming a reader and then saying no roll exists is a
    /// state the loader cannot produce. The snapshot pins the frame against
    /// its own committed copy, so the contradiction stays green there and
    /// reaches `docs/lookout/` and the published page.
    #[test]
    fn the_secrets_scene_does_not_deny_the_roll_its_readers_came_from() {
        let text = render_text(&scene(Scene::Secrets).1);
        let read_by = text
            .lines()
            .find(|line| line.contains("DB_PASSWORD"))
            .expect("the secrets scene draws a DB_PASSWORD row");

        assert!(
            read_by.contains("1 (1 online)"),
            "the scene's own row names a reader: {read_by:?}"
        );
        assert!(
            !text.contains("no muster roll yet"),
            "and so cannot also say the roll it came from was never written"
        );
        assert!(
            text.contains("READ BY as of the roll"),
            "it says how old the roll is instead"
        );
    }
}
