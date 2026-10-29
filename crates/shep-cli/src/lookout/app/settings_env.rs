//! An adopted dog's `env` on the settings screen: the key/value editor `e` and
//! `Enter` open on a key or `+ new`, the removal `d` arms, and the write both
//! arm for `Enter` to send.
//!
//! The dog pane files the same edits through [`ConfigPane`]; this screen
//! reaches the same [`DogEnvWrite`] and the same writer, so the two doors cannot
//! leave an entry in two shapes.

use std::path::Path;

use super::*;
use crate::commands::shep_toml::{ShepToml, ShepTomlError};

impl DogEnvWrite {
    /// Writes every edit to `name`'s `[daemon] adopted_dogs` entry in the
    /// `shep.toml` at `path`, in one locked edit.
    ///
    /// # Errors
    /// Whatever [`ShepToml::try_edit`] and
    /// [`ShepToml::set_adopted_dog_env`] refuse with. The file is untouched.
    pub(crate) fn apply(&self, path: &Path) -> Result<(), ShepTomlError> {
        ShepToml::try_edit::<(), ShepTomlError>(path, |cfg| {
            for (key, value) in &self.edits {
                cfg.set_adopted_dog_env(&self.name, key, value.as_ref().map(EnvValue::as_str))?;
            }
            Ok(())
        })
    }
}

impl Settings {
    /// The dog's name and the editor `e` or `Enter` on the cursor's row opens,
    /// or [`None`] on a row that is not one of a dog's `env` keys.
    ///
    /// An existing key opens on VALUE, empty: no value is ever read back, so
    /// there is none to seed it with.
    pub(in crate::lookout::app) fn env_editor_at_cursor(&self) -> Option<(String, KeyValues)> {
        match self.cursor()? {
            SettingsRow::DogEnvKey { dog, index } => {
                let dog = self.dog(dog)?;
                let key = dog.env_keys.get(index)?;
                Some((dog.name.clone(), KeyValues::under(Grammar::Env, key)))
            }
            SettingsRow::DogEnvNew(dog) => {
                Some((self.dog(dog)?.name.clone(), KeyValues::add(Grammar::Env)))
            }
            _ => None,
        }
    }

    /// The key under the cursor, with its dog, when it is a dog's `env` key.
    fn env_key_at_cursor(&self) -> Option<(String, String)> {
        let SettingsRow::DogEnvKey { dog, index } = self.cursor()? else {
            return None;
        };
        let dog = self.dog(dog)?;
        Some((dog.name.clone(), dog.env_keys.get(index)?.clone()))
    }

    /// Whether `name`'s `env` already holds `key`.
    fn holds_env_key(&self, name: &str, key: &str) -> bool {
        self.dog_named(name)
            .is_some_and(|dog| dog.env_keys.iter().any(|held| held == key))
    }
}

impl App {
    /// `e` or `Enter` on one of a dog's `env` rows: opens the key/value
    /// editor and gives it the keyboard. Answers whether the cursor was on
    /// such a row, so the caller leaves the key alone when it was.
    ///
    /// Behind [`Self::authorize_write`], so the refusal arrives before a key
    /// is typed.
    pub(super) fn open_env_editor(&mut self) -> bool {
        let Some(settings) = self.settings() else {
            return false;
        };
        let Some((dog, editor)) = settings.env_editor_at_cursor() else {
            return false;
        };
        if settings.pending.is_some() || self.authorize_write().is_none() {
            return true;
        }
        if let Some(settings) = self.settings_mut() {
            settings.pending = Some(Pending::TypingEnv { dog, editor });
            self.mode = InputMode::Text;
        }
        true
    }

    /// `d` on one of a dog's `env` keys: arms its removal. Answers whether the
    /// cursor was on such a key.
    pub(super) fn arm_env_removal(&mut self) -> bool {
        let now = self.now;
        let Some((name, key)) = self.settings().and_then(Settings::env_key_at_cursor) else {
            return false;
        };
        if self.authorize_write().is_none() {
            return true;
        }
        let text = env_confirm_text(&name, &[(key.clone(), None)]);
        if let Some(settings) = self.settings_mut() {
            settings.pending = Some(Pending::DogEnvArmed {
                name,
                edits: vec![(key, None)],
                text,
                at: now,
            });
        }
        true
    }

