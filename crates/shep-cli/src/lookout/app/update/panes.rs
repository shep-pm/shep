//! What a pane's read or write came back as: the settings screen, a dog
//! toggle, and the secrets pane.

use super::*;

impl App {
    /// `Msg::Settings`'s arm.
    ///
    /// The screen opens on what this read found; a failed read leaves
    /// the dashboard up. A landed write's re-read and `r` land here too,
    /// with `body` already `Body::Settings`, so `opening` is false and
    /// the cursor survives.
    pub(super) fn on_settings(&mut self, result: Result<SettingsSnapshot, String>) -> Effect {
        let opening = self.settings().is_none();
        match result {
            // A config pane opened while this read was in flight, so this
            // reply is the stale one. Adopting it would replace the pane with
            // a settings screen the operator has moved on from, and leave
            // `config_target` and `close_dialog` describing a screen not up.
            Ok(_) if self.config_pane().is_some() => {}
            Ok(snapshot) => {
                // An action armed while the read was in flight: once
                // the screen is up, `on_settings_key` no-ops `Confirm`
                // and the prompt would be unreachable.
                self.action = None;
                // A filter box left open would keep eating every
                // keystroke the settings keymap owns: `on_key` checks
                // the mode first. The query itself is kept.
                self.mode = InputMode::Normal;
                // Reset to the top only while `opening`: `Settings::cursor`
                // clamps on every read, so a preserved `Viewport` past a
                // shorter list still lands somewhere real. A dog already opened
                // keeps its tree, so a landed write need not re-probe its binary.
                let kept = self.settings().map(|settings| {
                    (
                        settings.view.clone(),
                        settings.folded.clone(),
                        settings.dog_trees.clone(),
                    )
                });
                let mut settings = Settings::new(snapshot);
                if !opening {
                    if let Some((view, folded, trees)) = kept {
                        settings.view = view;
                        settings.folded = folded;
                        settings.dog_trees = trees;
                    }
                    let len = settings.rows().len();
                    settings.view.clamp(len);
                }
                self.body = Body::Settings(settings);
            }
            Err(message) => {
                self.notice = Some(Notice::grave(message));
            }
        }
        Effect::None
    }

    /// `Msg::SettingWritten`'s arm.
    ///
    /// `Ok` re-reads rather than folding the write into the row, which covers
    /// `Unset` too. `Err` reopens the editor for the two free-text fields, so
    /// a long path need not be retyped. Both arms act on the screen only when
    /// this reply is the one it is waiting on: `Settings::resolve` answers that.
    pub(super) fn on_setting_written(
        &mut self,
        edit: SettingEdit,
        ticket: u64,
        result: Result<(), String>,
    ) -> Effect {
        let mine = self
            .settings_mut()
            .is_some_and(|settings| settings.resolve(ticket));
        match result {
            Ok(()) => self.reread_settings(),
            Err(message) => {
                // Split so no borrow of `self.body` is held across
                // the `self.notice` assignment below.
                if mine && let Some((target, buffer)) = reopened_editor(&edit) {
                    if let Some(settings) = self.settings_mut() {
                        settings.pending = Some(Pending::Typing { target, buffer });
                    }
                    // The overlay closes rather than the editor reopening
                    // beneath it: text mode restored from a message would take
                    // the keyboard while the box is still drawn. Closing it is
                    // the lesser surprise, and `h` reopens the box.
                    self.keymap_open = false;
                    self.mode = InputMode::Text;
                }
                self.notice = Some(Notice::grave(message));
                Effect::None
            }
        }
    }

    /// `Msg::DogPane`'s arm. The probe's own handling lives in
    /// [`crate::lookout::app::dog_pane`]: this is the arm, so every pane
    /// result reads through one file.
    pub(super) fn on_dog_pane_answer(
        &mut self,
        name: String,
        adopted_path: Option<PathBuf>,
        env_keys: Vec<String>,
        result: Result<serde_json::Value, String>,
    ) -> Effect {
        self.on_dog_pane(name, adopted_path, env_keys, result)
    }

