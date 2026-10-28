use super::super::*;

impl App {
    /// One `Request::SetSheepField` reply. Reaches for no pane: the write
    /// went out on the keypress that closed it, so this lands with the
    /// dashboard on screen. A success still re-reads the config, using
    /// `Effect::Send` so the re-read gets `Msg::Unsent` handling for free.
    ///
    /// Every sentence names the field, since a close sends the whole set at
    /// once and several of these can land in a row.
    pub(in crate::lookout::app) fn on_field_applied(
        &mut self,
        name: &str,
        key: &str,
        value: &FieldValue,
        result: Result<Response, RequestError>,
    ) -> Effect {
        match result {
            Ok(Response::SheepFieldSet {
                pending, warning, ..
            }) => {
                let key_text = match value.safe_summary() {
                    Some(v) => format!("{key} set to {v}"),
                    None => format!("{key} is set"),
                };
                let mut text = if pending {
                    format!("{name}: {key_text}, and waits for `shep reload {name}`")
                } else {
                    format!("{name}: {key_text}")
                };
                if let Some(warning) = warning {
                    text = format!("{text}; {warning}");
                }
                self.notice = Some(Notice::info(text));
                Effect::Send(Sent::SheepConfig {
                    name: name.to_owned(),
                })
            }
            other => {
                self.notice = Some(Notice::grave(format!(
                    "{name}: {key}: {}",
                    failed_reply(other)
                )));
                Effect::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;

    /// A reply for a write from an earlier pane can land while a new one
    /// is open and being typed into. On the env screen the discarded
    /// buffer is a secret the operator cannot read back.
    #[test]
    fn a_reply_landing_mid_edit_leaves_the_buffer_and_the_keyboard_alone() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let mut batch = wire_batch(close_writing(&mut app));
        let sent = batch.remove(0);
        // The pane is reopened and the operator starts typing while the
        // first write is still out.
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for typed in "/srv".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        assert_eq!(app.mode(), InputMode::Text);
        let _ = app.update(Msg::Unsent { sent });
        assert_eq!(app.mode(), InputMode::Text, "the keyboard is not stranded");
        let typing = app
            .config_pane()
            .unwrap()
            .typing()
            .expect("the buffer survives the reply");
        assert_eq!(typing.key, "cwd");
        assert!(
            typing.buffer.as_str().ends_with("/srv"),
            "{:?}",
            typing.buffer
        );
    }

    /// A landed write asks for a re-read, and the re-read rebuilds the
    /// whole `ConfigPane`, editor included.
    #[test]
    fn a_refresh_that_drops_an_open_editor_puts_the_keyboard_back() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(app.mode(), InputMode::Text);
        refresh_config(&mut app, &["DB_HOST", "LOG_LEVEL"]);
        assert_eq!(app.mode(), InputMode::Normal);
        assert!(app.config_pane().unwrap().typing().is_none());
    }

    /// Validation runs on entry, so the set is always sendable. An integer
    /// field mid-word holds the editor open rather than filing a bad value,
    /// which is what `apply_typing` already does today.
    #[test]
    fn a_value_that_does_not_parse_never_joins_the_set() {
        let mut app = fixtures::app_in_sheep_pane();
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_field(&mut app, "max_restarts");
        app.update(Msg::Key(KeyPress::Confirm));
        for typed in "not a number".chars() {
            app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
        assert!(app.config_pane().unwrap().edits().is_empty());
        assert_eq!(app.mode(), InputMode::Text, "the editor stays open");
    }

    /// Nothing is armed, so nothing eats a keystroke: the next key does
    /// its own job and the filed edit stays filed.
    #[test]
    fn a_key_after_an_edit_does_its_own_job_and_keeps_the_edit() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let before = app.config_pane().unwrap().view().cursor();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
        assert_eq!(app.update(Msg::Key(KeyPress::SelectDown)), Effect::None);
        assert_eq!(
            app.config_pane().unwrap().view().cursor(),
            before + 1,
            "the movement key moves"
        );
        assert_eq!(
            app.config_pane().unwrap().edits().len(),
            1,
            "and does not cancel the edit"
        );
    }

    /// A filed edit is not a question waiting for an answer, so no timer
    /// takes it away: an operator who walked off must not come back to
    /// work silently discarded.
    #[test]
    fn a_filed_edit_outlives_the_confirm_budget() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let _ = app.update(Msg::Tick {
            now: Instant::now() + CONFIRM_EXPIRY,
        });
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
    }

    /// The pending set is the operator's and the values are the
    /// shepherd's: a re-read replaces one and keeps the other.
    #[test]
    fn a_refresh_replaces_the_values_and_keeps_the_edits() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        refresh_config(&mut app, &[]);
        assert_eq!(
            app.config_pane().unwrap().edits().len(),
            1,
            "the edit survives the rebuild"
        );
        let request = one_wire(close_writing(&mut app));
        let Request::SetSheepField { key, .. } = request else {
            panic!("{request:?}");
        };
        assert_eq!(key, "autorestart");
    }
}