    /// The env editor's keymap, in force while a [`Pending::TypingEnv`] owns
    /// [`InputMode::Text`].
    ///
    /// `Enter` on KEY moves to VALUE, and on VALUE arms the write: a key the
    /// grammar refuses, and a key the file already holds, keep the editor open
    /// with what was typed and say so by name. `Esc` drops the editor.
    pub(super) fn on_settings_env_key(&mut self, key: KeyPress) -> Effect {
        let now = self.now;
        let Some(settings) = self.settings_mut() else {
            return Effect::None;
        };
        let Some(Pending::TypingEnv { dog, editor }) = settings.pending.as_mut() else {
            return Effect::None;
        };
        let refusal = match key {
            KeyPress::Quit => return Effect::Quit,
            KeyPress::TextChar(_) | KeyPress::TextBackspace | KeyPress::TextEdit(_) => {
                editor.handle(key);
                None
            }
            KeyPress::NextField | KeyPress::PrevField => match editor.step(key == KeyPress::PrevField)
            {
                Outcome::Refused(refusal) => Some(refusal.sentence(editor.key().as_str())),
                Outcome::Moved | Outcome::Submitted => None,
            },
            KeyPress::TextApply => match editor.submit() {
                Outcome::Refused(refusal) => Some(refusal.sentence(editor.key().as_str())),
                Outcome::Moved => None,
                Outcome::Submitted => {
                    let name = dog.clone();
                    let edits = env_edits(editor);
                    return match self.arm_env_write(name, edits, now) {
                        Ok(()) => Effect::None,
                        Err(why) => {
                            self.notice = Some(Notice::grave(why));
                            Effect::None
                        }
                    };
                }
            },
            KeyPress::TextAbandon => {
                settings.pending = None;
                self.mode = InputMode::Normal;
                None
            }
            _ => None,
        };
        if let Some(why) = refusal {
            self.notice = Some(Notice::grave(why));
        }
        Effect::None
    }

    /// Arms `edits` against `name`'s `env`, closing the editor, or says why
    /// the editor has to stay open.
    ///
    /// A key `shep.toml` already holds is refused by name when it would be
    /// added or renamed onto: changing the one that is there is what `e` on
    /// its own row does.
    fn arm_env_write(
        &mut self,
        name: String,
        edits: Vec<(String, Option<EnvValue>)>,
        now: Instant,
    ) -> Result<(), String> {
        let Some(settings) = self.settings_mut() else {
            return Ok(());
        };
        let taken = settings.env_typing().and_then(|(_, editor)| {
            let key = editor.key().as_str();
            (editor.opened_under() != Some(key) && settings.holds_env_key(&name, key))
                .then(|| key.to_owned())
        });
        if let Some(key) = taken {
            return Err(format!(
                "{key} is already in {name}'s env: `e` on its row changes it"
            ));
        }
        let text = env_confirm_text(&name, &edits);
        settings.pending = Some(Pending::DogEnvArmed {
            name,
            edits,
            text,
            at: now,
        });
        self.mode = InputMode::Normal;
        Ok(())
    }
}

/// What a submitted editor writes: the key set to the typed value, the key
/// removed when it is an existing one with nothing typed, or a rename as the
/// old key's removal and the new one's set.
fn env_edits(editor: &KeyValues) -> Vec<(String, Option<EnvValue>)> {
    let key = editor.key().as_str().to_owned();
    let typed = editor.value().as_str().to_owned();
    match editor.opened_under() {
        None => vec![(key, Some(EnvValue::from(typed)))],
        Some(under) if under == key => {
            vec![(key, (!typed.is_empty()).then(|| EnvValue::from(typed)))]
        }
        Some(under) => vec![(under.to_owned(), None), (key, Some(EnvValue::from(typed)))],
    }
}

/// The sentence `Enter` is asked to confirm. Names keys, never values.
fn env_confirm_text(name: &str, edits: &[(String, Option<EnvValue>)]) -> String {
    match edits {
        [(key, Some(_))] => format!("set {key} in {name}'s env in shep.toml?"),
        [(key, None)] => format!("remove {key} from {name}'s env in shep.toml?"),
        [(old, None), (new, Some(_))] => {
            format!("rename {old} to {new} in {name}'s env in shep.toml?")
        }
        _ => format!("change {name}'s env in shep.toml?"),
    }
}
