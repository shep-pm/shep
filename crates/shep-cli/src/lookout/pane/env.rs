//! The env rows: one per environment key, plus a row for adding one.
//!
//! Values never reach the screen and never reach `Debug`. A key is listed, an
//! edit is typed into the shared [`KeyValues`] editor this pane owns, and what
//! leaves is a `PaneEdit::SetEnv` carrying an [`EnvValue`], which prints as a
//! byte count.

use std::time::Instant;

use shep_core::protocol::EnvValue;

use super::super::app::KeyPress;
use super::super::keyvalue::{Grammar, KeyValues, Outcome};
use super::{ConfigPane, PaneEdit, PaneRow};

/// A delete armed on an env row: the key, and when it armed, one value rather
/// than two so a caller cannot set one without the other, the same pairing
/// [`ArmedDelete`](super::super::app::ArmedDelete) keeps on the secrets pane.
///
/// `Debug` is derived (IR-41): a key name and an instant, never a value.
#[derive(Debug, Clone)]
pub(crate) struct ArmedEnv {
    /// The key waiting on `Enter` to confirm the delete.
    pub(crate) key: String,
    /// When it armed, for the expiry the tick runs.
    pub(crate) at: Instant,
}

/// What `Enter` on the env editor did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnvSubmit {
    /// The editor stayed open with what was typed still in it. Carries the
    /// sentence to raise.
    Refused(String),
    /// The cursor moved from KEY to VALUE.
    Moved,
    /// The editor closed and its write was filed.
    Filed,
}

impl ConfigPane {
    /// The sheep's own env key names. Empty for a dog, which reads its own
    /// section rather than this list (see [`Self::dog`]).
    ///
    /// What [`Self::rows`]'s trailing [`PaneRow::Env`] rows index into.
    #[must_use]
    pub fn env_key_names(&self) -> &[String] {
        &self.env_keys
    }

    /// The open env editor, or [`None`].
    #[must_use]
    pub fn env_typing(&self) -> Option<&KeyValues> {
        self.env_typing.as_ref()
    }

    /// The armed env delete, or [`None`]. While this is set, `Enter` confirms
    /// the delete rather than opening the editor, exactly as
    /// [`SecretsPane::armed`](super::super::app::SecretsPane) does there.
    #[must_use]
    pub fn armed_env(&self) -> Option<&ArmedEnv> {
        self.armed_env.as_ref()
    }

    /// Opens the env editor on the row under the cursor. Does nothing unless
    /// the cursor is on [`PaneRow::Env`] or [`PaneRow::AddEnv`].
    ///
    /// The add row opens on KEY with both fields empty; an existing row opens
    /// on VALUE, whose field is empty because `Request::SheepConfig` answers
    /// with the env key names alone, so there is no value to seed it with and
    /// seeding one would mean this pane had been told a secret it never holds.
    pub(in crate::lookout) fn begin_env_typing(&mut self) {
        self.env_typing = match self.cursor() {
            Some(PaneRow::Env(index)) => self
                .env_keys
                .get(index)
                .map(|key| KeyValues::under(Grammar::Env, key)),
            Some(PaneRow::AddEnv) => Some(KeyValues::add(Grammar::Env)),
            Some(
                PaneRow::Field(_)
                | PaneRow::Table(_)
                | PaneRow::Entry(_)
                | PaneRow::Choice { .. }
                | PaneRow::Arm { .. }
                | PaneRow::Dog(_)
                | PaneRow::Under { .. },
            )
            | None => None,
        };
    }

    /// A typed character, a backspace or a cursor move, into the field of the
    /// open editor that holds the cursor.
    pub(in crate::lookout) fn type_env_text(&mut self, key: KeyPress) {
        if let Some(typing) = self.env_typing.as_mut() {
            typing.handle(key);
        }
    }

    /// `Tab` or `Shift-Tab` on the env editor.
    ///
    /// Returns the sentence a KEY the grammar refuses raises, with the cursor
    /// left on KEY, or [`None`] when the cursor moved.
    pub(in crate::lookout) fn step_env_field(&mut self, back: bool) -> Option<String> {
        let typing = self.env_typing.as_mut()?;
        match typing.step(back) {
            Outcome::Refused(refusal) => Some(refusal.sentence(typing.key().as_str())),
            Outcome::Moved | Outcome::Submitted => None,
        }
    }

