//! The edit lifecycle: arming a field, typing into it, and filing what was
//! typed.
//!
//! Nothing here writes to the shepherd. Every keystroke that lands files a
//! [`PaneEdit`] into [`Edits`], the pane keeps holding the stored value
//! beside it so a row can draw `old -> new`, and the whole set leaves
//! together when the pane closes.

use serde_json::Value;

use super::super::edits::{EditKey, Edits};
use super::super::field::{Field, FieldKind};
use super::super::validation::{self, Refusal};
use super::{ConfigPane, PaneEdit, PaneRow, PaneTarget};

// Link-only (IR-32): the env editor this one is never open beside, and
// the group a structural field carries.
#[cfg(doc)]
use super::EnvTyping;
#[cfg(doc)]
use shep_core::config::ApplyGroup;

/// The pane's open text editor: which field, and what has been typed.
///
/// A struct rather than the three-variant enum this was. Nothing arms and
/// nothing is in flight any more: a keystroke files straight into
/// [`Edits`], and the whole set leaves when the pane closes. Typing is the
/// one state left, so an enum named for a lifecycle would name two states
/// that no longer exist.
///
/// A field's key always exists before its edit starts, which is what keeps
/// this a bare `key: String` rather than the [`Option`] [`EnvTyping`] needs
/// for its `+ add a key` row.
///
/// `Debug` is manual and redacted (IR-41), exact-string-tested below. The
/// buffer is what the operator is halfway through typing.
#[derive(Clone, PartialEq, Eq)]
pub struct PaneTyping {
    /// Which field. Owns [`super::super::app::InputMode::Text`] for as long as
    /// this exists.
    pub key: String,
    /// What has been typed so far.
    pub buffer: String,
}

/// Prints the key and never the buffer. See the type doc for why.
/// Exact-string-tested below (`debug_names_no_value_on_a_pane_typing`).
impl core::fmt::Debug for PaneTyping {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "PaneTyping {{ key: {:?}, buffer: <{} chars> }}",
            self.key,
            self.buffer.chars().count()
        )
    }
}

impl ConfigPane {
    /// The section with every field edit in `edits` applied, in key order,
    /// comments and key order intact, ready for `Request::SetDogConfig`.
    ///
    /// `toml_edit` rather than a re-render of [`Self::values`], and that is
    /// the whole reason this method exists: the request replaces the section
    /// wholesale, so a re-render would delete every comment in it on the
    /// operator's own keystroke.
    ///
    /// This is what a close actually sends: the request replaces the table,
    /// so a batch of edits to one dog is one write, not one per entry. Each
    /// is applied to the document the previous one produced, walked in
    /// [`Edits::iter`]'s own key order.
    ///
    /// An [`EditKey::Env`] entry has no home in a dog's section, a dog
    /// having no env store, and is skipped rather than becoming a key in
    /// it.
    ///
    /// A `null` value removes the key, which is how the pane's empty buffer
    /// unsets one, and is what puts the dog back on its own default.
    ///
    /// [`None`] for a sheep pane, for a set holding no field edit, and for
    /// a section that does not parse, raised by any one entry: a partial
    /// section is worse than none, since the request would replace the
    /// table with it.
    #[must_use]
    pub fn edited_section_with(&self, edits: &Edits) -> Option<String> {
        let section = self.section.as_deref()?;
        let mut doc: toml_edit::DocumentMut = section.parse().ok()?;
        let mut wrote = false;
        for (key, edit) in edits.iter() {
            let EditKey::Field(name) = key else {
                continue;
            };
            let PaneEdit::Set { value, .. } = edit.edit() else {
                continue;
            };
            match value.as_value() {
                Value::Null => {
                    doc.remove(name.as_str());
                }
                Value::Bool(flag) => doc[name.as_str()] = toml_edit::value(*flag),
                // A number that is neither an i64 nor an f64 is not
                // something TOML can hold, so the edit is refused rather
                // than rounded.
                Value::Number(number) => match (number.as_i64(), number.as_f64()) {
                    (Some(int), _) => doc[name.as_str()] = toml_edit::value(int),
                    (None, Some(float)) => doc[name.as_str()] = toml_edit::value(float),
                    (None, None) => return None,
                },
                Value::String(text) => doc[name.as_str()] = toml_edit::value(text.as_str()),
                other => doc[name.as_str()] = toml_edit::value(other.to_string()),
            }
            wrote = true;
        }
        wrote.then(|| doc.to_string())
    }