    /// `Msg::SheepDogNames`'s arm. The listing's own handling lives in
    /// [`crate::lookout::app::pane_dogs`], for the same reason.
    pub(super) fn on_sheep_dog_names_answer(
        &mut self,
        sheep: String,
        ask: u64,
        dogs: Vec<ListedDog>,
    ) -> Effect {
        self.on_sheep_dog_names(&sheep, ask, dogs)
    }

    /// `Msg::SheepDog`'s arm, handled in [`crate::lookout::app::pane_dogs`].
    pub(super) fn on_sheep_dog_answer(&mut self, sheep: &str, entry: SheepDogEntry) -> Effect {
        self.on_sheep_dog(sheep, entry)
    }

    /// `Msg::DogWritten`'s arm.
    ///
    /// `Ok` raises the daemon half: `Cycle` arms, `Confirm` writes the file,
    /// this arm asks the shepherd, whether or not the screen is still waiting
    /// on this ticket, since the file already says the dog is on or off. `Err`
    /// clears only the prompt this ticket raised.
    pub(super) fn on_dog_written(
        &mut self,
        edit: DogEdit,
        ticket: u64,
        result: Result<DogEntry, String>,
    ) -> Effect {
        match result {
            Ok(entry) => Effect::Send(Sent::Dog {
                name: edit.name,
                enable: edit.enable,
                entry,
                ticket,
            }),
            Err(message) => {
                if let Some(settings) = self.settings_mut() {
                    settings.resolve(ticket);
                }
                self.notice = Some(Notice::grave(message));
                Effect::None
            }
        }
    }

    /// `Msg::DogEnvWritten`'s arm.
    ///
    /// A dog's `env` has no daemon half of its own: the shepherd builds it
    /// when it builds the dog's app, which it does at boot, on a reload, and
    /// on an enable of a dog at rest. A dog already up keeps the environment
    /// it was started with, so the notice names the two verbs that give it
    /// this one. The requests the same close built go out behind it.
    pub(super) fn on_dog_env_written(
        &mut self,
        name: &str,
        ticket: u64,
        then: Vec<Sent>,
        result: Result<(), String>,
    ) -> Effect {
        if let Some(settings) = self.settings_mut() {
            settings.resolve(ticket);
        }
        match result {
            Ok(()) => {
                self.notice = Some(Notice::info(format!(
                    "{name}: its env is written. A dog already up keeps the one it started with; \
                     `shep disable {name}` then `shep enable {name}` gives it this one"
                )));
                if then.is_empty() {
                    Effect::None
                } else {
                    Effect::SendAll(then)
                }
            }
            Err(message) => {
                self.notice = Some(Notice::grave(message));
                Effect::None
            }
        }
    }

    /// `Msg::DogTuneWritten`'s arm.
    ///
    /// The two keys answer differently. `boot_first_dogs` is read at boot, so a
    /// landed write has no daemon half. A grant has one: a dog already up would
    /// otherwise keep the grants it was started with. Whether it is enabled
    /// arrives with the answer, not off the screen here.
    pub(super) fn on_dog_tune_written(
        &mut self,
        tune: DogTune,
        ticket: u64,
        result: Result<DogTuneLanded, String>,
    ) -> Effect {
        let name = tune.dog().to_owned();
        match result {
            Ok(DogTuneLanded::BootOrder) => {
                if let Some(settings) = self.settings_mut() {
                    settings.resolve(ticket);
                }
                self.notice = Some(Notice::info(format!(
                    "{name}: the next shepherd boot starts it in a different place"
                )));
                self.reread_settings()
            }
            Ok(DogTuneLanded::Grants { entry, enabled }) => {
                if !enabled || matches!(self.link, Link::Lost { .. }) {
                    if let Some(settings) = self.settings_mut() {
                        settings.resolve(ticket);
                    }
                    self.notice = Some(Notice::info(format!(
                        "{name}: its grants are recorded, and it picks them up when the shepherd \
                         next starts it"
                    )));
                    return self.reread_settings();
                }
                Effect::Send(Sent::GrantDog {
                    name,
                    entry,
                    ticket,
                })
            }
            Err(message) => {
                if let Some(settings) = self.settings_mut() {
                    settings.resolve(ticket);
                }
                self.notice = Some(Notice::grave(message));
                Effect::None
            }
        }
    }

