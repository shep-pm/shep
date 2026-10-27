//! The dogs tab under a sheep pane: the listing that fills it, a dog's row
//! opening to its table, the keys on both, and the table write the tab's
//! removal sends.
//!
//! An open dog's table is a pane of its own, held by the tab. A key that
//! lands inside one runs against that pane through [`App::in_table`], which
//! seats it as the body for the length of the handler, so every handler
//! written for a pane edits a table inline as it did on a pane of its own.

use super::*;

impl App {
    /// The ask for the dogs listing, the first time the `dogs` tab is up.
    /// [`Effect::None`] on any other tab, and once it has been asked.
    pub(super) fn dogs_tab_effect(&mut self) -> Effect {
        let Some(pane) = self.config_pane() else {
            return Effect::None;
        };
        if !pane.shows_dogs() || !pane.dogs_tab().needs_listing() {
            return Effect::None;
        }
        let sheep = pane.target().name().to_owned();
        if self.refuse_over_host() {
            return Effect::None;
        }
        if let Some(pane) = self.config_pane_mut() {
            pane.dogs_tab_mut().start_listing();
        }
        self.sheep_dogs_ask += 1;
        Effect::ListSheepDogs {
            sheep,
            ask: self.sheep_dogs_ask,
        }
    }

    /// One [`Msg::SheepDogNames`]: lists the dogs on the tab that asked, every
    /// row closed.
    ///
    /// Guarded on the pane's own sheep the way [`Self::on_dog_section`] is
    /// guarded on its dog, and on the ask: closing the pane moves it on, so
    /// an answer outlives neither the pane nor a newer ask.
    pub(super) fn on_sheep_dog_names(
        &mut self,
        sheep: &str,
        ask: u64,
        dogs: Vec<ListedDog>,
    ) -> Effect {
        let latest = ask == self.sheep_dogs_ask;
        if let Some(pane) = self.config_pane_mut()
            && latest
            && matches!(pane.target(), PaneTarget::Sheep { name } if name == sheep)
        {
            pane.list_dogs(dogs);
        }
        Effect::None
    }

    /// One [`Msg::SheepDog`]: the dog's schema has come, so its row, if it
    /// was waiting, opens. A dog that publishes no sheep schema opens
    /// nothing and says so.
    pub(super) fn on_sheep_dog(&mut self, sheep: &str, entry: SheepDogEntry) -> Effect {
        let Some(pane) = self.config_pane_mut() else {
            return Effect::None;
        };
        if !matches!(pane.target(), PaneTarget::Sheep { name } if name == sheep) {
            return Effect::None;
        }
        if let Some(DogToggle::NoSchema(dog)) = pane.answer_dog(entry) {
            self.notice = Some(no_schema_notice(&dog));
        }
        Effect::None
    }

    /// The `dogs` tab's own keys, ahead of the pane's: an armed removal, a
    /// dog's row, and a key landing inside an open table. [`None`] for every
    /// key the pane's keymap answers the same way here as on any tab:
    /// movement, `Escape`, `r`, the tab keys. A table with its list open
    /// takes every key, those too.
    ///
    /// `d` arms a removal, which `Enter` confirms and every other key but a
    /// quit cancels, the secrets pane's rule. An armed removal eats the first
    /// `Escape`, as it does there.
    pub(super) fn on_dogs_tab_key(&mut self, key: KeyPress) -> Option<Effect> {
        let pane = self.config_pane()?;
        if !pane.shows_dogs() {
            return None;
        }
        let armed = pane.dogs_tab().armed().is_some();
        if armed && !matches!(key, KeyPress::Confirm | KeyPress::Quit) {
            self.config_pane_mut()?.dogs_tab_mut().disarm();
            if key == KeyPress::Escape {
                return Some(Effect::None);
            }
        }
        let pane = self.config_pane()?;
        // An open list owns the keyboard, movement and `Escape` included.
        let list_open = pane.focus().list().is_some();
        match pane.cursor()? {
            PaneRow::Dog(dog) => self.on_dog_row_key(key, dog),
            PaneRow::Under { .. } if list_open || lands_in_table(key) => Some(
                self.in_table(|app| app.on_pane_key(key))
                    .unwrap_or(Effect::None),
            ),
            _ => None,
        }
    }