    /// The open text editor, or [`None`].
    #[must_use]
    pub fn typing(&self) -> Option<&PaneTyping> {
        self.typing.as_ref()
    }

    /// Everything the operator has changed and nothing has written yet.
    #[must_use]
    pub fn edits(&self) -> &Edits {
        &self.edits
    }

    /// Hands the pending set out, leaving the pane holding nothing.
    ///
    /// Takes `&mut self` rather than consuming, which is where this
    /// departs from the pane's own story about closing: `Escape` can
    /// leave the pane on screen, because it offers the parked-field menu
    /// first and that menu reads the pane it is offering about. The write
    /// still goes out on that keypress, so the set has to leave whether
    /// the screen does or not.
    ///
    /// `#[must_use]`: this is the only door the filed set leaves through.
    /// A caller that drops the return value drops every edit with it,
    /// silently, since the pane already holds nothing once this returns.
    #[must_use]
    pub(in crate::lookout) fn close(&mut self) -> Edits {
        core::mem::take(&mut self.edits)
    }

    /// Drops the most recently filed edit and names it, for `u`.
    ///
    /// An open list sub-screen is re-read from what is filed afterwards, so
    /// `u` inside one shows the array it just restored rather than the one
    /// it has undone. One entry is one field, so a `u` there takes the
    /// whole field back to the shepherd's array rather than one keystroke
    /// of it.
    pub(in crate::lookout) fn undo_edit(&mut self) -> Option<EditKey> {
        let undone = self.edits.undo();
        if let Some(key) = self.list.as_ref().map(|list| list.key().to_owned()) {
            let elements = self.filed_elements_of(&key);
            if let Some(list) = self.list.as_mut() {
                list.set_elements(elements);
            }
        }
        undone
    }

    /// Files the opposite of what a bool holds, or the next name in a
    /// choice. Does nothing for a locked field, or for one no keystroke
    /// cycles ([`FieldKind::Text`], [`FieldKind::Integer`],
    /// [`FieldKind::Map`], [`FieldKind::Opaque`]).
    pub fn cycle(&mut self) {
        let Some(PaneRow::Field(index)) = self.cursor() else {
            return;
        };
        let Some(field) = self.fields.fields().get(index) else {
            return;
        };
        if self.lock(&field.key).is_some() {
            return;
        }
        // The base is whatever is already filed for this field, so a
        // second `space` walks the cycle instead of re-deriving the
        // stored value. Nothing filed for this key starts from the stored
        // value, which is what the row is showing.
        let filed_here = match self.edits.get(&EditKey::Field(field.key.clone())) {
            Some(entry) => match entry.edit() {
                PaneEdit::Set { value, .. } => Some(value.as_value()),
                PaneEdit::SetEnv { .. } => None,
            },
            None => None,
        };
        let current = filed_here.or_else(|| self.values.get(&field.key));
        let next = match &field.kind {
            FieldKind::Bool => Value::Bool(!current.and_then(Value::as_bool).unwrap_or(false)),
            FieldKind::Choice(names) | FieldKind::Suggested(names) if !names.is_empty() => {
                let current = current.and_then(Value::as_str);
                let next = current
                    .and_then(|value| names.iter().position(|name| name == value))
                    .map_or(0, |i| (i + 1) % names.len());
                Value::String(names[next].clone())
            }
            FieldKind::Choice(_)
            | FieldKind::Suggested(_)
            | FieldKind::Text
            | FieldKind::Integer
            | FieldKind::Map
            | FieldKind::List(_)
            | FieldKind::Opaque => return,
        };
        let key = field.key.clone();
        self.file_field(key, next);
    }

    /// Files one config field, or drops whatever was filed for it when the
    /// new value is the one the target already holds.
    ///
    /// The comparison is what stops a round trip counting: two `space`
    /// presses on a bool leave the row exactly as the shepherd has it, and
    /// an entry for it would still be counted by the title band, still be
    /// asked about on close, and still be written.
    ///
    /// The one door every config edit files through, which is what keeps
    /// [`ApplyGroup::Structural`] out of the set at all: every caller has
    /// already refused a locked row, and [`Self::lock`] locks exactly the
    /// Structural ones. [`super::super::app::App::close_offer`]'s own walk over
    /// [`Edits::iter`], which is what decides whether the close dialog
    /// appears, rests on that: it never has to ask what a Structural
    /// edit would cost, because one can never be in the set to ask about.
    pub(super) fn file_field(&mut self, key: String, value: Value) {
        if self.stored_value_is(&key, &value) {
            self.edits.remove(&EditKey::Field(key));
            return;
        }
        let impact = self.cost(&key);
        self.edits.set(
            PaneEdit::Set {
                key,
                value: value.into(),
            },
            impact,
        );
    }

