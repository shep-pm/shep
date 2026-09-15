//! The env sub-screen: one row per environment key, plus a row for adding
//! one.
//!
//! Values never reach the screen and never reach `Debug`. A key is listed,
//! an edit is typed into a buffer this module owns, and what leaves is a
//! `PaneEdit::SetEnv` carrying an [`EnvValue`], which prints as a byte
//! count.

use shep_core::protocol::EnvValue;

use super::{ConfigPane, PaneEdit, PaneRow};

/// The pane's open env editor: which key, and what has been typed.
///
/// `key` is [`None`] on the `+ add a key` row, where the buffer is the
/// whole `KEY=value` rather than a value alone: an env edit's key does not
/// exist yet on that row, and forcing it through [`PaneTyping`]'s bare
/// `key: String` would need an empty string as a sentinel, which is itself
/// a legal env key name.
///
/// `Debug` is manual and redacted (IR-41), exact-string-tested below. The
/// buffer is the secret itself, the whole of `DB_PASSWORD=hunter2` in one
/// string on the `+ add a key` row.
#[derive(Clone, PartialEq, Eq)]
pub struct EnvTyping {
    key: Option<String>,
    buffer: String,
}

/// Prints whether a key is under edit and never the key or the buffer.
/// See the type doc for why. Exact-string-tested below
/// (`debug_names_no_key_and_no_value_on_an_env_typing`).
impl core::fmt::Debug for EnvTyping {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "EnvTyping {{ key: {}, buffer: <{} chars> }}",
            self.key.is_some(),
            self.buffer.chars().count()
        )
    }
}

impl EnvTyping {
    /// Which key is under edit, or [`None`] on the `+ add a key` row,
    /// where [`Self::buffer`] is the whole `KEY=value` rather than a
    /// value alone.
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// What has been typed so far.
    #[must_use]
    pub fn buffer(&self) -> &str {
        &self.buffer
    }
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
    pub fn env_typing(&self) -> Option<&EnvTyping> {
        self.env_typing.as_ref()
    }

    /// Opens the env editor on the row under the cursor. Does nothing
    /// unless the cursor is on [`PaneRow::Env`] or [`PaneRow::AddEnv`].
    ///
    /// Seeded empty always, on an existing key too: `Request::SheepConfig`
    /// answers with the env key names alone, so there is no value to seed
    /// an editor with, and seeding one would mean this pane had been told
    /// a secret it never holds.
    pub(in crate::lookout) fn begin_env_typing(&mut self) {
        self.env_typing = match self.cursor() {
            Some(PaneRow::Env(index)) => Some(EnvTyping {
                key: self.env_keys.get(index).cloned(),
                buffer: String::new(),
            }),
            Some(PaneRow::AddEnv) => Some(EnvTyping {
                key: None,
                buffer: String::new(),
            }),
            Some(PaneRow::Field(_)) | None => None,
        };
    }

    /// Appends one typed character.
    pub fn type_env_char(&mut self, typed: char) {
        if let Some(typing) = self.env_typing.as_mut() {
            typing.buffer.push(typed);
        }
    }

    /// Removes the last typed character.
    pub fn type_env_backspace(&mut self) {
        if let Some(typing) = self.env_typing.as_mut() {
            typing.buffer.pop();
        }
    }

    /// Drops an editor under construction, leaving the pane open.
    pub fn abandon_env_typing(&mut self) {
        self.env_typing = None;
    }