    /// A key with the cursor on `dog`'s row.
    fn on_dog_row_key(&mut self, key: KeyPress, dog: usize) -> Option<Effect> {
        let armed = self.config_pane()?.dogs_tab().armed().is_some();
        let effect = match key {
            KeyPress::Confirm if armed => self.confirm_table_removal(),
            KeyPress::Confirm | KeyPress::Edit => self.toggle_dog_row(dog, None),
            KeyPress::TabNext => self.toggle_dog_row(dog, Some(true)),
            KeyPress::TabPrev => self.toggle_dog_row(dog, Some(false)),
            KeyPress::Remove => self.arm_table_removal(dog),
            // `space` files nothing on a row that holds no value.
            KeyPress::Cycle => Effect::None,
            _ => return None,
        };
        Some(effect)
    }

    /// Opens the row at `dog` if it is closed, else closes it; with `want`
    /// set, only toward that state. An adopted dog's first opening sends its
    /// probe.
    fn toggle_dog_row(&mut self, dog: usize, want: Option<bool>) -> Effect {
        let Some(pane) = self.config_pane() else {
            return Effect::None;
        };
        let Some(row) = pane.dogs_tab().rows().get(dog) else {
            return Effect::None;
        };
        if want.is_some_and(|open| open == row.is_open()) {
            return Effect::None;
        }
        let probes = !row.is_open() && row.needs_probe();
        let sheep = pane.target().name().to_owned();
        // An adopted dog's binary is on the shepherd's machine.
        if probes && self.refuse_over_host() {
            return Effect::None;
        }
        let Some(pane) = self.config_pane_mut() else {
            return Effect::None;
        };
        match pane.toggle_dog(dog) {
            DogToggle::Ask { dog, adopted_path } => Effect::LoadSheepDog {
                sheep,
                dog,
                adopted_path,
            },
            DogToggle::NoSchema(dog) => {
                self.notice = Some(no_schema_notice(&dog));
                Effect::None
            }
            DogToggle::Closed | DogToggle::Opened | DogToggle::Waiting => Effect::None,
        }
    }

    /// `d` on `dog`'s row: arms the removal of its table. A row with no
    /// table arms nothing and says nothing; a closed gate refuses in its own
    /// words.
    fn arm_table_removal(&mut self, dog: usize) -> Effect {
        let has_table = self
            .config_pane()
            .and_then(|pane| pane.dogs_tab().rows().get(dog))
            .is_some_and(DogRow::has_table);
        if !has_table || self.authorize_write().is_none() {
            return Effect::None;
        }
        if let Some(pane) = self.config_pane_mut() {
            pane.dogs_tab_mut().arm_removal(dog);
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
        let tab = pane.dogs_tab_mut();
        let Some(dog) = tab.armed().map(str::to_owned) else {
            return Effect::None;
        };
        tab.disarm();
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

    /// Runs `run` with the open table the cursor is inside seated as the
    /// body, then puts it back and moves the sheep pane's cursor to the row
    /// the table's own is on. [`None`] when the cursor is not in a table.
    pub(super) fn in_table<R>(&mut self, run: impl FnOnce(&mut Self) -> R) -> Option<R> {
        let dog = self.config_pane()?.focused_dog()?;
        self.in_dog_table(dog, run)
    }

    /// [`Self::in_table`] for the table of the dog at `dog`.
    pub(super) fn in_dog_table<R>(
        &mut self,
        dog: usize,
        run: impl FnOnce(&mut Self) -> R,
    ) -> Option<R> {
        let table = self.config_pane_mut()?.dogs_tab_mut().take_table(dog)?;
        let sheep = core::mem::replace(&mut self.body, Body::ConfigPane(*table));
        let out = run(self);
        if let Body::ConfigPane(table) = core::mem::replace(&mut self.body, sheep)
            && let Some(pane) = self.config_pane_mut()
        {
            pane.dogs_tab_mut().seat_table(dog, Box::new(table));
            pane.pull_focus(dog);
        }
        Some(out)
    }

    /// One `Request::SetSheepDogSettings` reply, for a removal or for a
    /// table's write on close.
    ///
    /// A success re-reads the sheep, so an open tab shows the table
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
                self.notice = Some(Notice::info(format!(
                    "{name}: its {dog} table is {verb}, and {dog} is told"
                )));
                return Effect::Send(Sent::SheepConfig {
                    name: name.to_owned(),
                });
            }
            other => failed_reply(other),
        };
        self.notice = Some(Notice::grave(format!("{name}: {dog}: {why}")));
        Effect::None
    }

    /// A table write the link task never took.
    pub(super) fn on_table_unsent(&mut self, name: &str, dog: &str) -> Effect {
        self.notice = Some(Notice::grave(format!(
            "{name}: its {dog} table was not sent"
        )));
        Effect::None
    }
}

