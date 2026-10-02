//! The dogs sub-screen under a sheep pane: the probe that fills it, its
//! keys, and the table write it and the per-sheep table pane both send.

use super::*;

impl App {
    /// `Enter` or `e` on a sheep pane's `dogs` row: probe every dog for its
    /// per-sheep schema, as a new ask. [`None`] on any other row, and on any
    /// other pane.
    pub(super) fn dogs_row_effect(&mut self) -> Option<Effect> {
        let pane = self.config_pane()?;
        let PaneTarget::Sheep { name } = pane.target() else {
            return None;
        };
        let PaneRow::Field(index) = pane.cursor()? else {
            return None;
        };
        if pane.fields().fields().get(index)?.key != "dogs" {
            return None;
        }
        let sheep = name.clone();
        self.sheep_dogs_ask += 1;
        Some(Effect::LoadSheepDogs {
            sheep,
            ask: self.sheep_dogs_ask,
        })
    }

    /// One [`Msg::SheepDogs`]: opens the sub-screen over the pane that
    /// asked.
    ///
    /// Guarded on the pane's own sheep the way [`Self::on_dog_section`] is
    /// guarded on its dog: an answer for a sheep the operator has left opens
    /// nothing. Nor does one landing over an editor or a sub-screen opened
    /// while the probe ran, which would leave two things owning the keys, or
    /// one a newer ask has superseded.
    pub(super) fn on_sheep_dogs(
        &mut self,
        sheep: &str,
        ask: u64,
        dogs: Vec<SheepDogEntry>,
    ) -> Effect {
        let latest = ask == self.sheep_dogs_ask;
        let Some(pane) = self.config_pane_mut() else {
            return Effect::None;
        };
        let asked = latest && matches!(pane.target(), PaneTarget::Sheep { name } if name == sheep);
        let busy = pane.typing().is_some()
            || pane.env_typing().is_some()
            || pane.list().is_some()
            || pane.dogs().is_some();
        if asked && !busy {
            pane.open_dogs(dogs);
            self.notice = None;
        }
        Effect::None
    }

    /// The dogs sub-screen's own keymap, in force while the pane holds one.
    ///
    /// `Escape` closes the sub-screen and leaves the sheep pane up. `Enter`
    /// or `e` opens the dog's table pane; `d` arms a removal, which `Enter`
    /// confirms and every other key but a quit cancels, the secrets pane's
    /// rule. An armed removal eats the first `Escape`, as it does there.
    pub(super) fn on_dogs_key(&mut self, key: KeyPress) -> Effect {
        // Disarms as it asks: every key but Enter and a quit cancels an
        // armed removal before its own arm runs.
        let was_armed = !matches!(key, KeyPress::Confirm | KeyPress::Quit)
            && self.dogs_mut().is_some_and(DogsPane::disarm);
        match key {
            KeyPress::Quit => return Effect::Quit,
            KeyPress::Escape => {
                if !was_armed && let Some(pane) = self.config_pane_mut() {
                    pane.close_dogs();
                }
            }
            KeyPress::SelectUp
            | KeyPress::SelectDown
            | KeyPress::SelectFirst
            | KeyPress::SelectLast => {
                if let Some(dogs) = self.dogs_mut() {
                    match key {
                        KeyPress::SelectUp => dogs.move_by(-1),
                        KeyPress::SelectDown => dogs.move_by(1),
                        KeyPress::SelectFirst => dogs.move_to_first(),
                        KeyPress::SelectLast => dogs.move_to_last(),
                        _ => unreachable!(),
                    }
                }
            }
            KeyPress::Refresh => return self.reread_pane(),
            KeyPress::Confirm => {
                if self
                    .config_pane()
                    .and_then(ConfigPane::dogs)
                    .is_some_and(|dogs| dogs.armed().is_some())
                {
                    return self.confirm_table_removal();
                }
                return self.open_sheep_dog_pane();
            }
            KeyPress::Edit => return self.open_sheep_dog_pane(),
            KeyPress::Remove => return self.arm_table_removal(),
            KeyPress::Help => {
                self.open_keymap();
            }
            KeyPress::Action(_)
            | KeyPress::Cycle
            | KeyPress::Settings
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::StepUp
            | KeyPress::StepDown
            | KeyPress::FoldView
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::SecretDelete
            | KeyPress::Collapse
            | KeyPress::Continue
            | KeyPress::Undo
            | KeyPress::NextGroup
            | KeyPress::PrevGroup
            | KeyPress::Group(_) => {}
            KeyPress::StreamCycle
            | KeyPress::LevelCycle
            | KeyPress::PageDown
            | KeyPress::PageUp
            | KeyPress::FollowToggle
            | KeyPress::WrapToggle
            | KeyPress::MatchNext
            | KeyPress::MatchPrev
            | KeyPress::Bleats => {}
        }
        Effect::None
    }

