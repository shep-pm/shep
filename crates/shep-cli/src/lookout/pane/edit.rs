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
use super::super::textbox::{TextBox, TextEdit};
use super::{ConfigPane, PaneEdit, PaneRow, PaneTarget, UNSET};
use crate::dog_settings::{self as validation, Refusal};

// Link-only (IR-32): the key/value editor this one is never open beside, and
// the group a structural field carries.
#[cfg(doc)]
use crate::lookout::keyvalue::KeyValues;
#[cfg(doc)]
use shep_core::config::ApplyGroup;

/// The pane's open text editor: which field, and what has been typed.
///
/// A struct rather than an enum: nothing arms and nothing is in flight, a
/// keystroke files straight into [`Edits`], and typing is the one state left.
/// A field's key always exists before its edit starts, which is what keeps
/// this a bare `key: String`: [`KeyValues`] has to be able to hold a key that
/// does not exist yet. `Debug` is manual and redacted (IR-41).
#[derive(Clone, PartialEq, Eq)]
pub struct PaneTyping {
    /// Which field. Owns [`super::super::app::InputMode::Text`] for as long as
    /// this exists.
    pub key: String,
    /// What has been typed so far.
    pub buffer: TextBox,
}

/// Whether `value` is `null` or an empty array or table.
fn holds_nothing(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

/// Prints the key and never the buffer. See the type doc for why.
/// Exact-string-tested below (`debug_names_no_value_on_a_pane_typing`).
impl core::fmt::Debug for PaneTyping {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "PaneTyping {{ key: {:?}, buffer: <{} chars> }}",
            self.key,
            self.buffer.char_count()
        )
    }
}

impl ConfigPane {
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
    /// Takes `&mut self` rather than consuming: `Escape` can leave the pane on
    /// screen, because it offers the parked-field menu first, and that menu
    /// reads the pane it is offering about. The set has to leave either way.
    ///
    /// `#[must_use]`: this is the only door the filed set leaves through.
    #[must_use]
    pub(in crate::lookout) fn close(&mut self) -> Edits {
        self.fold_entry_edits();
        let edits = core::mem::take(&mut self.edits);
        self.reshapes = Default::default();
        self.resync_entries();
        edits
    }

    /// Drops the most recently filed edit and names it, for `u`.
    ///
    /// An open list sub-screen is re-read from what is filed afterwards, so
    /// `u` inside one shows the array it just restored. One entry is one
    /// field, so a `u` there takes the whole field back to the shepherd's.
    pub(in crate::lookout) fn undo_edit(&mut self) -> Option<EditKey> {
        let undone = self.edits.undo();
        if let Some(EditKey::Field(key)) = &undone
            && self.entries_field(key).is_some()
        {
            self.restore_reshape(key);
            self.resync_entries();
            self.retain_known_open();
        }
        if let Some(key) = self.list.as_ref().map(|list| list.key().to_owned()) {
            let elements = self.filed_elements_of(&key);
            if let Some(list) = self.list.as_mut() {
                list.set_elements(elements);
            }
        }
        undone
    }

    /// Files the opposite of what a bool holds, or the next name in a
    /// choice, or a name of a checklist toggled. Does nothing for a locked field, or for one no keystroke
    /// cycles ([`FieldKind::Text`], [`FieldKind::Integer`],
    /// [`FieldKind::Number`], [`FieldKind::Map`], [`FieldKind::Opaque`],
    /// [`FieldKind::Entries`]).
    pub fn cycle(&mut self) {
        if let Some(PaneRow::Choice { field, item }) = self.cursor() {
            self.toggle_choice(field, item);
            return;
        }
        let Some(field) = self.cursor_field() else {
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
        let current = filed_here.or_else(|| self.stored_of(&field.key));
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
            | FieldKind::Number
            | FieldKind::Map
            | FieldKind::List(_)
            | FieldKind::Entries(_)
            | FieldKind::Union(_)
            | FieldKind::Opaque => return,
        };
        let key = field.key.clone();
        self.file_field(key, next);
    }