/// Whether a key with the cursor inside an open table is the table's to
/// answer. Movement, `Escape`, `r`, the help overlay and the tab keys are the
/// sheep pane's, because the table is only part of what they walk.
fn lands_in_table(key: KeyPress) -> bool {
    !matches!(
        key,
        KeyPress::Quit
            | KeyPress::Escape
            | KeyPress::Refresh
            | KeyPress::Help
            | KeyPress::SelectUp
            | KeyPress::SelectDown
            | KeyPress::SelectFirst
            | KeyPress::SelectLast
            | KeyPress::NextGroup
            | KeyPress::PrevGroup
            | KeyPress::Group(_)
    )
}

fn no_schema_notice(dog: &str) -> Notice {
    Notice::grave(format!(
        "{dog} publishes no sheep schema, so its table is not edited here"
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use shep_core::protocol::{RpcError, RpcErrorCode};

    use super::*;
    use crate::lookout::app::testing::*;
    use crate::lookout::pane::{DogTableState, ListRow};

    fn dog_names(app: &App) -> Vec<&str> {
        app.config_pane()
            .expect("a pane")
            .dogs_tab()
            .rows()
            .iter()
            .map(DogRow::name)
            .collect()
    }

    fn row<'a>(app: &'a App, dog: &str) -> &'a DogRow {
        app.config_pane()
            .expect("a pane")
            .dogs_tab()
            .rows()
            .iter()
            .find(|row| row.name() == dog)
            .unwrap_or_else(|| panic!("{dog} is not listed"))
    }

    fn cursor_dog(app: &App) -> Option<&str> {
        let pane = app.config_pane()?;
        match pane.cursor()? {
            PaneRow::Dog(index) => pane.dogs_tab().rows().get(index).map(DogRow::name),
            _ => None,
        }
    }

    #[test]
    fn the_dogs_tab_opens_with_every_dog_collapsed_and_no_probe_sent() {
        let mut app = app_in_web(Control::Allowed);
        let ask = ask_dogs(&mut app);
        let effect = app.update(Msg::SheepDogNames {
            sheep: "web".into(),
            ask,
            dogs: listing(),
        });
        assert_eq!(effect, Effect::None, "the listing probes no dog");
        assert_eq!(dog_names(&app), ["deploy", "jobs", "legacy"]);
        for dog in ["deploy", "jobs", "legacy"] {
            assert!(!row(&app, dog).is_open(), "{dog} starts collapsed");
        }
        assert!(
            app.config_pane()
                .expect("a pane")
                .rows()
                .iter()
                .all(|row| matches!(row, PaneRow::Dog(_))),
            "no table is drawn yet"
        );
    }

    #[test]
    fn opening_a_dog_sends_one_probe_and_its_table_draws_inline() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "jobs");
        assert_eq!(
            app.update(Msg::Key(KeyPress::Confirm)),
            Effect::LoadSheepDog {
                sheep: "web".into(),
                dog: "jobs".into(),
                adopted_path: Some("/opt/jobs".into()),
            }
        );
        assert_eq!(
            app.update(Msg::SheepDog {
                sheep: "web".into(),
                entry: jobs_entry(),
            }),
            Effect::None
        );
        assert!(row(&app, "jobs").is_open());
        let pane = app.config_pane().expect("a pane");
        assert!(
            pane.rows()
                .iter()
                .any(|row| matches!(row, PaneRow::Under { .. })),
            "the table's rows sit under the dog's"
        );
        assert_eq!(
            row(&app, "jobs")
                .table()
                .expect("built")
                .value("concurrency"),
            "2"
        );

        // Closed and opened again, the answer is kept: no second probe.
        assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
        assert!(!row(&app, "jobs").is_open());
        assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
        assert!(row(&app, "jobs").is_open());
    }

    /// `SetSheepDogSettings` replaces the table, so the one write carries
    /// the secret the operator never touched exactly as it was.
    #[test]
    fn an_edit_inside_an_inline_table_writes_the_whole_table_once() {
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
    fn closing_with_a_table_open_and_no_edits_sends_nothing() {
        let mut app = app_in_jobs_table();
        assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::None);
        assert!(matches!(app.body(), Body::FlockTable));
    }

    #[test]
    fn a_table_closed_after_an_edit_still_writes_it() {
        let mut app = app_in_jobs_table();
        type_concurrency(&mut app, "4");
        cursor_on(&mut app, "jobs");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(!row(&app, "jobs").is_open());
        let batch = wire_batch(close_jobs_table(&mut app));
        let [Sent::SetSheepDogTable { dog, .. }] = batch.as_slice() else {
            panic!("one table write: {batch:?}");
        };
        assert_eq!(dog, "jobs");
    }

    /// The list a table's field opens to owns every key, movement and
    /// `Escape` included, the same as on a pane of its own.
    #[test]
    fn a_list_inside_an_inline_table_walks_adds_and_backs_out_one_level() {
        let mut schema = jobs_schema();
        schema["properties"]["tags"] = json!({ "type": "array", "items": { "type": "string" } });
        let mut app = app_in_jobs_table_over(SheepDogEntry {
            schema: Some(schema),
            ..jobs_entry()
        });
        pane_to(&mut app, "tags");
        let _ = app.update(Msg::Key(KeyPress::Edit));
        let list = |app: &App| {
            app.config_pane()
                .expect("a pane")
                .focus()
                .list()
                .map(|list| (list.elements().to_vec(), list.cursor()))
        };
        assert_eq!(list(&app), Some((vec![], Some(ListRow::New))));

        for typed in ['a', 'b'] {
            let _ = app.update(Msg::Key(KeyPress::SelectLast));
            let _ = app.update(Msg::Key(KeyPress::Confirm));
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
            let _ = app.update(Msg::Key(KeyPress::TextApply));
        }
        let _ = app.update(Msg::Key(KeyPress::SelectFirst));
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(
            list(&app),
            Some((vec!["a".into(), "b".into()], Some(ListRow::Item(1))))
        );

        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert_eq!(list(&app), None, "esc closes the list");
        assert!(
            matches!(app.body(), Body::ConfigPane(_)),
            "and leaves the sheep pane up"
        );
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

    #[test]
    fn a_dog_with_no_sheep_schema_opens_nothing_and_says_why() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "legacy");
        assert_eq!(app.update(Msg::Key(KeyPress::Edit)), Effect::None);
        assert!(!row(&app, "legacy").is_open());
        let notice = app.notice().expect("a refusal").to_string();
        assert!(notice.contains("no sheep schema"), "{notice}");
        assert!(!notice.contains(PASSWORD), "{notice}");
    }

    #[test]
    fn a_probe_that_finds_no_schema_leaves_the_row_closed_and_says_why() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "deploy");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::SheepDog {
            sheep: "web".into(),
            entry: SheepDogEntry {
                name: "deploy".into(),
                adopted_path: Some("/opt/deploy".into()),
                schema: None,
            },
        });
        assert!(!row(&app, "deploy").is_open());
        let notice = app.notice().expect("a refusal").to_string();
        assert!(
            notice.contains("deploy publishes no sheep schema"),
            "{notice}"
        );
    }

    #[test]
    fn a_probe_answer_for_another_sheep_opens_nothing() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "jobs");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::SheepDog {
            sheep: "api".into(),
            entry: jobs_entry(),
        });
        assert!(row(&app, "jobs").table().is_none());
    }

    #[test]
    fn the_dogs_tab_lists_once_per_pane() {
        let mut app = app_in_web(Control::Allowed);
        let _ = ask_dogs(&mut app);
        let _ = app.update(Msg::Key(KeyPress::Group(1)));
        assert_eq!(app.update(Msg::Key(KeyPress::Group(9))), Effect::None);
    }

    #[test]
    fn an_answer_for_a_sheep_the_pane_has_left_lists_nothing() {
        let mut app = app_in_web(Control::Allowed);
        let ask = ask_dogs(&mut app);
        answer(&mut app, "api", ask, listing());
        assert!(!app.config_pane().unwrap().dogs_tab().is_listed());

        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none());
        answer(&mut app, "web", ask, listing());
        assert!(app.config_pane().is_none(), "a late answer opens nothing");
        assert!(app.notice().is_none(), "silently: nothing went wrong");
    }

    #[test]
    fn an_answer_for_a_closed_pane_lists_nothing_on_its_reopening() {
        let mut app = app_in_web(Control::Allowed);
        let ask = ask_dogs(&mut app);
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig { name: "web".into() },
            result: Ok(Response::SheepConfig(Box::new(web_view(true)))),
        });
        assert!(app.config_pane().is_some(), "reopened");
        answer(&mut app, "web", ask, listing());
        assert!(!app.config_pane().unwrap().dogs_tab().is_listed());
    }

    /// `d` there would file `dogs = null`, and the close would then drop
    /// every table the sheep carries in one write; `space` files nothing on
    /// a row that holds no value.
    #[test]
    fn space_on_a_dog_row_files_nothing() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "jobs");
        assert_eq!(app.update(Msg::Key(KeyPress::Cycle)), Effect::None);
        assert!(app.config_pane().unwrap().edits().is_empty());
        assert!(app.notice().is_none(), "{:?}", app.notice());
    }

    #[test]
    fn d_arms_and_enter_sends_the_removal() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "jobs");
        assert_eq!(app.update(Msg::Key(KeyPress::Remove)), Effect::None);
        assert_eq!(app.config_pane().unwrap().dogs_tab().armed(), Some("jobs"));
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
        assert_eq!(app.config_pane().unwrap().dogs_tab().armed(), None);
    }

    #[test]
    fn any_other_key_disarms_and_the_first_esc_is_eaten() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "jobs");
        let _ = app.update(Msg::Key(KeyPress::Remove));
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(app.config_pane().unwrap().dogs_tab().armed(), None);
        assert_eq!(cursor_dog(&app), Some("legacy"));

        let _ = app.update(Msg::Key(KeyPress::Remove));
        assert_eq!(
            app.config_pane().unwrap().dogs_tab().armed(),
            Some("legacy")
        );
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert_eq!(app.config_pane().unwrap().dogs_tab().armed(), None);
        assert!(app.config_pane().is_some(), "the pane stays up");
    }

    #[test]
    fn a_row_with_no_table_arms_nothing() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "deploy");
        let _ = app.update(Msg::Key(KeyPress::Remove));
        assert_eq!(app.config_pane().unwrap().dogs_tab().armed(), None);
        assert!(app.notice().is_none(), "{:?}", app.notice());
    }

    #[test]
    fn a_closed_gate_refuses_the_removal_in_its_own_words() {
        let mut app = app_in_dogs(Control::ReadOnly);
        cursor_on(&mut app, "jobs");
        assert_eq!(app.update(Msg::Key(KeyPress::Remove)), Effect::None);
        assert_eq!(app.config_pane().unwrap().dogs_tab().armed(), None);
        assert_eq!(
            app.notice().map(ToString::to_string),
            Some(READ_ONLY_REFUSAL.to_string())
        );
    }

    #[test]
    fn a_landed_removal_notices_and_the_re_read_refreshes_the_tab() {
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
        assert_eq!(row(&app, "jobs").state(), DogTableState::Unset);
        assert_eq!(cursor_dog(&app), Some("jobs"));
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

    /// `Msg` derives `Debug`, and a listing or a probe answer carries each
    /// dog's schema, defaults included (IR-41).
    #[test]
    fn the_listing_and_probe_answers_debug_name_no_schema() {
        let entry = SheepDogEntry {
            name: "jobs".into(),
            adopted_path: None,
            schema: Some(json!({ "default": TOKEN })),
        };
        let listed = Msg::SheepDogNames {
            sheep: "web".into(),
            ask: 1,
            dogs: vec![ListedDog {
                entry: entry.clone(),
                asked: true,
            }],
        };
        assert!(!format!("{listed:?}").contains(TOKEN), "{listed:?}");
        let probed = Msg::SheepDog {
            sheep: "web".into(),
            entry,
        };
        assert!(!format!("{probed:?}").contains(TOKEN), "{probed:?}");
    }

    /// The sheep is the table's owner, so `r` re-reads the sheep and the
    /// open table takes its values from the answer, cursor and edits kept.
    #[test]
    fn r_re_reads_the_sheep_and_rebuilds_the_open_table_keeping_the_cursor() {
        let mut app = app_in_jobs_table();
        type_concurrency(&mut app, "4");
        pane_to(&mut app, "token");
        let cursor = app.config_pane().unwrap().view().cursor();
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
        assert!(row(&app, "jobs").is_open());
        assert_eq!(
            row(&app, "jobs")
                .table()
                .expect("built")
                .value("concurrency"),
            "9"
        );
        assert_eq!(app.config_pane().unwrap().view().cursor(), cursor);
        assert_eq!(table_edits(&app), 1, "the operator's edit survives");
    }

    /// A table another writer added above `jobs` shifts every row. The
    /// highlighted row and the row the table acts on must stay one row.
    #[test]
    fn a_re_read_that_shifts_the_rows_keeps_the_cursor_on_its_field() {
        let mut app = app_in_jobs_table();
        pane_to(&mut app, "token");
        let _ = app.update(Msg::Key(KeyPress::Refresh));
        let mut view = web_view(true);
        let alpha = json!({ "on": true });
        view.config.dogs.insert(
            "alpha".into(),
            alpha.as_object().cloned().expect("a table").into(),
        );
        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig { name: "web".into() },
            result: Ok(Response::SheepConfig(Box::new(view))),
        });
        assert_eq!(dog_names(&app)[0], "alpha", "the new table sorts first");
        let pane = app.config_pane().expect("a pane");
        let Some(PaneRow::Under { dog, at }) = pane.cursor() else {
            panic!("the cursor stays inside jobs's table: {:?}", pane.cursor());
        };
        let jobs = &pane.dogs_tab().rows()[dog];
        assert_eq!(jobs.name(), "jobs");
        let table = jobs.table().expect("built");
        assert_eq!(table.view().cursor(), at, "the table acts on the shown row");
        assert_eq!(
            table.cursor_field().map(|field| field.key.as_str()),
            Some("token")
        );
    }

    /// The probe answer lands after the cursor moved to another dog; the
    /// table it draws shifts the rows below it.
    #[test]
    fn a_probe_answer_after_the_cursor_moved_keeps_it_on_its_dog() {
        let mut app = app_in_dogs(Control::Allowed);
        cursor_on(&mut app, "jobs");
        assert!(matches!(
            app.update(Msg::Key(KeyPress::Confirm)),
            Effect::LoadSheepDog { .. }
        ));
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(cursor_dog(&app), Some("legacy"));
        let _ = app.update(Msg::SheepDog {
            sheep: "web".into(),
            entry: jobs_entry(),
        });
        assert!(row(&app, "jobs").is_open());
        assert_eq!(cursor_dog(&app), Some("legacy"));
    }

    #[test]
    fn a_re_read_of_another_sheep_leaves_the_open_table_alone() {
        let mut app = app_in_jobs_table();
        let mut view = web_view(true);
        view.name = "api".into();
        let other = json!({ "concurrency": 64 });
        view.config.dogs.insert(
            "jobs".into(),
            other.as_object().cloned().expect("a table").into(),
        );
        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig { name: "api".into() },
            result: Ok(Response::SheepConfig(Box::new(view))),
        });
        assert_eq!(
            row(&app, "jobs")
                .table()
                .expect("built")
                .value("concurrency"),
            "2"
        );
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