    /// Whether `value` is what the target already holds for `key`.
    ///
    /// An absent key and a `null` are the same fact here: the pane renders
    /// both as `(unset)`, so unsetting a field that is already unset is
    /// not a change.
    fn stored_value_is(&self, key: &str, value: &Value) -> bool {
        match self.values.get(key) {
            Some(stored) => stored == value,
            None => value.is_null(),
        }
    }

    /// `d` on the field list: restores the field under the cursor to its
    /// default.
    ///
    /// What gets filed depends on which door the write leaves through, not
    /// on the field's kind:
    ///
    /// - A sheep's write is `Request::SetSheepField`, which re-validates
    ///   the value against `AppConfig`'s own type for the key, and `null`
    ///   only deserializes into an `Option<T>`. So the value filed is
    ///   [`Field::default_value`] when the schema names one, else
    ///   [`Value::Null`]: a bool, a plain `Vec`, or a non-optional scalar
    ///   needs its schema default filed to actually clear, while an
    ///   `Option<T>` field with no default (`cwd` and its like) keeps
    ///   unsetting the way it always has. See [`Field::default_value`]'s
    ///   own doc for why the two `None` cases collapse to the same value.
    /// - A dog's write patches the section's own TOML text (see
    ///   [`Self::edited_section_with`]), where `null` already means
    ///   "remove this key" rather than a value handed to a deserializer.
    ///   Removing the key is exactly a restore: the dog reads its own
    ///   compiled default for whatever is absent. Filing the schema
    ///   default there instead would hard-code the value into the section
    ///   rather than restoring it, so a dog always files [`Value::Null`],
    ///   regardless of the field's default.
    ///
    /// Either way, filed through the same [`Self::file_field`] every other
    /// edit goes through, so a re-edit back to the stored value still drops
    /// the entry rather than counting a no-op.
    ///
    /// Files nothing when the row is already showing its default: see
    /// [`Self::field_shows_default`]. A locked row is refused the same way
    /// [`Self::cycle`] refuses one, since a key that reaches here has
    /// already been refused once, by the caller's own lock check, and this
    /// is the defense behind it.
    pub(in crate::lookout) fn file_default(&mut self) {
        let Some(PaneRow::Field(index)) = self.cursor() else {
            return;
        };
        let Some(field) = self.fields.fields().get(index) else {
            return;
        };
        if self.lock(&field.key).is_some() {
            return;
        }
        let key = field.key.clone();
        if self.field_shows_default(&key) {
            return;
        }
        let value = self.default_for(field);
        self.file_field(key, value);
    }

    /// The value that restores `field` to its default, matching whichever
    /// door the write leaves through. Shared by [`Self::file_default`] (`d`)
    /// and [`Self::apply_typing`]'s empty-buffer case, which is the same
    /// intent from a different key: clearing the field back to nothing.
    ///
    /// - A sheep's write is `Request::SetSheepField`, which re-validates
    ///   against `AppConfig`'s own type for the key, and `null` only
    ///   deserializes into an `Option<T>`. So this files
    ///   [`Field::default_value`] when the schema names one, else
    ///   [`Value::Null`].
    /// - A dog's write patches the section's own TOML text, where `null`
    ///   already means "remove this key" and removing it restores the dog's
    ///   own compiled default. Filing the schema default there instead
    ///   would hard-code it into the section rather than restoring it, so a
    ///   dog always gets [`Value::Null`], regardless of the field's default.
    fn default_for(&self, field: &Field) -> Value {
        match &self.target {
            PaneTarget::Sheep { .. } => field.default_value.clone().unwrap_or(Value::Null),
            PaneTarget::Dog { .. } => Value::Null,
        }
    }

