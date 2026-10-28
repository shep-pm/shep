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
/// A struct rather than the three-variant enum this was. Nothing arms and
/// nothing is in flight any more: a keystroke files straight into
/// [`Edits`], and the whole set leaves when the pane closes. Typing is the
/// one state left, so an enum named for a lifecycle would name two states
/// that no longer exist.
///
/// A field's key always exists before its edit starts, which is what keeps
/// this a bare `key: String`: [`KeyValues`] has to be able to hold a key that
/// does not exist yet, so it carries both halves as boxes.
///
/// `Debug` is manual and redacted (IR-41), exact-string-tested below. The
/// buffer is what the operator is halfway through typing.
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
        self.fold_entry_edits();
        let edits = core::mem::take(&mut self.edits);
        self.reshapes = Default::default();
        self.resync_entries();
        edits
    }

    /// Drops the most recently filed edit and names it, for `u`.
    ///
    /// An open list sub-screen is re-read from what is filed afterwards, so
    /// `u` inside one shows the array it just restored rather than the one
    /// it has undone. One entry is one field, so a `u` there takes the
    /// whole field back to the shepherd's array rather than one keystroke
    /// of it. Undoing the removal of an entry gives its rows back, with the
    /// edits filed under the array and the entries that were open.
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
    /// both as `(unset)`, so unsetting a field that is already unset is
    /// not a change.
    ///
    /// For an array or map of tables an empty one is the same fact too: an
    /// entry added and removed again leaves the section as it was.
    fn stored_value_is(&self, key: &str, value: &Value) -> bool {
        let stored = self.stored_of(key).unwrap_or(&Value::Null);
        stored == value
            || (self.entries_field(key).is_some() && holds_nothing(stored) && holds_nothing(value))
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
    /// - A dog's table on a sheep is the same case: `null` removes the leaf
    ///   from the table, and the dog applies its own default when it reads
    ///   the table back.
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
    /// can. A dog's `values` is the raw section text with no defaults
    /// merged in, so an absent or `null` key already means default there,
    /// which is exactly what [`Self::stored_value_is`] checks. A dog's table
    /// on a sheep is read the same raw way.
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
    /// [`Self::begin_typing`]'s seed is what the row shows, which is the right
    /// seed for a character-at-a-time editor and the wrong one for a caller
    /// that already holds the whole text: the settings screen keeps its own
    /// text box for every row, dog's own field included, and hands the finished
    /// buffer here rather than retyping it. Validation and the typed-to-kind
    /// conversion stay in [`Self::apply_typing`], so this is the same gate a
    /// keystroke in a pane goes through.
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

    /// Files the buffer as an edit, typed to the field's kind, or hands
    /// back the reason it did not.
    ///
    /// The editor `Enter` on a map's `+ new` row opens is not for a field: it
    /// takes the key of the entry to add, see [`Self::apply_new_key`].
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
    /// The check is [`crate::dog_settings::refusal`], which reads the
    /// grammar off the field rather than off the key: shep's own two string
    /// types go through their `FromStr`, a dog's own `pattern` through the
    /// regex it published, an integer through its schema's `minimum`,
    /// `maximum`, `exclusiveMinimum` and `exclusiveMaximum` (the two strict
    /// spellings folded into the pair at the next integer in), and a decimal
    /// through the same four as the schema wrote them.
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
            // a refusal because `None` is this function's word for filed:
            // returning it here would restore the editor, file nothing, and
            // say nothing, which is the shape the gate above exists to stop.
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
            // The same second parse for a decimal. `validation::refusal` already
            // refused every text `f64::from_str` cannot hold and every
            // non-finite one it can, so this arm answers a refusal rather than
            // filing: `Value::from(f64)` hands back `null` for a non-finite
            // float, and a dog's section writer reads `null` as "remove this
            // key".
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
