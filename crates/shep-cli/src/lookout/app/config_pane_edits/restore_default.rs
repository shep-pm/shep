use super::super::*;

impl App {
    /// `d` on the config pane's field list. Files an edit that removes the
    /// operator's value for the field under the cursor, so the stored
    /// default shows through once sent: the same verb the list sub-screen's
    /// `d` performs on an element, which is why both carry
    /// [`KeyPress::Remove`].
    ///
    /// Same lock-then-control order as [`Self::cycle_field`] and
    /// [`Self::confirm_field`]: the lock names the more specific reason and
    /// `--allow-control` would not change it. Does nothing on an env row,
    /// on `+ add a key`, or with no row at all: [`ConfigPane::cursor_kind`]
    /// is `None` for exactly those, and there is no field to restore.
    pub(in crate::lookout::app) fn restore_default(&mut self) -> Effect {
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
        if self
            .config_pane()
            .and_then(ConfigPane::cursor_kind)
            .is_none()
        {
            return Effect::None;
        }
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        if let Some(pane) = self.config_pane_mut() {
            pane.file_default();
        }
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::*;
    use super::*;
    use crate::lookout::app::testing::*;

    /// `d` on an overridden field files the schema's own `default` rather
    /// than [`Value::Null`]: `max_restarts` is a plain `u32`, not an
    /// `Option<u32>`, and the shepherd's deserializer refuses `null` for
    /// one of those. The Flockfile schema's default for `max_restarts` is
    /// `16`.
    #[test]
    fn d_restores_an_overridden_field_to_its_schema_default() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        assert!(
            app.config_pane().unwrap().is_overridden("max_restarts"),
            "the fixture overrides max_restarts"
        );
        pane_to(&mut app, "max_restarts");
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
        assert_eq!(filed_value(&app, "max_restarts"), serde_json::json!(16));

        let request = one_wire(close_writing(&mut app));
        let Request::SetSheepField { key, value, .. } = request else {
            panic!("expected SetSheepField, got {request:?}");
        };
        assert_eq!(key, "max_restarts");
        assert_eq!(value, serde_json::json!(16));
    }

    /// A field the operator has not overridden is already showing its
    /// default, so `d` files nothing: an edit that changes nothing would
    /// still be counted by the title band.
    #[test]
    fn d_on_a_field_already_at_its_default_files_nothing() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        assert!(
            !app.config_pane().unwrap().is_overridden("autorestart"),
            "the fixture does not override autorestart"
        );
        pane_to(&mut app, "autorestart");
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(app.config_pane().unwrap().edits().is_empty());
    }

    /// A field flipped this session, even one the shepherd never
    /// overrode, is no longer showing its default: `d` restores the
    /// schema default rather than leaving the flipped value in place. For
    /// a field the shepherd never overrode, the schema default and what
    /// the shepherd already holds are the same value, so restoring it
    /// exactly cancels the flip: the fresh edit drops rather than being
    /// replaced by a second one.
    #[test]
    fn d_after_cycling_a_fresh_value_restores_the_default_anyway() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        assert!(
            !app.config_pane().unwrap().is_overridden("autostart"),
            "the fixture does not override autostart"
        );
        pane_to(&mut app, "autostart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
        assert_ne!(
            filed_value(&app, "autostart"),
            serde_json::Value::Null,
            "the flip files the opposite of the stored value"
        );

        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(
            app.config_pane().unwrap().edits().is_empty(),
            "the schema default for autostart is the fixture's own stored value, \
                 so restoring it cancels the flip rather than filing a second edit"
        );
    }

    /// A field that is already `(unset)` on the shepherd's own side has
    /// nowhere further to fall: cancelling a fresh, unsent edit to it
    /// files nothing rather than a `Null` edit that would just repeat what
    /// the shepherd already has.
    #[test]
    fn d_after_typing_an_unset_field_leaves_nothing_filed() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        fixtures::type_into_the_open_editor(&mut app, "/srv/api");
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);

        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(
            app.config_pane().unwrap().edits().is_empty(),
            "cwd was already (unset), so cancelling the typed edit leaves nothing to send"
        );
    }

    /// The lock wins over the control gate here too: `d` refuses a
    /// Structural field with the same sentence `space` and `Enter` give it.
    #[test]
    fn d_refuses_a_locked_field_with_its_lock_sentence() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "instances");
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        let notice = app.notice().expect("a locked row answers").to_string();
        assert!(notice.contains("`shep stock`"), "{notice}");
        assert!(app.config_pane().unwrap().edits().is_empty());
    }

    /// A read-only pane refuses `d` the same way it refuses `space` and
    /// `Enter`: on the keystroke that would file the edit, not on a later
    /// close that would try to send it.
    #[test]
    fn d_refuses_when_the_pane_is_read_only() {
        let mut app = fixtures::app_in_sheep_pane();
        pane_to(&mut app, "autorestart");
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(app.config_pane().unwrap().edits().is_empty());
        assert_eq!(
            app.notice().map(ToString::to_string),
            Some(READ_ONLY_REFUSAL.to_string())
        );
    }

    /// `d` means nothing on an env row: unsetting a key entirely is a
    /// different act from restoring a default, and the spec does not ask
    /// for it.
    #[test]
    fn d_on_an_env_row_files_nothing() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::AddEnv));
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(app.config_pane().unwrap().edits().is_empty());
    }
}