    /// Opens the per-sheep table pane on the dog under the cursor, over the
    /// table the sheep pane already holds. Nothing is asked of the shepherd.
    ///
    /// Refused while the sheep pane holds unwritten edits: the new pane
    /// replaces it, and its edits would go with it unwritten.
    fn open_sheep_dog_pane(&mut self) -> Effect {
        let Some(pane) = self.config_pane() else {
            return Effect::None;
        };
        let PaneTarget::Sheep { name: sheep } = pane.target() else {
            return Effect::None;
        };
        let Some(row) = pane.dogs().and_then(DogsPane::cursor_row) else {
            return Effect::None;
        };
        let dog = row.name().to_owned();
        let Some(schema) = row.entry().schema.clone() else {
            self.notice = Some(Notice {
                text: format!("{dog} publishes no sheep schema, so its table is not edited here"),
                grave: true,
            });
            return Effect::None;
        };
        if !pane.edits().is_empty() {
            self.notice = Some(Notice {
                text: format!(
                    "{sheep} has unwritten edits: esc back to its pane to write or undo them before {dog} opens"
                ),
                grave: true,
            });
            return Effect::None;
        }
        let opened =
            ConfigPane::sheep_dog(sheep.clone(), dog.clone(), &schema, pane.sheep_table(&dog));
        self.body = Body::ConfigPane(opened);
        self.release_text_mode_if_unowned();
        Effect::None
    }

    /// `d` on the sub-screen: arms the removal of the table under the
    /// cursor. A row with no table arms nothing and says nothing; a closed
    /// gate refuses in its own words.
    fn arm_table_removal(&mut self) -> Effect {
        let has_table = self
            .config_pane()
            .and_then(ConfigPane::dogs)
            .and_then(DogsPane::cursor_row)
            .is_some_and(|row| row.state() != DogTableState::Unset);
        if !has_table || self.authorize_write().is_none() {
            return Effect::None;
        }
        if let Some(dogs) = self.dogs_mut() {
            dogs.arm_removal();
        }
        Effect::None
    }

    /// `Enter` with a removal armed: sends it and disarms.
    fn confirm_table_removal(&mut self) -> Effect {
        let Some(pane) = self.config_pane_mut() else {
            return Effect::None;
        };
        let PaneTarget::Sheep { name } = pane.target().clone() else {
            return Effect::None;
        };
        let Some(dogs) = pane.dogs_mut() else {
            return Effect::None;
        };
        let Some(dog) = dogs.armed().map(str::to_owned) else {
            return Effect::None;
        };
        dogs.disarm();
        let Some(authority) = self.authorize_write() else {
            return Effect::None;
        };
        let ticket = self.take_write_ticket();
        Effect::Send(Sent::SetSheepDogTable {
            name,
            dog,
            ticket,
            table: None,
            authority,
        })
    }