    /// Whether `key`'s row is already showing its stored default, with
    /// nothing filed for it in this session either.
    ///
    /// A sheep's `values` is the effective config, defaults merged in, so a
    /// bool field's default is never `null` and [`Self::stored_value_is`]
    /// cannot answer this for it; [`Self::is_overridden`] is the fact that
    /// can. A dog's `values` is the raw section text with no defaults
    /// merged in, so an absent or `null` key already means default there,
    /// which is exactly what [`Self::stored_value_is`] checks.
    fn field_shows_default(&self, key: &str) -> bool {
        if self.edits.get(&EditKey::Field(key.to_owned())).is_some() {
            return false;
        }
        match &self.target {
            PaneTarget::Sheep { .. } => !self.is_overridden(key),
            PaneTarget::Dog { .. } => self.stored_value_is(key, &Value::Null),
        }
    }

    /// Opens the text editor on the row under the cursor. Does nothing for
    /// a locked field, or for one that is not typed.
    ///
    /// Seeded with what is on screen, except for a secret, which is seeded
    /// empty: the pane renders `<set>` for one and never holds the value,
    /// so a seed would have to invent it.
    pub fn begin_typing(&mut self) {
        let Some(PaneRow::Field(index)) = self.cursor() else {
            return;
        };
        let Some(field) = self.fields.fields().get(index) else {
            return;
        };
        if self.lock(&field.key).is_some()
            || !matches!(
                field.kind,
                FieldKind::Text | FieldKind::Integer | FieldKind::Suggested(_)
            )
        {
            return;
        }
        let seed = if field.secret {
            String::new()
        } else {
            match self.value(&field.key) {
                unset if unset == "(unset)" => String::new(),
                value => value,
            }
        };
        self.typing = Some(PaneTyping {
            key: field.key.clone(),
            buffer: seed,
        });
    }

    /// Appends one typed character.
    pub fn type_char(&mut self, typed: char) {
        if let Some(typing) = self.typing.as_mut() {
            typing.buffer.push(typed);
        }
    }

    /// Removes the last typed character.
    pub fn type_backspace(&mut self) {
        if let Some(typing) = self.typing.as_mut() {
            typing.buffer.pop();
        }
    }

    /// Files the buffer as an edit, typed to the field's kind, or hands
    /// back the reason it did not.
    ///
    /// An empty buffer restores the field's default, through
    /// [`Self::default_for`] — the same value `d` files, and the same
    /// reasoning: `Enter` is an explicit apply, not an ambient one, so a
    /// buffer the operator has cleared all the way and then committed reads
    /// as "put this back," not as a typo. That is a different keypress from
    /// the one below it: a buffer the field's own schema refuses keeps the
    /// editor open rather than filing a value the operator will find out
    /// about later, because *non-empty and wrong* is either a typo or an
    /// operator still mid-word. An empty buffer has nothing left to finish
    /// typing, so there is no "mid-word" reading available for it, only
    /// "unset" or "wrong sheep" — and `Enter` picks unset. Validation runs
    /// here, on the way in, so every entry in the set is one the pane is
    /// willing to send.
    ///
    /// The check is [`super::super::validation::refusal`], which reads the
    /// grammar off the field rather than off the key: shep's own two string
    /// types go through their `FromStr`, a dog's own `pattern` through the
    /// regex it published, and an integer through its schema's `minimum`
    /// and `maximum`.
    ///
    /// This is the only gate a dog's section has. `Request::SetDogConfig`
    /// checks that the table is TOML and writes it, because the dog is the
    /// authority on its own config, so a value filed here is a value that
    /// reaches `dogs.toml` unread. A sheep's write is re-validated against
    /// `AppConfig` by the daemon, so for one of those this is the earlier
    /// of two refusals rather than the only one.
    ///
    /// `#[must_use]`: the returned refusal is what puts the sentence in
    /// the status bar. Dropping it leaves the editor open with nothing
    /// saying why, which is the shape of the bug this gate closed.
    #[must_use]
    pub fn apply_typing(&mut self) -> Option<Refusal> {
        let PaneTyping { key, buffer } = self.typing.take()?;
        let field = self.fields.by_key(&key).cloned();
        if let Some(refused) = field
            .as_ref()
            .and_then(|field| validation::refusal(field, &buffer))
        {
            self.typing = Some(PaneTyping { key, buffer });
            return Some(refused);
        }
        let kind = field.as_ref().map(|field| field.kind.clone());
        let value = match (kind, buffer.as_str()) {
            (_, "") => field
                .as_ref()
                .map_or(Value::Null, |field| self.default_for(field)),
            // Already parsed once by `validation::refusal`, which is what
            // refused every text an `i64` cannot hold. The second parse is
            // the conversion, and its `Err` arm is unreachable rather than
            // a second opinion: filing the string instead would send the
            // daemon a value it refuses for a field the gate just passed.
            (Some(FieldKind::Integer), text) => match text.parse::<i64>() {
                Ok(number) => Value::from(number),
                Err(_) => {
                    self.typing = Some(PaneTyping { key, buffer });
                    return None;
                }
            },
            (_, text) => Value::String(text.to_owned()),
        };
        self.file_field(key, value);
        None
    }