    /// Closes the editor, reads what it holds, and files it.
    ///
    /// An existing key with an empty buffer removes it: there is no
    /// separate unset key for env, and no widget for one either, so an
    /// empty value and no value are the same keystroke here. On
    /// `+ add a key` a buffer with no `=` or an empty key names nothing
    /// and nothing is filed, since guessing a key would be inventing the
    /// operator's intent.
    pub fn apply_env_typing(&mut self) {
        let Some(EnvTyping { key, buffer }) = self.env_typing.take() else {
            return;
        };
        let (key, value) = match key {
            Some(key) => (key, (!buffer.is_empty()).then_some(buffer)),
            None => {
                let Some((key, value)) = buffer.split_once('=') else {
                    return;
                };
                if key.is_empty() {
                    return;
                }
                (key.to_owned(), Some(value.to_owned()))
            }
        };
        self.file_env(key, value.map(EnvValue::from));
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
            PaneRow::Field(_) => None,
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
                    PaneRow::Field(_) | PaneRow::AddEnv => false,
                })
            })
            .unwrap_or_else(|| rows.len().saturating_sub(1));
        let len = rows.len();
        self.view.move_to(index, len);
    }

    /// The key name of the env row the cursor is on, or [`None`] when the
    /// cursor is on `+ add a key` or on a field. What
    /// `the_env_cursor_is_carried_by_key_and_not_by_index_across_a_refresh`
    /// reads directly, the way an assertion on `EnvPane::cursor_key` used
    /// to before the sub-screen it belonged to folded into this list.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn cursor_env_key_name(&self) -> Option<&str> {
        match self.cursor()? {
            PaneRow::Env(index) => self.env_keys.get(index).map(String::as_str),
            PaneRow::Field(_) | PaneRow::AddEnv => None,
        }
    }

    /// Files an env write from the sub-screen's own editor.
    ///
    /// Env files like everything else, and unlike everything else it is
    /// never compared against a stored value: `Request::SheepConfig`
    /// answers with the key names alone, so the pane has nothing to
    /// compare against and cannot tell a round trip from a change.
    pub(in crate::lookout) fn file_env(&mut self, key: String, value: Option<EnvValue>) {
        let impact = self.cost("env");
        self.edits.set(PaneEdit::SetEnv { key, value }, impact);
    }
}

#[cfg(test)]
mod tests {
    use shep_core::config::ApplyGroup;

    use super::super::super::edits::EditKey;
    use super::super::fixtures::web;
    use super::*;

    /// Env is `NeedsRespawn` in every case, and the set records that
    /// rather than the key's own name, which is not a config field.
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
    fn the_env_rows_list_keys_and_add_a_key_and_an_empty_apply_means_unset() {
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
        for c in "NEW_KEY=value".chars() {
            pane.type_env_char(c);
        }
        pane.apply_env_typing();
        let entry = pane
            .edits()
            .get(&EditKey::Env("NEW_KEY".to_owned()))
            .expect("NEW_KEY was filed");
        assert!(matches!(entry.edit(), PaneEdit::SetEnv { key, .. } if key == "NEW_KEY"));

        pane.move_by(-1);
        assert_eq!(pane.cursor(), Some(PaneRow::Env(0)));
        pane.begin_env_typing();
        pane.apply_env_typing();
        let removal = pane
            .edits()
            .get(&EditKey::Env("DB_HOST".to_owned()))
            .expect("DB_HOST was filed");
        assert!(matches!(
            removal.edit(),
            PaneEdit::SetEnv { key, value: None } if key == "DB_HOST"
        ));
    }

    /// `ConfigPane::dog` always leaves `env_keys` empty, so an `env` field
    /// on a dog reads its own section, not the sheep key count. No
    /// built-in declares such a field; the dog pane exists for schemas
    /// shep did not write.
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
        assert_eq!(typing.buffer, "staging");
    }

    /// The buffer on `+ add a key` is the whole `KEY=value`, secret
    /// included (IR-41).
    #[test]
    fn debug_names_no_key_and_no_value_on_an_env_typing() {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_last();
        assert_eq!(pane.cursor(), Some(PaneRow::AddEnv));
        pane.begin_env_typing();
        let typed = "STRIPE_KEY=sk_live_1";
        for typed in typed.chars() {
            pane.type_env_char(typed);
        }
        let typing = pane.env_typing().expect("the editor is open");
        assert_eq!(
            format!("{typing:?}"),
            format!(
                "EnvTyping {{ key: false, buffer: <{} chars> }}",
                typed.chars().count()
            )
        );
    }
}