    /// Files one config field, or drops whatever was filed for it when the
    /// new value is the one the target already holds.
    ///
    /// The comparison is what stops a round trip counting. Two `space`
    /// presses on a bool leave the row as the shepherd has it. An entry for
    /// it would still be counted by the title band and written. The one door
    /// every config edit files through keeps [`ApplyGroup::Structural`] out.
    pub(super) fn file_field(&mut self, key: String, value: Value) {
        if let Some(place) = self.place(&key) {
            self.file_arm(place, value);
            return;
        }
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
    /// both as `(unset)`, so unsetting a field that is already unset is not a
    /// change. For an array or map of tables an empty one is the same fact.
    fn stored_value_is(&self, key: &str, value: &Value) -> bool {
        let stored = self.stored_of(key).unwrap_or(&Value::Null);
        stored == value
            || (self.entries_field(key).is_some() && holds_nothing(stored) && holds_nothing(value))
    }

    /// `d` on the field list: restores the field under the cursor to its
    /// default.
    ///
    /// What gets filed depends on which door the write leaves through, not on
    /// the field's kind: see [`Self::default_for`], which spells both out.
    /// Either way it goes through [`Self::file_field`], so a re-edit back to
    /// the stored value drops the entry rather than counting a no-op.
    pub(in crate::lookout) fn file_default(&mut self) {
        let Some(field) = self.cursor_field() else {
            return;
        };
        if self.lock(&field.key).is_some() {
            return;
        }
        let key = field.key.clone();
        if self.place(&key).is_some() {
            self.restore_arm_field(&key);
            return;
        }
        if self.field_shows_default(&key) {
            return;
        }
        let value = self.default_for(field);
        self.file_field(key, value);
    }

    /// The value that restores `field` to its default, matching whichever door
    /// the write leaves through. Shared by [`Self::file_default`] and [`Self::apply_typing`].
    ///
    /// A sheep's write re-validates against `AppConfig`'s own type, and `null`
    /// only deserializes into an `Option<T>`, so this files
    /// [`Field::default_value`] when the schema names one, else [`Value::Null`].
    /// A dog's write patches TOML, where `null` means remove this key.
    fn default_for(&self, field: &Field) -> Value {
        match &self.target {
            PaneTarget::Sheep { .. } => field.default_value.clone().unwrap_or(Value::Null),
            PaneTarget::Dog { .. } | PaneTarget::SheepDog { .. } => Value::Null,
        }
    }

    /// Whether `key`'s row is already showing its stored default, with
    /// nothing filed for it in this session either.
    ///
    /// A sheep's `values` is the effective config, defaults merged in, so a
    /// bool field's default is never `null` and [`Self::stored_value_is`]
    /// cannot answer this for it; [`Self::is_overridden`] is the fact that
    /// can. A dog's `values` is raw section text with no defaults merged in.
    fn field_shows_default(&self, key: &str) -> bool {
        if self.edits.get(&EditKey::Field(key.to_owned())).is_some() {
            return false;
        }
        match &self.target {
            PaneTarget::Sheep { .. } => !self.is_overridden(key),
            PaneTarget::Dog { .. } | PaneTarget::SheepDog { .. } => {
                self.stored_value_is(key, &Value::Null)
            }
        }
    }

    /// Opens the text editor on the row under the cursor. Does nothing for
    /// a locked field, or for one that is not typed.
    ///
    /// Seeded with what is on screen, except for a secret, which is seeded
    /// empty: the pane renders `<set>` for one and never holds the value,
    /// so a seed would have to invent it.
    pub fn begin_typing(&mut self) {
        let Some(field) = self.cursor_field() else {
            return;
        };
        if self.lock(&field.key).is_some()
            || !matches!(
                field.kind,
                FieldKind::Text | FieldKind::Integer | FieldKind::Number | FieldKind::Suggested(_)
            )
        {
            return;
        }
        let seed = if field.secret {
            String::new()
        } else {
            match self.value(&field.key) {
                unset if unset == UNSET => String::new(),
                value => value,
            }
        };
        self.typing = Some(PaneTyping {
            key: field.key.clone(),
            buffer: TextBox::from(seed),
        });
    }

    /// Opens an editor on the cursor's row and fills it with `text`.
    ///
    /// [`Self::begin_typing`]'s seed is what the row shows, which is the wrong
    /// seed for a caller that already holds the whole text: the settings screen
    /// hands the finished buffer here rather than retyping it. Validation stays
    /// in [`Self::apply_typing`], so this is the same gate a keystroke goes through.
    pub(in crate::lookout) fn type_over(&mut self, text: &str) {
        self.begin_typing();
        if let Some(typing) = self.typing.as_mut() {
            typing.buffer = TextBox::from(text);
        }
    }

    /// Inserts one typed character at the cursor.
    pub fn type_char(&mut self, typed: char) {
        if let Some(typing) = self.typing.as_mut() {
            typing.buffer.insert(typed);
        }
    }

    /// Removes the character before the cursor.
    pub fn type_backspace(&mut self) {
        if let Some(typing) = self.typing.as_mut() {
            typing.buffer.backspace();
        }
    }

    /// Moves the cursor or deletes, per `edit`.
    pub fn edit_text(&mut self, edit: TextEdit) {
        if let Some(typing) = self.typing.as_mut() {
            typing.buffer.edit(edit);
        }
    }

    /// Files the buffer as an edit, typed to the field's kind, or hands back the
    /// reason it did not. An empty buffer restores the default, through
    /// [`Self::default_for`]: `Enter` is an explicit apply.
    ///
    /// The check is [`crate::dog_settings::refusal`], which reads the grammar
    /// off the field rather than off the key. This is the only gate a dog's
    /// section has: `Request::SetDogConfig` checks only that it is TOML.
    #[must_use]
    pub fn apply_typing(&mut self) -> Option<Refusal> {
        let PaneTyping { key, buffer } = self.typing.take()?;
        if let Some(field) = self.new_entry_field(&key) {
            return self.apply_new_key(field, key, buffer);
        }
        let field = self.field_of(&key).cloned();
        if let Some(refused) = field
            .as_ref()
            .and_then(|field| validation::refusal(field, buffer.as_str()))
        {
            self.typing = Some(PaneTyping { key, buffer });
            return Some(refused);
        }
        if buffer.as_str().is_empty()
            && let Some(text) = self.required_refusal(&key)
        {
            self.typing = Some(PaneTyping { key, buffer });
            return Some(Refusal { text });
        }
        if buffer.as_str().is_empty() && self.place(&key).is_some() {
            self.restore_arm_field(&key);
            return None;
        }
        let kind = field.as_ref().map(|field| field.kind.clone());
        let value = match (kind, buffer.as_str()) {
            (_, "") => field
                .as_ref()
                .map_or(Value::Null, |field| self.default_for(field)),
            // Already parsed once by `validation::refusal`, which refused
            // every text an `i64` cannot hold, so this second parse is the
            // conversion rather than a second opinion. Its `Err` arm answers
            // a refusal because `None` is this function's word for filed.
            (Some(FieldKind::Integer), text) => match text.parse::<i64>() {
                Ok(number) => Value::from(number),
                Err(_) => {
                    let shown = field
                        .as_ref()
                        .map_or(text, |field| validation::shown_value(field.secret, text));
                    let refused = validation::not_a_whole_number(&key, shown);
                    self.typing = Some(PaneTyping { key, buffer });
                    return Some(refused);
                }
            },
            // The same second parse for a decimal. `validation::refusal`
            // already refused every text `f64::from_str` cannot hold and every
            // non-finite one it can, so this arm answers a refusal rather than
            // filing: `Value::from(f64)` hands back `null` for a non-finite float.
            (Some(FieldKind::Number), text) => match text.parse::<f64>() {
                Ok(number) if number.is_finite() => Value::from(number),
                _ => {
                    let shown = field
                        .as_ref()
                        .map_or(text, |field| validation::shown_value(field.secret, text));
                    let refused = validation::not_a_number(&key, shown);
                    self.typing = Some(PaneTyping { key, buffer });
                    return Some(refused);
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