    /// Drops an editor under construction, leaving the pane open.
    pub fn abandon_typing(&mut self) {
        self.typing = None;
    }
}

#[cfg(test)]
mod tests {
    use shep_core::config::{AppConfig, ApplyGroup, apply_group};
    use shep_core::protocol::SheepConfigView;

    use super::super::Lock;
    use super::super::fixtures::{bark_pane, field, filed, filed_impact, secret_dog_pane, web};
    use super::*;

    #[test]
    fn cycling_a_bool_files_a_set_with_the_flipped_value() {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_key("autorestart");
        pane.cycle();
        assert_eq!(filed(&pane, "autorestart"), Some(serde_json::json!(false)));
        assert_eq!(pane.edits().len(), 1, "one key, one entry");
    }

    /// A round trip is not a change, so the entry goes rather than being
    /// filed as the value the sheep already holds: an entry that changes
    /// nothing would still be written and still be counted.
    #[test]
    fn cycling_a_bool_back_to_the_stored_value_unfiles_it() {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_key("autorestart");
        pane.cycle();
        pane.cycle();
        assert_eq!(filed(&pane, "autorestart"), None);
        assert!(pane.edits().is_empty());
    }

    #[test]
    fn space_cycles_a_suggested_field_and_e_still_opens_the_editor() {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_key("kill_signal");
        pane.cycle();
        assert!(
            filed(&pane, "kill_signal").is_some(),
            "space files a suggestion"
        );
        pane.begin_typing();
        assert!(pane.typing().is_some(), "e still opens a free-text editor");
    }

    /// The set records what each edit costs, per entry, so the close
    /// dialog can read the heaviest one without re-deriving anything.
    /// `apply_group` is a fact about the field; a write's fate is a fact
    /// about the flock, which only the shepherd knows.
    #[test]
    fn a_filed_edit_carries_the_fields_own_apply_group() {
        for (key, want) in [
            ("autorestart", ApplyGroup::Live),
            ("watch", ApplyGroup::Live),
            ("autostart", ApplyGroup::NextSpawn),
            ("merge_logs", ApplyGroup::NeedsRespawn),
            ("shutdown_with_message", ApplyGroup::NeedsRespawn),
        ] {
            let mut pane = ConfigPane::sheep(web());
            pane.move_to_key(key);
            pane.cycle();
            assert_eq!(
                pane.cost(key),
                Some(want),
                "the fixture must keep {key} {want:?} or the test means nothing"
            );
            assert_eq!(filed_impact(&pane, key), Some(want), "{key}");
        }
    }

    /// A dog's section belongs to the dog, so shep records no cost for it.
    #[test]
    fn a_dogs_filed_edit_carries_no_apply_group() {
        let mut pane = bark_pane();
        pane.move_to_key("history_bytes");
        pane.begin_typing();
        for _ in 0..8 {
            pane.type_backspace();
        }
        for typed in "8192".chars() {
            pane.type_char(typed);
        }
        assert_eq!(pane.apply_typing(), None, "the field takes what was typed");
        assert_eq!(filed(&pane, "history_bytes"), Some(serde_json::json!(8192)));
        assert_eq!(filed_impact(&pane, "history_bytes"), None);
    }