    /// `Enter`: submit from VALUE, or check KEY and move off it.
    ///
    /// A submitted editor files and closes. A refused one keeps what was typed
    /// and keeps the cursor where the operator can fix it, the same shape the
    /// field editor's own refusal has.
    pub(in crate::lookout) fn submit_env_typing(&mut self) -> EnvSubmit {
        if self.env_typing.is_none() {
            return EnvSubmit::Filed;
        }
        let outcome = self.env_typing.as_mut().expect("checked above").submit();
        match outcome {
            Outcome::Refused(refusal) => {
                let key = self
                    .env_typing
                    .as_ref()
                    .expect("still open")
                    .key()
                    .as_str()
                    .to_owned();
                EnvSubmit::Refused(refusal.sentence(&key))
            }
            Outcome::Moved => EnvSubmit::Moved,
            Outcome::Submitted => {
                let typing = self.env_typing.take().expect("an open editor");
                self.file_env_typing(&typing);
                EnvSubmit::Filed
            }
        }
    }

    /// Drops an editor under construction, leaving the pane open.
    pub(in crate::lookout) fn abandon_env_typing(&mut self) {
        self.env_typing = None;
    }

    /// Arms the removal of the env key under the cursor. Does nothing on
    /// [`PaneRow::AddEnv`], which names no key to remove.
    pub(in crate::lookout) fn arm_env_delete(&mut self, at: Instant) {
        if let Some(key) = self.cursor_env_key_name() {
            self.armed_env = Some(ArmedEnv {
                key: key.to_owned(),
                at,
            });
        }
    }

    /// Confirms the armed delete: files the key's removal, and disarms.
    /// Whether one was there at all is the caller's `Escape` cue, the same
    /// answer [`disarm_env_delete`](Self::disarm_env_delete) gives.
    pub(in crate::lookout) fn confirm_env_delete(&mut self) -> bool {
        let Some(ArmedEnv { key, .. }) = self.armed_env.take() else {
            return false;
        };
        self.file_env(key, None);
        true
    }

    /// Clears an armed delete, and says whether one was there.
    pub(in crate::lookout) fn disarm_env_delete(&mut self) -> bool {
        self.armed_env.take().is_some()
    }

    /// Files an env write from the editor, which it closes over.
    ///
    /// An existing key with an empty VALUE removes it: there is no separate
    /// unset key for env, and no widget for one either, so an empty value and
    /// no value are the same keystroke here. A rename files both halves from
    /// the one `Enter`, the new key set and the old one removed, so an operator
    /// moves a key rather than deleting it and adding it again. The editor
    /// refuses a rename that carried no value, so the set half is never empty
    /// here.
    fn file_env_typing(&mut self, typing: &KeyValues) {
        let key = typing.key().as_str().to_owned();
        let typed = typing.value().as_str();
        let typed = typed.to_owned();
        match typing.opened_under() {
            None => self.file_env(key, Some(EnvValue::from(typed))),
            Some(under) if under == key => {
                let value = (!typed.is_empty()).then(|| EnvValue::from(typed));
                self.file_env(key, value);
            }
            Some(under) => {
                let under = under.to_owned();
                self.file_env(under, None);
                self.file_env(key, Some(EnvValue::from(typed)));
            }
        }
    }

    /// Files an env write.
    ///
    /// Env files like everything else, and unlike everything else it is never
    /// compared against a stored value: `Request::SheepConfig` answers with the
    /// key names alone, so the pane has nothing to compare against and cannot
    /// tell a round trip from a change.
    pub(in crate::lookout) fn file_env(&mut self, key: String, value: Option<EnvValue>) {
        let impact = self.cost("env");
        self.edits.set(PaneEdit::SetEnv { key, value }, impact);
    }

    /// The env key the cursor sits on, when it is on an env row:
    /// `Some(Some(key))` on [`PaneRow::Env`], `Some(None)` on
    /// [`PaneRow::AddEnv`], [`None`] when the cursor is on a field.
    ///
    /// What a refresh carries instead of the cursor's own index: adding or
    /// removing an env key shifts every row after it, and an index that
    /// survived would name a different key. See [`Self::adopt_env_cursor`].
    #[must_use]
    pub(in crate::lookout) fn cursor_env_key(&self) -> Option<Option<String>> {
        match self.cursor()? {
            PaneRow::Env(index) => Some(self.env_keys.get(index).cloned()),
            PaneRow::AddEnv => Some(None),
            PaneRow::Field(_)
            | PaneRow::Table(_)
            | PaneRow::Entry(_)
            | PaneRow::Choice { .. }
            | PaneRow::Arm { .. }
            | PaneRow::Dog(_)
            | PaneRow::Under { .. } => None,
        }
    }