    /// `Msg::Secrets`'s arm.
    ///
    /// Dropped when the pane has since closed, the way a settings
    /// read that outraced a config pane is: adopting it would reopen
    /// a screen the operator has already left.
    pub(super) fn on_secrets(
        &mut self,
        environment: String,
        result: Result<Box<SecretsModel>, String>,
    ) -> Effect {
        if let Body::Secrets(pane) = &mut self.body {
            match result {
                Ok(model) => {
                    // A fresh read describes the store as it is now;
                    // an arm from before it landed named a row this
                    // model may no longer even have.
                    pane.armed = None;
                    // Only on the very first load, where the pane's
                    // model is still the empty default and so has no
                    // tab yet: the daemon's own default environment
                    // wins the tab it lands on.
                    let first_load = pane.model.environments.is_empty();
                    if first_load {
                        pane.tab = model
                            .environments
                            .iter()
                            .position(|candidate| candidate == &environment)
                            .unwrap_or(0);
                    }
                    // The environments list is a union recomputed on
                    // every load and can shrink, so clamp a tab past
                    // the new end onto the last surviving one
                    // instead of dangling.
                    pane.tab = pane.tab.min(model.environments.len().saturating_sub(1));
                    // A selection on the trailing `+ new key` sentinel stays
                    // on it under the fresh model's own row count, rather than
                    // the ordinary clamp below landing it on the new model's
                    // last real row. Excludes the first load's empty model.
                    pane.selected = if !first_load && pane.selected_is_new_key_row() {
                        model.rows.len()
                    } else {
                        pane.selected.min(model.rows.len().saturating_sub(1))
                    };
                    pane.model = model;
                    // The row count clamp above says nothing about collapse
                    // state, and `collapsed` survives a reload: the surviving
                    // index can still name a row a still-folded namespace
                    // hides. Same fallback the `Collapse` arm uses.
                    if pane
                        .model
                        .rows
                        .get(pane.selected)
                        .is_some_and(|row| pane.is_collapsed(&row.source))
                    {
                        pane.move_by(0);
                    }
                }
                Err(message) => {
                    self.notice = Some(Notice::grave(message));
                }
            }
        }
        Effect::None
    }

    /// `Msg::Revealed`'s arm. The read's own handling lives in
    /// [`crate::lookout::app::secrets_write`], for the same reason.
    pub(super) fn on_revealed_answer(
        &mut self,
        key: String,
        environment: String,
        value: Option<RevealedValue>,
    ) -> Effect {
        self.on_revealed(&key, &environment, value);
        Effect::None
    }

    /// `Msg::SecretWritten`'s arm.
    ///
    /// `Ok` re-reads (like `Msg::SettingWritten`) so the table shows
    /// the file's new contents rather than what was typed, and clears
    /// any revealed value, which belonged to the prior store. `Err`
    /// raises no reload, so the table keeps its last known-good read.
    pub(super) fn on_secret_written(&mut self, result: Result<bool, String>) -> Effect {
        match result {
            Ok(true) => {
                self.hide_revealed();
                Effect::LoadSecrets
            }
            // `secrets::unset` found no slot. Nothing changed, so
            // nothing is re-read, and the operator hears about it: a
            // destructive action reporting success over a no-op is the
            // one answer this pane must never give.
            Ok(false) => {
                self.notice = Some(Notice::grave(NOTHING_REMOVED.to_string()));
                Effect::None
            }
            Err(message) => {
                self.notice = Some(Notice::grave(message));
                Effect::None
            }
        }
    }
}