    /// The invariant [`super::super::app::App::close_offer`]'s own walk over
    /// [`Edits::iter`] rests on: nothing a keystroke can do files a
    /// `Structural` edit, because [`ConfigPane::sheep`] marks those fields
    /// not editable and every filing door checks [`ConfigPane::lock`]
    /// first.
    #[test]
    fn no_key_files_an_edit_for_a_structural_field() {
        let structural: Vec<String> = ConfigPane::sheep(web())
            .fields()
            .fields()
            .iter()
            .filter(|field| apply_group(&field.key) == ApplyGroup::Structural)
            .map(|field| field.key.clone())
            .collect();
        assert_eq!(
            structural,
            vec!["instances".to_owned(), "name".to_owned()],
            "the schema must still carry the two Structural fields"
        );
        for key in structural {
            let mut pane = ConfigPane::sheep(web());
            pane.move_to_key(&key);
            assert_eq!(pane.lock(&key), Some(Lock::Refused), "{key}");
            pane.cycle();
            pane.begin_typing();
            pane.type_char('x');
            assert_eq!(pane.apply_typing(), None, "the field takes what was typed");
            assert!(pane.edits().is_empty(), "{key} reached the set");
        }
    }

    /// A string here would be refused by `AppConfig`'s own deserializer.
    #[test]
    fn typing_into_an_integer_and_applying_files_a_number_not_a_string() {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_key("max_restarts");
        pane.begin_typing();
        let typing = pane.typing().expect("the editor is open");
        assert_eq!(typing.buffer, "32", "the editor opens on what is on screen");
        pane.type_backspace();
        pane.type_backspace();
        for c in "40".chars() {
            pane.type_char(c);
        }
        assert_eq!(pane.apply_typing(), None, "the field takes what was typed");
        assert_eq!(filed(&pane, "max_restarts"), Some(serde_json::json!(40)));
    }

    /// The sibling of `d`'s own defect: emptying an integer field's buffer
    /// used to file `Value::Null` regardless of the field's type, and
    /// `max_restarts` is `u32`, not `Option<u32>`, so the daemon would have
    /// refused it the same way it refused a bare `d` before that fix. An
    /// emptied buffer now files the schema default through the same
    /// [`ConfigPane::default_for`] `d` uses, not `null`.
    #[test]
    fn emptying_an_integer_field_and_applying_files_its_default_not_null() {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_key("max_restarts");
        pane.begin_typing();
        for _ in 0..10 {
            pane.type_backspace();
        }
        assert_eq!(pane.typing().expect("still open").buffer, "");
        assert_eq!(pane.apply_typing(), None, "the field takes what was typed");
        assert_eq!(filed(&pane, "max_restarts"), Some(serde_json::json!(16)));
    }

    /// An `Option<T>` field with no schema default still unsets to `null`
    /// through the emptied-buffer door: [`ConfigPane::default_for`] falls
    /// back to [`Value::Null`] when [`Field::default_value`] is `None`,
    /// which is exactly `cwd`'s case, so this path is unchanged for it.
    #[test]
    fn emptying_a_field_with_no_schema_default_still_unsets_to_null() {
        let config = AppConfig {
            name: "web".into(),
            cwd: Some("/srv/web".into()),
            ..AppConfig::default()
        };
        let mut pane = ConfigPane::sheep(SheepConfigView::new(config, vec!["cwd".into()], vec![]));
        pane.move_to_key("cwd");
        pane.begin_typing();
        assert_eq!(
            pane.typing().expect("the editor is open").buffer,
            "/srv/web",
            "the editor opens on what is on screen"
        );
        for _ in 0..8 {
            pane.type_backspace();
        }
        assert_eq!(pane.apply_typing(), None, "the field takes what was typed");
        assert_eq!(filed(&pane, "cwd"), Some(serde_json::Value::Null));
    }

    /// The request names one key and one JSON value; the daemon
    /// deserializes that value into the field it names, so an integer
    /// field handed `"40"` is refused rather than set.
    #[test]
    fn an_edit_carries_the_key_and_the_typed_value_and_nothing_else() {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_key("max_restarts");
        pane.begin_typing();
        pane.type_backspace();
        pane.type_backspace();
        for c in "40".chars() {
            pane.type_char(c);
        }
        assert_eq!(pane.apply_typing(), None, "the field takes what was typed");
        let writes = pane.close().into_writes();
        let [PaneEdit::Set { key, value }] = writes.as_slice() else {
            panic!("{writes:?}");
        };
        assert_eq!(key, "max_restarts");
        assert_eq!(value.as_value(), &serde_json::json!(40));
        assert!(
            !matches!(value.as_value(), serde_json::Value::String(_)),
            "an integer field must not travel as a string"
        );
    }