    /// A sheep's re-read landing under one of its dogs' table panes: the
    /// pane takes that table's current value in place, cursor and edits
    /// kept. `false`, and nothing touched, when no such pane is up.
    pub(super) fn refresh_sheep_dog_pane(&mut self, view: &SheepConfigView) -> bool {
        let Some(pane) = self.config_pane_mut() else {
            return false;
        };
        let PaneTarget::SheepDog { sheep, dog, .. } = pane.target() else {
            return false;
        };
        if *sheep != view.name {
            return false;
        }
        let table = view
            .config
            .dogs
            .get(dog)
            .map(|table| table.as_map().clone())
            .unwrap_or_default();
        pane.adopt_table(table);
        self.release_text_mode_if_unowned();
        true
    }

    /// The open sub-screen, for the keys that move or arm on it.
    fn dogs_mut(&mut self) -> Option<&mut DogsPane> {
        self.config_pane_mut()?.dogs_mut()
    }

    /// One `Request::SetSheepDogSettings` reply, for a removal or for a
    /// table pane's close.
    ///
    /// A success re-reads the sheep, so an open sub-screen shows the table
    /// gone; [`Self::on_sheep_config`] drops the answer when no pane is
    /// waiting for it. Neither arm can print a value: the reply carries
    /// none, and `removed` comes off the request.
    pub(super) fn on_table_set(
        &mut self,
        name: &str,
        dog: &str,
        removed: bool,
        result: Result<Response, RequestError>,
    ) -> Effect {
        let why = match result {
            Ok(Response::SheepDogSettingsSet { .. }) => {
                let verb = if removed { "removed" } else { "written" };
                self.notice = Some(Notice {
                    text: format!("{name}: its {dog} table is {verb}, and {dog} is told"),
                    grave: false,
                });
                return Effect::Send(Sent::SheepConfig {
                    name: name.to_owned(),
                });
            }
            Ok(_unrecognised) => {
                "the shepherd answered something this lookout does not understand".to_owned()
            }
            Err(RequestError::Rpc(err)) => err.message,
            Err(other) => other.to_string(),
        };
        self.notice = Some(Notice {
            text: format!("{name}: {dog}: {why}"),
            grave: true,
        });
        Effect::None
    }

    /// A table write the link task never took.
    pub(super) fn on_table_unsent(&mut self, name: &str, dog: &str) -> Effect {
        self.notice = Some(Notice {
            text: format!("{name}: its {dog} table was not sent"),
            grave: true,
        });
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use shep_core::protocol::{RpcError, RpcErrorCode};

    use super::*;
    use crate::lookout::app::testing::*;

    /// `d` there would file `dogs = null`, and the close would then drop
    /// every table the sheep carries in one write.
    #[test]
    fn d_or_space_on_the_dogs_row_files_nothing_and_points_at_enter() {
        for key in [KeyPress::Remove, KeyPress::Cycle] {
            let mut app = app_in_web(Control::Allowed);
            assert_eq!(app.update(Msg::Key(key)), Effect::None);
            assert!(app.config_pane().unwrap().edits().is_empty(), "{key:?}");
            assert_eq!(
                app.notice().expect("refused").to_string(),
                "dogs opens its own screen: press enter"
            );
        }
    }

    #[test]
    fn enter_or_e_on_the_dogs_row_probes_and_the_answer_opens_the_list() {
        for key in [KeyPress::Confirm, KeyPress::Edit] {
            let mut app = app_in_web(Control::ReadOnly);
            let ask = ask_dogs(&mut app, key);
            assert!(
                app.config_pane().unwrap().dogs().is_none(),
                "not on the key"
            );
            let waiting = app.notice().expect("the probe's wait is said");
            assert!(!waiting.grave, "{waiting:?}");
            answer(&mut app, "web", ask);
            let names: Vec<&str> = dogs(&app).rows().iter().map(|row| row.name()).collect();
            assert_eq!(names, ["deploy", "jobs", "legacy"], "{key:?}");
            assert!(app.notice().is_none(), "the list replaces the wait");
        }
    }

    #[test]
    fn an_answer_for_a_sheep_the_pane_has_left_opens_nothing() {
        let mut app = app_in_web(Control::Allowed);
        let ask = ask_dogs(&mut app, KeyPress::Confirm);
        answer(&mut app, "api", ask);
        assert!(app.config_pane().unwrap().dogs().is_none(), "another sheep");

        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none());
        answer(&mut app, "web", ask);
        assert!(
            app.config_pane().is_none(),
            "a late answer re-opens nothing"
        );
        assert!(app.notice().is_none(), "silently: nothing went wrong");
    }

