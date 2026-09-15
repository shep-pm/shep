use super::super::*;

impl App {
    /// `space` on the settings screen: arms a candidate for the cursor's row,
    /// or refuses through [`Self::authorize_write`].
    pub(super) fn cycle_setting(&mut self) -> Effect {
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        let Some(cursor) = self.settings().and_then(Settings::cursor) else {
            return Effect::None;
        };
        match cursor {
            SettingsRow::Scalar(field) => self.cycle_scalar(field),
            SettingsRow::Dog(index) => self.cycle_dog(index),
        }
    }

    /// `space` on one of the six scalar rows. Re-arms when a candidate is
    /// already armed, so a second `space` walks one step further along the
    /// cycle. Does nothing on the two free-text fields.
    ///
    /// Replaces a [`Pending::Sent`] outright rather than refusing over it:
    /// the write it names is local file I/O the operator need not wait on,
    /// and its answer can no longer reach the edit armed here. See
    /// [`Settings::pending`].
    fn cycle_scalar(&mut self, field: SettingField) -> Effect {
        // See the comment in `Self::take_pane_writes`: a direct field
        // match, not `Self::settings_mut`, so `self.now` stays reachable
        // below.
        let Some(settings) = (match &mut self.body {
            Body::Settings(settings) => Some(settings),
            Body::FlockTable
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }) else {
            return Effect::None;
        };
        let Some(value) = settings.next_candidate(field) else {
            return Effect::None;
        };
        let source = settings.source_of(field);
        let text = confirm_text(field, &value, source);
        settings.pending = Some(Pending::Armed {
            edit: SettingEdit::Set { field, value },
            text,
            at: self.now,
        });
        Effect::None
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use crate::lookout::view::fixtures;

    /// Six log levels and one cycle key: without re-arming, the fourth needs a
    /// cancel in between.
    #[test]
    fn space_advances_the_candidate_rather_than_needing_a_cancel() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let first = app.settings().unwrap().pending().unwrap().text.to_string();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let second = app.settings().unwrap().pending().unwrap().text.to_string();
        assert_ne!(first, second);
    }

    /// With `$SHEP_STYLE=bare` over a file saying `full`, cycling the resolved
    /// value would propose `full`: a no-op write, reported as a change.
    #[test]
    fn the_style_cycle_starts_from_the_file_and_not_the_level_in_force() {
        let mut app = fixtures::app_in_settings_with_shadowed_style(StyleSource::Env);
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(
            text.contains("set style level to plain"),
            "the file says full, so one step is plain: {text}"
        );
    }
}