    /// The set is what closing hands out, and closing empties it: a set
    /// that has been written is not still pending.
    #[test]
    fn closing_hands_out_every_filed_edit_and_leaves_the_pane_empty() {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_key("autorestart");
        pane.cycle();
        pane.move_to_key("autostart");
        pane.cycle();
        assert_eq!(pane.edits().len(), 2);
        let writes = pane.close().into_writes();
        assert_eq!(writes.len(), 2, "{writes:?}");
        assert!(pane.edits().is_empty(), "the pane keeps nothing back");
        assert!(
            pane.close().into_writes().is_empty(),
            "a second close writes nothing twice"
        );
    }

    /// The confirm sentence quotes what a write would actually mean, not
    /// the digits the operator typed: this is the moment the maintainer's
    /// own report says nothing warned them.
    /// A unit field files the buffer verbatim, resolved unit or not: the
    /// daemon parses `MemSize`, so a pane that rewrote `64` as `64 B`
    /// would be a second grammar to keep in step with it. The resolving
    /// happens on the way to the screen, in `display_value`, and nowhere
    /// else.
    #[test]
    fn a_unit_field_files_the_buffer_and_never_a_resolved_form() {
        for typed in ["64", "512M"] {
            let mut pane = ConfigPane::sheep(web());
            pane.move_to_key("max_memory");
            pane.begin_typing();
            for c in typed.chars() {
                pane.type_char(c);
            }
            assert_eq!(pane.apply_typing(), None, "{typed} is a size shep takes");
            assert_eq!(
                filed(&pane, "max_memory"),
                Some(serde_json::json!(typed)),
                "{typed}"
            );
        }
    }

    /// The other half of the test above, which used to prove this same
    /// field filed `banana`. It did, all the way to the daemon, which was
    /// the backstop for a sheep and does not exist for a dog.
    #[test]
    fn a_unit_field_refuses_a_buffer_its_own_grammar_does_not_take() {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_key("max_memory");
        pane.begin_typing();
        for c in "banana".chars() {
            pane.type_char(c);
        }
        let refused = pane.apply_typing().expect("a size shep cannot read");
        assert!(refused.text.contains("max_memory"), "{}", refused.text);
        assert!(refused.text.contains("banana"), "{}", refused.text);
        assert!(refused.text.contains("512M, 2G"), "{}", refused.text);
        assert_eq!(filed(&pane, "max_memory"), None, "nothing was filed");
        assert!(
            pane.typing().is_some(),
            "the editor stays open on what was typed"
        );
    }

    /// The one editor a secret can be typed into, and `secret_dog_pane` is
    /// the only fixture that reaches it. Seeded empty rather than with what
    /// the section holds: the screen draws `<set>` for a secret and the
    /// pane must not hand the old credential back for a backspace to edit.
    #[test]
    fn a_dogs_secret_field_seeds_its_editor_empty_and_files_what_was_typed() {
        let mut pane = secret_dog_pane();
        pane.move_to_key("webhook");
        pane.begin_typing();
        assert_eq!(
            pane.typing().expect("the editor is open").buffer,
            "",
            "a secret seeds empty, however much the section holds"
        );
        for typed in "https://hook/NEW".chars() {
            pane.type_char(typed);
        }
        assert_eq!(pane.apply_typing(), None, "the field takes what was typed");
        assert_eq!(
            filed(&pane, "webhook"),
            Some(serde_json::json!("https://hook/NEW"))
        );
        let debug = format!("{pane:?}");
        assert!(!debug.contains("OLD"), "{debug}");
        assert!(!debug.contains("NEW"), "{debug}");
    }

    /// Re-rendering from parsed values would delete every comment in a
    /// file shep does not author.
    #[test]
    fn an_edited_section_keeps_its_comments_and_changes_one_key() {
        let pane = bark_pane();
        let mut edits = Edits::default();
        edits.set(field("poll", serde_json::json!("30s")), None);
        let out = pane
            .edited_section_with(&edits)
            .expect("the fixture section parses");
        assert!(out.contains("# how often"), "{out}");
        assert!(out.contains("poll = \"30s\""), "{out}");
        assert!(out.contains("history_bytes = 4096"), "{out}");
        assert!(out.contains("url = \"https://hooks.example/x\""), "{out}");
    }