    #[test]
    fn j_and_k_move_and_esc_closes_only_the_sub_screen() {
        let mut app = app_in_dogs(Control::Allowed);
        assert_eq!(
            dogs(&app).cursor_row().map(|row| row.name()),
            Some("deploy")
        );
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(dogs(&app).cursor_row().map(|row| row.name()), Some("jobs"));
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        assert_eq!(
            dogs(&app).cursor_row().map(|row| row.name()),
            Some("deploy")
        );
        assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::None);
        let pane = app.config_pane().expect("the sheep pane stays");
        assert!(pane.dogs().is_none());
        assert_eq!(pane.target().name(), "web");
    }

    #[test]
    fn enter_on_a_dog_with_a_schema_opens_its_table_without_asking_again() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "jobs");
        assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
        let pane = app.config_pane().expect("a pane");
        assert_eq!(
            pane.target(),
            &PaneTarget::SheepDog {
                sheep: "web".into(),
                dog: "jobs".into(),
            }
        );
        assert_eq!(pane.value("concurrency"), "2");
    }

    #[test]
    fn a_read_only_dog_opens_nothing_and_says_why() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "legacy");
        assert_eq!(app.update(Msg::Key(KeyPress::Edit)), Effect::None);
        assert!(app.config_pane().unwrap().dogs().is_some(), "still listed");
        let notice = app.notice().expect("a refusal").to_string();
        assert!(notice.contains("no sheep schema"), "{notice}");
        assert!(!notice.contains(PASSWORD), "{notice}");
    }

    #[test]
    fn enter_on_a_dog_refuses_while_the_sheep_pane_holds_edits() {
        let mut app = app_in_web(Control::Allowed);
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
        pane_to(&mut app, "dogs");
        let ask = ask_dogs(&mut app, KeyPress::Confirm);
        answer(&mut app, "web", ask);
        cursor_on(&mut app, "jobs");
        assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
        let pane = app.config_pane().expect("the sheep pane stays");
        assert!(matches!(pane.target(), PaneTarget::Sheep { .. }));
        assert_eq!(pane.edits().len(), 1, "the edit is still filed");
        let notice = app.notice().expect("a refusal").to_string();
        assert_eq!(
            notice,
            "web has unwritten edits: esc back to its pane to write or undo them before jobs opens"
        );
    }

    #[test]
    fn d_arms_and_enter_sends_the_removal() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "jobs");
        assert_eq!(app.update(Msg::Key(KeyPress::Remove)), Effect::None);
        assert_eq!(dogs(&app).armed(), Some("jobs"));
        let sent = app.update(Msg::Key(KeyPress::Confirm));
        let Effect::Send(Sent::SetSheepDogTable {
            ref name,
            ref dog,
            table: None,
            ..
        }) = sent
        else {
            panic!("the confirm sends the removal: {sent:?}");
        };
        assert_eq!((name.as_str(), dog.as_str()), ("web", "jobs"));
        assert_eq!(
            wire(sent),
            Request::SetSheepDogSettings {
                name: "web".into(),
                dog: "jobs".into(),
                table: None,
            }
        );
        assert_eq!(dogs(&app).armed(), None);
    }

    #[test]
    fn any_other_key_disarms_and_the_first_esc_is_eaten() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "jobs");
        let _ = app.update(Msg::Key(KeyPress::Remove));
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(dogs(&app).armed(), None);
        assert_eq!(
            dogs(&app).cursor_row().map(|row| row.name()),
            Some("legacy")
        );

        let _ = app.update(Msg::Key(KeyPress::Remove));
        assert_eq!(dogs(&app).armed(), Some("legacy"));
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert_eq!(dogs(&app).armed(), None);
        assert!(app.config_pane().unwrap().dogs().is_some(), "still up");
    }

    #[test]
    fn a_row_with_no_table_arms_nothing() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "deploy");
        let _ = app.update(Msg::Key(KeyPress::Remove));
        assert_eq!(dogs(&app).armed(), None);
        assert!(app.notice().is_none(), "{:?}", app.notice());
        assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
    }

    #[test]
    fn a_closed_gate_refuses_the_removal_in_its_own_words() {
        let mut app = app_in_dogs(Control::ReadOnly);
        cursor_on(&mut app, "jobs");
        assert_eq!(app.update(Msg::Key(KeyPress::Remove)), Effect::None);
        assert_eq!(dogs(&app).armed(), None);
        assert_eq!(
            app.notice().map(ToString::to_string),
            Some(READ_ONLY_REFUSAL.to_string())
        );
    }

    #[test]
    fn a_landed_removal_notices_and_the_re_read_refreshes_the_list() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "jobs");
        let _ = app.update(Msg::Key(KeyPress::Remove));
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("the removal goes out");
        };
        let effect = app.update(Msg::Replied {
            sent,
            result: Ok(Response::SheepDogSettingsSet {
                name: "web".into(),
                dog: "jobs".into(),
            }),
        });
        assert_eq!(
            effect,
            Effect::Send(Sent::SheepConfig { name: "web".into() })
        );
        let notice = app.notice().expect("reported").to_string();
        assert_eq!(notice, "web: its jobs table is removed, and jobs is told");

        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig { name: "web".into() },
            result: Ok(Response::SheepConfig(Box::new(web_view(false)))),
        });
        let jobs = dogs(&app)
            .rows()
            .iter()
            .find(|row| row.name() == "jobs")
            .expect("jobs still has a schema");
        assert_eq!(jobs.state(), DogTableState::Unset);
        assert_eq!(dogs(&app).cursor_row().map(|row| row.name()), Some("jobs"));
    }

    #[test]
    fn a_refused_write_lands_its_message_and_no_value() {
        let mut app = app_in_dogs(Control::Allowed);
        let table: serde_json::Map<String, serde_json::Value> =
            json!({ "token": TOKEN }).as_object().cloned().unwrap();
        let sent = Sent::SetSheepDogTable {
            name: "web".into(),
            dog: "jobs".into(),
            ticket: 7,
            table: Some(table.into()),
            authority: WriteAuthority::granted(&app).expect("the gate is open"),
        };
        assert!(!format!("{sent:?}").contains(TOKEN), "{sent:?}");
        let effect = app.update(Msg::Replied {
            sent: sent.clone(),
            result: Err(RequestError::Rpc(RpcError {
                code: RpcErrorCode::NotFound,
                message: "no sheep named web".into(),
                daemon_version: None,
            })),
        });
        assert_eq!(effect, Effect::None, "a refusal does not re-read");
        let notice = app.notice().expect("reported");
        assert!(notice.is_grave());
        assert_eq!(notice.to_string(), "web: jobs: no sheep named web");

        let _ = app.update(Msg::Unsent { sent });
        let notice = app.notice().expect("reported").to_string();
        assert_eq!(notice, "web: its jobs table was not sent");
    }

    /// `Msg` derives `Debug`, and a probe answer carries each dog's schema,
    /// defaults included (IR-41).
    #[test]
    fn a_probe_answers_debug_names_no_schema() {
        let msg = Msg::SheepDogs {
            sheep: "web".into(),
            ask: 1,
            dogs: vec![SheepDogEntry {
                name: "jobs".into(),
                adopted_path: None,
                schema: Some(json!({ "default": TOKEN })),
            }],
        };
        assert!(!format!("{msg:?}").contains(TOKEN), "{msg:?}");
    }

    /// `SetSheepDogSettings` replaces the table, so the one write carries
    /// the secret the operator never touched exactly as it was.
    #[test]
    fn closing_a_table_pane_sends_the_whole_table_once_and_lands_on_the_dashboard() {
        let mut app = app_in_jobs_table();
        type_concurrency(&mut app, "4");
        let effect = close_jobs_table(&mut app);
        assert!(!format!("{effect:?}").contains(TOKEN), "{effect:?}");
        let batch = wire_batch(effect);
        let [
            Sent::SetSheepDogTable {
                name,
                dog,
                table: Some(table),
                ..
            },
        ] = batch.as_slice()
        else {
            panic!("one table write: {batch:?}");
        };
        assert_eq!((name.as_str(), dog.as_str()), ("web", "jobs"));
        assert_eq!(
            serde_json::Value::Object(table.as_map().clone()),
            json!({ "concurrency": 4, "token": TOKEN })
        );
        assert!(
            matches!(app.body(), Body::FlockTable),
            "esc lands on the dashboard"
        );
    }

    #[test]
    fn closing_a_table_pane_with_no_edits_sends_nothing() {
        let mut app = app_in_jobs_table();
        assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::None);
        assert!(matches!(app.body(), Body::FlockTable));
    }

    #[test]
    fn a_landed_table_write_says_so_and_names_no_value() {
        let mut app = app_in_jobs_table();
        type_concurrency(&mut app, "4");
        let mut batch = wire_batch(close_jobs_table(&mut app));
        let effect = app.update(Msg::Replied {
            sent: batch.remove(0),
            result: Ok(Response::SheepDogSettingsSet {
                name: "web".into(),
                dog: "jobs".into(),
            }),
        });
        assert_eq!(
            effect,
            Effect::Send(Sent::SheepConfig { name: "web".into() })
        );
        let notice = app.notice().expect("reported").to_string();
        assert_eq!(notice, "web: its jobs table is written, and jobs is told");
    }

    /// The sheep is the table's owner, so `r` re-reads the sheep and the
    /// pane takes its table from the answer, cursor and edits kept.
    #[test]
    fn r_re_reads_the_sheep_and_rebuilds_the_table_keeping_the_cursor() {
        let mut app = app_in_jobs_table();
        type_concurrency(&mut app, "4");
        pane_to(&mut app, "token");
        let cursor = app.config_pane().unwrap().view().cursor();
        assert_ne!(cursor, 0, "a cursor a reset would move");
        assert_eq!(
            app.update(Msg::Key(KeyPress::Refresh)),
            Effect::Send(Sent::SheepConfig { name: "web".into() })
        );
        let mut view = web_view(true);
        let moved = json!({ "concurrency": 9, "token": "rotated" });
        view.config.dogs.insert(
            "jobs".into(),
            moved.as_object().cloned().expect("a table").into(),
        );
        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig { name: "web".into() },
            result: Ok(Response::SheepConfig(Box::new(view))),
        });
        let pane = app.config_pane().expect("still open");
        assert!(
            matches!(pane.target(), PaneTarget::SheepDog { .. }),
            "{pane:?}"
        );
        assert_eq!(pane.value("concurrency"), "9");
        assert_eq!(pane.view().cursor(), cursor);
        assert_eq!(pane.edits().len(), 1, "the operator's edit survives");
    }

    /// Called directly: `on_sheep_config` already drops a reply for a sheep
    /// nobody asked about, so through `Msg` this guard is never the one
    /// that holds. Without it, closing the pane would write `api`'s table
    /// to `web`.
    #[test]
    fn a_re_read_of_another_sheep_leaves_the_table_pane_alone() {
        let mut app = app_in_jobs_table();
        let mut view = web_view(true);
        view.name = "api".into();
        let other = json!({ "concurrency": 64 });
        view.config.dogs.insert(
            "jobs".into(),
            other.as_object().cloned().expect("a table").into(),
        );
        assert!(!app.refresh_sheep_dog_pane(&view));
        assert_eq!(app.config_pane().unwrap().value("concurrency"), "2");
    }

    /// An answer opens nothing over an editor opened while the probe ran,
    /// and rebuilds nothing under a list already up: either would take the
    /// keys from what holds them.
    #[test]
    fn an_answer_over_an_editor_or_an_open_list_opens_nothing() {
        let mut app = app_in_web(Control::Allowed);
        let ask = ask_dogs(&mut app, KeyPress::Confirm);
        pane_to(&mut app, "cwd");
        let _ = app.update(Msg::Key(KeyPress::Edit));
        assert!(app.config_pane().unwrap().typing().is_some(), "an editor");
        answer(&mut app, "web", ask);
        let pane = app.config_pane().unwrap();
        assert!(pane.dogs().is_none() && pane.typing().is_some(), "{pane:?}");

        let mut app = app_in_web(Control::Allowed);
        let ask = ask_dogs(&mut app, KeyPress::Confirm);
        answer(&mut app, "web", ask);
        cursor_on(&mut app, "legacy");
        answer(&mut app, "web", ask);
        assert_eq!(
            dogs(&app).cursor_row().map(|row| row.name()),
            Some("legacy")
        );
    }

    /// Two quick Enters ask twice, and either answer can land first. Only
    /// the second ask's may open the list: the first, landing after the
    /// list closed again, would reopen it with the older schemas.
    #[test]
    fn only_the_newest_asks_answer_opens_the_list() {
        let mut app = app_in_web(Control::Allowed);
        let first = ask_dogs(&mut app, KeyPress::Confirm);
        let second = ask_dogs(&mut app, KeyPress::Confirm);
        answer(&mut app, "web", first);
        assert!(app.config_pane().unwrap().dogs().is_none(), "superseded");
        answer(&mut app, "web", second);
        assert!(app.config_pane().unwrap().dogs().is_some(), "the newest");
        let _ = app.update(Msg::Key(KeyPress::Escape));
        answer(&mut app, "web", first);
        assert!(app.config_pane().unwrap().dogs().is_none(), "late");
    }

    /// A probe outlives its pane: Enter, close, reopen the same sheep, and
    /// the first pane's answer lands on the second, which asked nothing.
    #[test]
    fn an_answer_for_a_closed_pane_opens_nothing_on_its_reopening() {
        let mut app = app_in_web(Control::Allowed);
        let ask = ask_dogs(&mut app, KeyPress::Confirm);
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none());
        let _ = app.update(Msg::Key(KeyPress::Edit));
        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig { name: "web".into() },
            result: Ok(Response::SheepConfig(Box::new(web_view(true)))),
        });
        assert!(app.config_pane().is_some(), "reopened");
        answer(&mut app, "web", ask);
        assert!(app.config_pane().unwrap().dogs().is_none());
    }

    #[test]
    fn a_reply_of_the_wrong_kind_is_reported_and_re_reads_nothing() {
        let mut app = app_in_dogs(Control::Allowed);
        let effect = app.update(Msg::Replied {
            sent: Sent::SetSheepDogTable {
                name: "web".into(),
                dog: "jobs".into(),
                ticket: 7,
                table: None,
                authority: WriteAuthority::granted(&app).expect("the gate is open"),
            },
            result: Ok(Response::SheepConfig(Box::new(web_view(true)))),
        });
        assert_eq!(effect, Effect::None);
        let notice = app.notice().expect("reported");
        assert!(notice.is_grave());
        assert_eq!(
            notice.to_string(),
            "web: jobs: the shepherd answered something this lookout does not understand"
        );
    }
}