    /// Puts the cursor back on `key`'s own row after a refresh, or on
    /// `+ add a key` when `key` is [`None`] or is no longer among
    /// [`Self::env_key_names`].
    ///
    /// Called only when [`Self::cursor_env_key`] read on the previous pane
    /// reported the cursor was on an env row; every other case is a plain
    /// [`Self::adopt_view`], index-clamped.
    pub(in crate::lookout) fn adopt_env_cursor(&mut self, key: Option<&str>) {
        let rows = self.rows();
        let index = key
            .and_then(|key| {
                rows.iter().position(|row| match row {
                    PaneRow::Env(env_index) => {
                        self.env_keys.get(*env_index).map(String::as_str) == Some(key)
                    }
                    PaneRow::Field(_)
                    | PaneRow::Table(_)
                    | PaneRow::Entry(_)
                    | PaneRow::Choice { .. }
                    | PaneRow::Arm { .. }
                    | PaneRow::Dog(_)
                    | PaneRow::Under { .. }
                    | PaneRow::AddEnv => false,
                })
            })
            .unwrap_or_else(|| rows.len().saturating_sub(1));
        let len = rows.len();
        self.view.move_to(index, len);
    }

    /// The key name of the env row the cursor is on, or [`None`] when the
    /// cursor is on `+ add a key` or on a field. What
    /// `the_env_cursor_is_carried_by_key_and_not_by_index_across_a_refresh`
    /// reads directly, the way an assertion on `EnvPane::cursor_key` used to
    /// before the sub-screen it belonged to folded into this list.
    #[must_use]
    pub(in crate::lookout) fn cursor_env_key_name(&self) -> Option<&str> {
        match self.cursor()? {
            PaneRow::Env(index) => self.env_keys.get(index).map(String::as_str),
            PaneRow::Field(_)
            | PaneRow::Table(_)
            | PaneRow::Entry(_)
            | PaneRow::Choice { .. }
            | PaneRow::Arm { .. }
            | PaneRow::Dog(_)
            | PaneRow::Under { .. }
            | PaneRow::AddEnv => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use shep_core::config::ApplyGroup;

    use super::super::fixtures::web;
    use super::*;
    use crate::lookout::app::KeyPress;
    use crate::lookout::edits::EditKey;
    use crate::lookout::keyvalue::Field;

    /// Types `text` into whichever field the open editor has the cursor on,
    /// the way `map_key` hands a box its characters.
    fn type_into(pane: &mut ConfigPane, text: &str) {
        for typed in text.chars() {
            pane.type_env_text(KeyPress::TextChar(typed));
        }
    }

    /// Env is `NeedsRespawn` in every case, and the set records that rather
    /// than the key's own name, which is not a config field.
    #[test]
    fn a_filed_env_edit_carries_envs_own_apply_group() {
        for value in [Some("hunter2".to_owned().into()), None] {
            let mut pane = ConfigPane::sheep(web());
            pane.file_env("DB_PASSWORD".into(), value);
            let entry = pane
                .edits()
                .get(&EditKey::Env("DB_PASSWORD".into()))
                .expect("filed under its env key");
            assert_eq!(entry.impact(), Some(ApplyGroup::NeedsRespawn));
            assert!(
                pane.edits()
                    .get(&EditKey::Field("DB_PASSWORD".into()))
                    .is_none(),
                "an env key is not a config field"
            );
        }
    }

    /// `web()` carries one env key, `DB_HOST`, so its rows are one
    /// [`PaneRow::Env`] and the trailing [`PaneRow::AddEnv`].
    #[test]
    fn the_env_rows_list_keys_and_add_a_key_and_an_empty_value_means_unset() {
        let mut pane = ConfigPane::sheep(web());
        let env_row_count = pane
            .rows()
            .into_iter()
            .filter(|row| matches!(row, PaneRow::Env(_) | PaneRow::AddEnv))
            .count();
        assert_eq!(env_row_count, 2, "one key and a + add a key row");

        pane.move_to_last();
        assert_eq!(pane.cursor(), Some(PaneRow::AddEnv));
        pane.begin_env_typing();
        type_into(&mut pane, "NEW_KEY");
        assert_eq!(pane.step_env_field(false), None, "KEY is a valid name");
        type_into(&mut pane, "value");
        assert_eq!(pane.submit_env_typing(), EnvSubmit::Filed);
        let entry = pane
            .edits()
            .get(&EditKey::Env("NEW_KEY".to_owned()))
            .expect("NEW_KEY was filed");
        assert!(matches!(entry.edit(), PaneEdit::SetEnv { key, .. } if key == "NEW_KEY"));

        pane.move_by(-1);
        assert_eq!(pane.cursor(), Some(PaneRow::Env(0)));
        pane.begin_env_typing();
        assert_eq!(pane.submit_env_typing(), EnvSubmit::Filed);
        let removal = pane
            .edits()
            .get(&EditKey::Env("DB_HOST".to_owned()))
            .expect("DB_HOST was filed");
        assert!(matches!(
            removal.edit(),
            PaneEdit::SetEnv { key, value: None } if key == "DB_HOST"
        ));
    }

    /// A rename is one `Enter`: the new key is set and the old one removed,
    /// both filed, neither sent until the pane closes.
    #[test]
    fn renaming_an_env_key_files_the_new_key_and_the_removal_of_the_old_one() {
        let mut pane = ConfigPane::sheep(web());
        assert_eq!(pane.env_key_names(), ["DB_HOST"]);
        // `move_to_last` lands on `+ add a key`; one step up is `DB_HOST`, the
        // fixture's only env key.
        pane.move_to_last();
        pane.move_by(-1);
        assert_eq!(pane.cursor(), Some(PaneRow::Env(0)));
        pane.begin_env_typing();
        pane.step_env_field(true);
        pane.type_env_text(KeyPress::TextEdit(crate::lookout::textbox::TextEdit::DeleteToStart));
        type_into(&mut pane, "DB_URL");
        pane.step_env_field(false);
        type_into(&mut pane, "db.internal");

        assert_eq!(pane.submit_env_typing(), EnvSubmit::Filed);
        assert!(
            matches!(
                pane.edits().get(&EditKey::Env("DB_URL".into())).expect("new key").edit(),
                PaneEdit::SetEnv { key, value: Some(_) } if key == "DB_URL"
            ),
            "the new key carries what was typed"
        );
        assert!(
            matches!(
                pane.edits().get(&EditKey::Env("DB_HOST".into())).expect("old key").edit(),
                PaneEdit::SetEnv { key, value: None } if key == "DB_HOST"
            ),
            "and the key it was renamed out of is removed"
        );
    }

    /// `ConfigPane::dog` always leaves `env_keys` empty, so an `env` field on
    /// a dog reads its own section, not the sheep key count. No built-in
    /// declares such a field; the dog pane exists for schemas shep did not
    /// write.
    #[test]
    fn a_dogs_own_env_field_reads_its_section_and_not_the_sheep_key_count() {
        let schema = serde_json::json!({
            "properties": {
                "env": { "type": "string" },
            }
        });
        let mut pane = ConfigPane::dog(
            "pydog".into(),
            None,
            schema,
            "env = \"staging\"\n".to_owned(),
        );
        assert_eq!(pane.value("env"), "staging");
        pane.move_to_key("env");
        pane.begin_typing();
        let typing = pane.typing().expect("the editor is open");
        assert_eq!(typing.buffer.as_str(), "staging");
    }

    /// The editor on an existing row is the shared one, and its `Debug` names
    /// neither the key nor what was typed (IR-41).
    #[test]
    fn debug_names_no_key_and_no_value_on_an_env_editor() {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_last();
        assert_eq!(pane.cursor(), Some(PaneRow::AddEnv));
        pane.begin_env_typing();
        type_into(&mut pane, "STRIPE_KEY");
        pane.step_env_field(false);
        type_into(&mut pane, "sk_live_1");
        let typing = pane.env_typing().expect("the editor is open");
        assert_eq!(
            format!("{typing:?}"),
            "KeyValues { add: true, key: <10 chars>, value: <9 chars>, focus: Value }"
        );
    }

    /// A KEY the env grammar refuses is refused when the field loses focus,
    /// with the cursor kept on it and what was typed kept too.
    #[test]
    fn an_invalid_env_key_is_refused_as_the_field_loses_focus() {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_last();
        pane.begin_env_typing();
        type_into(&mut pane, "9BAD");

        let refused = pane.step_env_field(false).expect("a refusal says so");
        assert!(refused.contains("a letter or `_`"), "{refused}");
        let typing = pane.env_typing().expect("the editor stayed open");
        assert_eq!(typing.focus(), Field::Key);
        assert_eq!(typing.key().as_str(), "9BAD", "and nothing was thrown away");
    }

    /// `D` arms, `Enter` files the removal, and any other key clears the arm.
    #[test]
    fn an_armed_env_delete_files_a_removal_on_enter_and_clears_on_anything_else() {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_last();
        pane.move_by(-1);
        assert_eq!(pane.cursor(), Some(PaneRow::Env(0)));
        pane.arm_env_delete(Instant::now());
        assert_eq!(pane.armed_env().map(|armed| armed.key.as_str()), Some("DB_HOST"));
        assert!(pane.confirm_env_delete(), "the arm was there to confirm");
        let removal = pane
            .edits()
            .get(&EditKey::Env("DB_HOST".into()))
            .expect("DB_HOST was filed");
        assert!(matches!(
            removal.edit(),
            PaneEdit::SetEnv { key, value: None } if key == "DB_HOST"
        ));

        pane.arm_env_delete(Instant::now());
        assert!(pane.disarm_env_delete(), "and a second arm clears the same way");
        assert!(pane.armed_env().is_none());
    }

    /// `+ add a key` names no key, so arming there arms nothing.
    #[test]
    fn arming_a_delete_on_the_add_row_arms_nothing() {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_last();
        assert_eq!(pane.cursor(), Some(PaneRow::AddEnv));
        pane.arm_env_delete(Instant::now());
        assert!(pane.armed_env().is_none());
    }
}
