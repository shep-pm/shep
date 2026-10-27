use super::super::*;
use crate::commands::settings::Shape;
use crate::lookout::field::shep_toml_shape;

impl App {
    /// `space` on the settings screen: folds a section, or arms a candidate
    /// for the cursor's row, or refuses through [`Self::authorize_write`].
    pub(super) fn cycle_setting(&mut self) -> Effect {
        if self
            .settings_mut()
            .is_some_and(|s| s.pending.is_none() && s.toggle_fold())
        {
            return Effect::None;
        }
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        let Some(cursor) = self.settings().and_then(Settings::cursor) else {
            return Effect::None;
        };
        match cursor {
            SettingsRow::Field(index) => self.cycle_key(index),
            SettingsRow::Dog(index) => self.cycle_dog(index),
            SettingsRow::Table(_) | SettingsRow::Entry { .. } | SettingsRow::AddEntry(_) => {
                Effect::None
            }
        }
    }

    /// `space` on a key row. Re-arms when a candidate is already armed, so a
    /// second `space` walks one step further along the cycle. Does nothing
    /// on a key that is typed.
    ///
    /// Replaces a [`Pending::Sent`] outright rather than refusing over it:
    /// the write it names is local file I/O the operator need not wait on,
    /// and its answer can no longer reach the edit armed here. See
    /// [`Settings::pending`].
    fn cycle_key(&mut self, index: usize) -> Effect {
        let shape = shep_toml_shape();
        let field = &shape.fields.fields()[index];
        let Some(settings) = self.body.settings_mut() else {
            return Effect::None;
        };
        let (Some(value), Some(path), Some(kind)) = (
            settings.next_candidate(field),
            shape.paths.get(&field.key),
            Shape::of(&field.kind),
        ) else {
            return Effect::None;
        };
        let edit = SettingEdit::Set {
            path: path.clone(),
            shape: kind,
            value,
        };
        let text = confirm_text(&edit, settings.snapshot.style.1);
        settings.pending = Some(Pending::Armed {
            edit,
            text,
            at: self.now,
        });
        Effect::None
    }

    /// `d` on the settings screen: arms removing the cursor's key or map
    /// entry from `shep.toml`. An armed candidate eats the key first, like
    /// every other key here.
    pub(super) fn remove_setting(&mut self) -> Effect {
        if self.disarm_settings_candidate() || self.authorize_write().is_none() {
            return Effect::None;
        }
        let now = self.now;
        let Some(settings) = self.body.settings_mut() else {
            return Effect::None;
        };
        if let Some(edit) = settings.removal_at_cursor() {
            let text = confirm_text(&edit, settings.snapshot.style.1);
            settings.pending = Some(Pending::Armed {
                edit,
                text,
                at: now,
            });
        }
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
        let mut app = fixtures::app_in_settings_on("daemon.log_level");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let first = app.settings().unwrap().pending().unwrap().text.to_string();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let second = app.settings().unwrap().pending().unwrap().text.to_string();
        assert_ne!(first, second);
    }

    /// `[style] level` has no schema default, and `full` is what is in force
    /// when the file is silent, so the first step is `plain`.
    #[test]
    fn the_style_cycle_from_a_silent_file_does_not_propose_the_level_in_force() {
        let mut app = fixtures::app_in_settings_with_silent_style();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(
            text.contains("set style level to plain"),
            "the file says nothing, so full is in force and one step is plain: {text}"
        );
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
