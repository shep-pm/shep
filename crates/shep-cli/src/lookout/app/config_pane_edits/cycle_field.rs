use super::super::*;

impl App {
    /// `space` on the config pane. Arms the next value for the row under
    /// the cursor, or refuses and says why.
    ///
    /// The gate is [`Self::authorize_write`], the same one every settings
    /// write passes and for the same reason: a keystroke that changes a
    /// running flock's config needs the fat-finger catch a keystroke that
    /// stops a sheep has.
    pub(in crate::lookout::app) fn cycle_field(&mut self) -> Effect {
        // The lock is checked ahead of the control gate, the same order
        // `confirm_field::confirm_field` takes: it is the more specific fact, and
        // `--allow-control` would not change it. A screen that answers
        // one question two ways teaches an operator to believe neither.
        if let Some((key, lock)) = self
            .config_pane()
            .and_then(ConfigPane::cursor_lock)
            .map(|(key, lock)| (key.to_owned(), lock))
        {
            self.notice = Some(Notice {
                text: Self::lock_refusal(&key, lock),
                grave: true,
            });
            return Effect::None;
        }
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        if let Some(pane) = self.config_pane_mut() {
            pane.cycle();
        }
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::*;
    use super::*;
    use crate::lookout::app::testing::*;

    /// The whole shape of the pane in one test: a keystroke files, and
    /// nothing reaches the shepherd for it.
    #[test]
    fn cycling_a_bool_files_an_edit_and_sends_nothing() {
        let mut app = fixtures::app_in_sheep_pane();
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_field(&mut app, "autorestart");
        let effect = app.update(Msg::Key(KeyPress::Cycle));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
    }

    #[test]
    fn undo_drops_the_edit_it_filed() {
        let mut app = fixtures::app_in_sheep_pane();
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_field(&mut app, "autorestart");
        app.update(Msg::Key(KeyPress::Cycle));
        app.update(Msg::Key(KeyPress::Undo));
        assert!(app.config_pane().unwrap().edits().is_empty());
    }

    #[test]
    fn cycling_a_bool_back_to_its_stored_value_files_nothing() {
        let mut app = fixtures::app_in_sheep_pane();
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_field(&mut app, "autorestart");
        app.update(Msg::Key(KeyPress::Cycle));
        app.update(Msg::Key(KeyPress::Cycle));
        assert!(
            app.config_pane().unwrap().edits().is_empty(),
            "a round trip back to the stored value is not an edit"
        );
    }

    /// `u` drops the newest edit and leaves the older one filed.
    #[test]
    fn u_undoes_the_newest_edit_only() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        pane_to(&mut app, "watch");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let _ = app.update(Msg::Key(KeyPress::Undo));
        let requests = wire_all(close_writing(&mut app));
        let named: Vec<String> = requests
            .iter()
            .map(|request| match request {
                Request::SetSheepField { key, .. } => key.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(named, vec!["autorestart".to_owned()]);
    }
}