    /// An empty buffer unsets the key, putting it back on the dog's own
    /// default.
    #[test]
    fn a_null_edit_removes_the_key_from_the_section() {
        let pane = bark_pane();
        let mut edits = Edits::default();
        edits.set(field("history_bytes", serde_json::Value::Null), None);
        let out = pane
            .edited_section_with(&edits)
            .expect("the fixture section parses");
        assert!(!out.contains("history_bytes"), "{out}");
        assert!(out.contains("# how often"), "{out}");
    }

    /// A dog's write patches its section's own TOML text rather than going
    /// through `Request::SetSheepField`'s deserializer, so `null` already
    /// means "remove this key" there, not a value refused for the wrong
    /// type. `d` must keep filing `Value::Null` for a dog even when its
    /// schema names a non-null default, or it would hard-code the default
    /// into the section instead of restoring it.
    #[test]
    fn d_on_a_dog_field_files_null_even_with_a_schema_default() {
        let schema = serde_json::json!({
            "properties": {
                "merge_logs": { "type": "boolean", "default": true },
            },
        });
        let mut pane = ConfigPane::dog("bark".into(), None, schema, "merge_logs = false\n".into());
        pane.move_to_key("merge_logs");
        pane.file_default();
        assert_eq!(
            filed(&pane, "merge_logs"),
            Some(Value::Null),
            "a dog restores by removing the key, not by filing its default"
        );
        let out = pane
            .edited_section_with(pane.edits())
            .expect("the fixture section parses");
        assert!(!out.contains("merge_logs"), "{out}");
    }

    /// A dog section takes every filed edit in one write rather than one
    /// write per edit.
    #[test]
    fn a_dog_section_takes_every_filed_edit_in_one_write() {
        let pane = bark_pane();
        let mut edits = Edits::default();
        edits.set(field("url", serde_json::json!("http://a")), None);
        edits.set(field("timeout", serde_json::json!(30)), None);
        let section = pane
            .edited_section_with(&edits)
            .expect("the fixture parses");
        assert!(section.contains("http://a"), "{section}");
        assert!(section.contains("30"), "{section}");
    }

    /// An env edit has no home in a dog's section and must not silently
    /// become a key in it.
    #[test]
    fn an_env_edit_is_ignored_by_a_dog_section() {
        let pane = bark_pane();
        let mut edits = Edits::default();
        edits.set(
            PaneEdit::SetEnv {
                key: "SECRET".to_owned(),
                value: None,
            },
            None,
        );
        assert_eq!(pane.edited_section_with(&edits), None);
    }

    /// A stray env edit in a set that also carries a field edit must not
    /// revive the old "one bad entry aborts the whole batch" bug: skipping
    /// the env edit and an early return on any non-field entry both leave
    /// `an_env_edit_is_ignored_by_a_dog_section` green, since an env-only
    /// set returns `None` either way. Only a mixed set tells them apart.
    #[test]
    fn a_field_edit_lands_even_when_the_same_batch_carries_an_env_edit() {
        let pane = bark_pane();
        let mut edits = Edits::default();
        edits.set(field("poll", serde_json::json!("30s")), None);
        edits.set(
            PaneEdit::SetEnv {
                key: "SECRET".to_owned(),
                value: None,
            },
            None,
        );
        let out = pane
            .edited_section_with(&edits)
            .expect("the field edit alone should still write");
        assert!(out.contains("poll = \"30s\""), "{out}");
        assert!(!out.contains("SECRET"), "{out}");
    }

    /// A section here would send a sheep's config out through a dog's
    /// door.
    #[test]
    fn a_sheep_pane_has_no_section_to_edit() {
        let pane = ConfigPane::sheep(web());
        let mut edits = Edits::default();
        edits.set(field("cwd", serde_json::json!("/srv")), None);
        assert_eq!(pane.edited_section_with(&edits), None);
    }

    /// The buffer is what the operator is halfway through typing, and on
    /// the env screen that is the secret itself (IR-41).
    #[test]
    fn debug_names_no_value_on_a_pane_typing() {
        let typing = PaneTyping {
            key: "cwd".into(),
            buffer: "/home/ada/secret-project".into(),
        };
        assert_eq!(
            format!("{typing:?}"),
            r#"PaneTyping { key: "cwd", buffer: <24 chars> }"#
        );
    }
}
