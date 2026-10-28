//! The list sub-screen: one row per element of an array field.
//!
//! An array is edited element by element and filed whole: every keystroke
//! that lands rebuilds the array and files one `PaneEdit::Set` over the
//! field, so the shepherd never sees a half-written list.
//!
//! An element is checked against the bounds its item schema published
//! ([`super::super::field::Bounds::items`]) before it is filed. A dog's
//! `Request::SetSheepDogSettings` replaces the whole table unread, so this
//! is the only gate that write has.

use serde_json::Value;

use super::super::edits::EditKey;
use super::super::field::{Bounds, Field, FieldKind, ListItem};
use super::super::textbox::{TextBox, TextEdit};
use super::super::viewport::Viewport;
use super::{ConfigPane, PaneEdit, PaneRow, PaneTarget};
use crate::dog_settings::{Refusal, item_refusal};

/// One row of the list sub-screen.
///
/// `Debug` is derived (IR-41): an index, or a marker for the row that adds
/// an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListRow {
    /// Index into [`ListPane::elements`].
    Item(usize),
    /// The `+ new` row.
    New,
}

/// The list sub-screen: one array field's elements, plus an editor.
///
/// Values are drawn, unlike an env row: an array arrives with the config,
/// so hiding an element would leave the cursor unable to say which one it
/// holds. `Debug` is manual and redacted (IR-41), exact-string-tested
/// below, for the same reason as [`ConfigPane`]'s. Elements are held as
/// text; [`ListItem`] turns the array back to JSON.
#[derive(Clone, PartialEq, Eq)]
pub struct ListPane {
    key: String,
    item: ListItem,
    /// What the item schema says an element must satisfy, which is what
    /// [`Self::apply_typing`] checks a buffer against. Empty for a list
    /// whose items are a plain string or a plain integer.
    item_bounds: Bounds,
    /// Whether the field is a secret, which decides what a refusal may
    /// print: the element, or `<set>`.
    secret: bool,
    elements: Vec<String>,
    view: Viewport,
    /// `Some((Some(index), buffer))` on an element, `Some((None, buffer))`
    /// on the `+ new` row.
    typing: Option<(Option<usize>, TextBox)>,
}

impl ListPane {
    /// A sub-screen over one array field, cursor at the top and nothing
    /// being typed.
    ///
    /// `item_bounds` are that field's [`Bounds::items`], and `secret` is
    /// its [`Field::secret`](super::super::field::Field::secret): both come
    /// off the field the sub-screen was opened on, and neither is a value.
    #[must_use]
    pub fn new(
        key: String,
        item: ListItem,
        item_bounds: Bounds,
        secret: bool,
        elements: Vec<String>,
    ) -> Self {
        Self {
            key,
            item,
            item_bounds,
            secret,
            elements,
            view: Viewport::new(),
            typing: None,
        }
    }

    /// The field this array belongs to, which is also the key a write
    /// carries.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// What the elements are, for the editor that parses one back.
    #[must_use]
    pub fn item(&self) -> &ListItem {
        &self.item
    }

    /// Whether the field is a secret: what masks an element's value on the
    /// screen, and what decides whether a refusal may print one.
    #[must_use]
    pub fn is_secret(&self) -> bool {
        self.secret
    }

    /// The elements, in the order the array holds them.
    #[must_use]
    pub fn elements(&self) -> &[String] {
        &self.elements
    }

    /// One row per element, then the `+ new` row.
    #[must_use]
    pub fn rows(&self) -> Vec<ListRow> {
        let mut rows: Vec<ListRow> = (0..self.elements.len()).map(ListRow::Item).collect();
        rows.push(ListRow::New);
        rows
    }

    /// The row under the cursor. Never [`None`]: [`Self::rows`] always ends
    /// with [`ListRow::New`], so there is always at least one row.
    #[must_use]
    pub fn cursor(&self) -> Option<ListRow> {
        self.rows().get(self.view.cursor()).copied()
    }

    /// The cursor and offset.
    #[must_use]
    pub fn view(&self) -> &Viewport {
        &self.view
    }

    /// What is being typed: which element it is for ([`None`] on the `+
    /// new` row) and the buffer. [`None`] while no editor is open.
    #[must_use]
    pub fn typing(&self) -> Option<(Option<usize>, &TextBox)> {
        self.typing.as_ref().map(|(index, buffer)| (*index, buffer))
    }

    /// Records the terminal's height, in rows of data.
    pub fn set_rows(&mut self, rows: usize) {
        let len = self.rows().len();
        self.view.set_rows(rows, len);
    }

    pub(in crate::lookout) fn move_by(&mut self, delta: isize) {
        let len = self.rows().len();
        self.view.move_by(delta, len);
    }

    pub(in crate::lookout) fn move_to(&mut self, index: usize) {
        let len = self.rows().len();
        self.view.move_to(index, len);
    }

    pub(in crate::lookout) fn move_to_first(&mut self) {
        self.move_to(0);
    }

    pub(in crate::lookout) fn move_to_last(&mut self) {
        let len = self.rows().len();
        self.move_to(len.saturating_sub(1));
    }

    /// Adopts a previous sub-screen's cursor and offset, clamped to this
    /// one's own row count.
    ///
    /// By index rather than by name, unlike [`ConfigPane::adopt_env_cursor`]:
    /// an element has no name, and its position is the only thing that
    /// identifies it. A cursor past the end lands on the `+ new` row,
    /// which is the one row where `Enter` destroys nothing.
    pub(in crate::lookout) fn adopt_view(&mut self, view: Viewport) {
        self.view = view;
        let len = self.rows().len();
        self.view.clamp(len);
    }

    /// Replaces the elements with the ones an edit has just filed, keeping
    /// the cursor where it is and clamping it to the new row count.
    ///
    /// By index, for [`Self::adopt_view`]'s own reason: an element has no
    /// name. A removal shortens the array under a cursor that stays put, so
    /// the cursor lands on whatever took the removed element's place, which
    /// is where an operator removing a run of elements wants it.
    pub(in crate::lookout) fn set_elements(&mut self, elements: Vec<String>) {
        self.elements = elements;
        let len = self.rows().len();
        self.view.clamp(len);
    }

    /// Opens the editor on the row under the cursor, seeded with the
    /// element it is on and empty on `+ new`.
    pub fn begin_typing(&mut self) {
        self.typing = match self.cursor() {
            Some(ListRow::Item(index)) => self
                .elements
                .get(index)
                .map(|element| (Some(index), TextBox::from(element.as_str()))),
            Some(ListRow::New) => Some((None, TextBox::new())),
            None => None,
        };
    }

    /// Inserts one typed character at the cursor.
    pub fn type_char(&mut self, typed: char) {
        if let Some((_, buffer)) = self.typing.as_mut() {
            buffer.insert(typed);
        }
    }

    /// Removes the character before the cursor.
    pub fn type_backspace(&mut self) {
        if let Some((_, buffer)) = self.typing.as_mut() {
            buffer.backspace();
        }
    }

    /// Moves the cursor or deletes, per `edit`.
    pub fn edit_text(&mut self, edit: TextEdit) {
        if let Some((_, buffer)) = self.typing.as_mut() {
            buffer.edit(edit);
        }
    }

    /// Drops the editor, leaving the sub-screen open.
    pub fn abandon_typing(&mut self) {
        self.typing = None;
    }

    /// Closes the editor and reads what it holds.
    ///
    /// `Ok(None)` three ways, and only one of them closes the editor: an
    /// empty buffer is nothing to write, and `d` is the key that removes an
    /// element. An element the item schema refuses keeps the editor open,
    /// the same rule [`ConfigPane::apply_typing`] follows.
    ///
    /// # Errors
    ///
    /// [`Refusal`] naming the rule the element failed, with the buffer left
    /// in the editor: an integer element that does not parse, or one the
    /// item schema's `minLength`, `pattern`, `enum`, `minimum` or `maximum`
    /// turns down. Nothing is filed for it.
    pub fn apply_typing(&mut self) -> Result<Option<String>, Refusal> {
        let Some(buffer) = self
            .typing
            .as_ref()
            .map(|(_, buffer)| buffer.as_str().to_owned())
        else {
            return Ok(None);
        };
        if buffer.is_empty() {
            self.typing = None;
            return Ok(None);
        }
        if let Some(refused) = item_refusal(
            &self.key,
            &self.item,
            &self.item_bounds,
            &buffer,
            self.secret,
        ) {
            return Err(refused);
        }
        self.typing = None;
        Ok(Some(buffer))
    }

    /// The elements with `text` written at the cursor, appended on the `+
    /// new` row. [`None`] when the cursor names no element.
    pub(in crate::lookout) fn with_element(&self, text: String) -> Option<Vec<String>> {
        let mut elements = self.elements.clone();
        match self.cursor()? {
            ListRow::Item(index) => *elements.get_mut(index)? = text,
            ListRow::New => elements.push(text),
        }
        Some(elements)
    }

    /// The elements without the one under the cursor. [`None`] on the `+
    /// new` row, which holds no element to remove.
    pub(in crate::lookout) fn without_element(&self) -> Option<Vec<String>> {
        let ListRow::Item(index) = self.cursor()? else {
            return None;
        };
        let mut elements = self.elements.clone();
        (index < elements.len()).then(|| {
            elements.remove(index);
            elements
        })
    }

    /// The elements with the one under the cursor moved `delta` places.
    /// [`None`] on the `+ new` row and at either end.
    pub(in crate::lookout) fn reordered(&self, delta: isize) -> Option<Vec<String>> {
        let ListRow::Item(index) = self.cursor()? else {
            return None;
        };
        let target = usize::try_from(isize::try_from(index).ok()? + delta).ok()?;
        if target >= self.elements.len() {
            return None;
        }
        let mut elements = self.elements.clone();
        elements.swap(index, target);
        Some(elements)
    }
}

/// A JSON value's elements as one string each, empty for anything that is
/// not an array. A non-scalar element renders as compact JSON, which is
/// what an editor would have to type back.
fn array_elements(value: &Value) -> Vec<String> {
    let Value::Array(values) = value else {
        return Vec::new();
    };
    values
        .iter()
        .map(|value| match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .collect()
}

/// The whole array as JSON, ready for `Request::SetSheepField`.
///
/// An integer element that does not parse travels as the string it is, so
/// the daemon refuses it by name instead of this guessing a number. Only
/// reachable for an element the config itself carried, since
/// [`ListPane::apply_typing`] refuses to arm one an operator typed.
fn list_value(item: &ListItem, elements: &[String]) -> Value {
    let element = |text: &String| match item {
        ListItem::Text | ListItem::Choice(_) => Value::String(text.clone()),
        ListItem::Integer => text
            .parse::<i64>()
            .map_or_else(|_| Value::String(text.clone()), Value::from),
    };
    Value::Array(elements.iter().map(element).collect())
}

impl core::fmt::Debug for ListPane {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "ListPane {{ key: {:?}, item: {:?}, secret: {}, elements: {}, typing: {} }}",
            self.key,
            self.item,
            self.secret,
            self.elements.len(),
            if self.typing.is_some() {
                "some"
            } else {
                "none"
            }
        )
    }
}

impl ConfigPane {
    /// The open list sub-screen, or [`None`] when the field list is what is
    /// on screen.
    #[must_use]
    pub fn list(&self) -> Option<&ListPane> {
        self.list.as_deref()
    }

    pub(in crate::lookout) fn list_mut(&mut self) -> Option<&mut ListPane> {
        self.list.as_deref_mut()
    }

    /// The items `field`'s list sub-screen edits, or [`None`] when the field
    /// is not a list it opens for. A list of choices the pane does not draw
    /// as a checklist, a flat pane's or a secret one, is typed as text.
    fn sub_screen_item(&self, field: &Field) -> Option<ListItem> {
        match &field.kind {
            FieldKind::List(item @ (ListItem::Text | ListItem::Integer)) => Some(item.clone()),
            FieldKind::List(ListItem::Choice(_)) if !self.drawn_as_checklist(field) => {
                Some(ListItem::Text)
            }
            _ => None,
        }
    }

    /// Opens the list sub-screen over the array field under the cursor.
    /// Does nothing on any other row.
    pub(in crate::lookout) fn open_list(&mut self) {
        let field = match self.cursor() {
            Some(PaneRow::Field(index)) => self.fields.fields().get(index),
            Some(row @ PaneRow::Arm { .. }) => self.arm_field(row),
            _ => None,
        };
        let Some(field) = field else {
            return;
        };
        let Some(item) = self.sub_screen_item(field) else {
            return;
        };
        let key = field.key.clone();
        let item_bounds = field.bounds.items.as_deref().cloned().unwrap_or_default();
        let secret = field.secret;
        let elements = self.filed_elements_of(&key);
        self.list = Some(Box::new(ListPane::new(
            key,
            item,
            item_bounds,
            secret,
            elements,
        )));
    }

    /// Closes it, leaving the field list up.
    pub(in crate::lookout) fn close_list(&mut self) {
        self.list = None;
    }

    /// `key`'s stored array as one string per element, empty when the
    /// field holds no array. What the shepherd last sent, which is only
    /// the right seed for a field nothing is filed for: see
    /// [`Self::filed_elements_of`].
    fn elements_of(&self, key: &str) -> Vec<String> {
        self.values
            .get(key)
            .or_else(|| self.arm_value(key))
            .map(array_elements)
            .unwrap_or_default()
    }

    /// `key`'s array as the operator has it: whatever is filed for the
    /// field, or the shepherd's own array when nothing is.
    ///
    /// The filed entry is the source of truth, not the stored value.
    /// Nothing writes until the pane closes, so seeding from the stored
    /// value would drop the operator's own work, and the next keystroke
    /// would rebuild from an array they have already changed.
    pub(super) fn filed_elements_of(&self, key: &str) -> Vec<String> {
        let filed = match self.edits.get(&EditKey::Field(key.to_owned())) {
            Some(entry) => match entry.edit() {
                PaneEdit::Set { value, .. } => Some(value.as_value()),
                PaneEdit::SetEnv { .. } => None,
            },
            None => None,
        };
        match filed {
            Some(value) => array_elements(value),
            None => self.elements_of(key),
        }
    }

    /// Files the whole array with `text` written at the sub-screen's
    /// cursor, appended when the cursor is on `+ new`.
    ///
    /// The whole array travels as one value: `Request::SetSheepField`
    /// carries one field, so an element is not a thing the wire can name.
    ///
    /// # Errors
    ///
    /// [`Refusal`] as [`Self::file_list`] has it.
    pub(in crate::lookout) fn file_list_element(&mut self, text: String) -> Result<(), Refusal> {
        let Some(elements) = self.list.as_ref().and_then(|list| list.with_element(text)) else {
            return Ok(());
        };
        self.file_list(elements)
    }

    /// Files the whole array without the element under the cursor.
    ///
    /// # Errors
    ///
    /// [`Refusal`] as [`Self::file_list`] has it.
    pub(in crate::lookout) fn file_list_removal(&mut self) -> Result<(), Refusal> {
        let Some(elements) = self.list.as_ref().and_then(|list| list.without_element()) else {
            return Ok(());
        };
        self.file_list(elements)
    }

    /// Files the whole array with the element under the cursor moved
    /// `delta` places. Does nothing at either end, where there is nowhere
    /// to move.
    ///
    /// # Errors
    ///
    /// [`Refusal`] as [`Self::file_list`] has it.
    pub(in crate::lookout) fn file_list_reorder(&mut self, delta: isize) -> Result<(), Refusal> {
        let Some(elements) = self.list.as_ref().and_then(|list| list.reordered(delta)) else {
            return Ok(());
        };
        self.file_list(elements)
    }

    /// Files `elements` as the field's whole value, then makes the open
    /// sub-screen show them.
    ///
    /// The second half stops a keystroke being lost: every filing door here
    /// rebuilds the array from the sub-screen's own elements, so one left
    /// showing the shepherd's array loses this keystroke.
    ///
    /// # Errors
    ///
    /// [`Refusal`] naming the first element of an integer list that is not
    /// an integer, on a dog's pane, with nothing filed. This pane writes a
    /// dog's section itself, so a refusal at the close would take every
    /// other filed edit with it.
    fn file_list(&mut self, elements: Vec<String>) -> Result<(), Refusal> {
        let Some(list) = self.list.as_ref() else {
            return Ok(());
        };
        if matches!(self.target(), PaneTarget::Dog { .. })
            && *list.item() == ListItem::Integer
            && let Some(odd) = elements.iter().find(|text| text.parse::<i64>().is_err())
        {
            let key = list.key();
            let shown = if list.is_secret() {
                "(hidden)"
            } else {
                odd.as_str()
            };
            return Err(Refusal {
                text: format!(
                    "{key} holds \"{shown}\", which is not an integer; remove it before changing {key}"
                ),
            });
        }
        let key = list.key().to_owned();
        let value = list_value(list.item(), &elements);
        self.file_field(key, value);
        if let Some(list) = self.list.as_mut() {
            list.set_elements(elements);
        }
        Ok(())
    }

    /// Re-opens the list sub-screen on the refreshed array, at the cursor
    /// and offset it had, so `r` does not slam the sub-screen shut on the
    /// operator.
    ///
    /// Seeded from [`Self::filed_elements_of`], not from the config that
    /// has just arrived: a refresh keeps the operator's filed set, and the
    /// sub-screen has to keep showing it.
    pub(in crate::lookout) fn adopt_list_view(&mut self, key: &str, view: Viewport) {
        let Some(field) = self.field_of(key) else {
            return;
        };
        let Some(item) = self.sub_screen_item(field) else {
            return;
        };
        let item_bounds = field.bounds.items.as_deref().cloned().unwrap_or_default();
        let secret = field.secret;
        let mut list = ListPane::new(
            key.to_owned(),
            item,
            item_bounds,
            secret,
            self.filed_elements_of(key),
        );
        list.adopt_view(view);
        self.list = Some(Box::new(list));
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{filed, web_with_args};
    use super::*;

    /// A dog's per-sheep table with one array field, `tags`, whose items
    /// carry `minLength` and a `pattern`: the shape a `Vec<Slug>` flattened
    /// one table down publishes, and the shape whose elements the pane
    /// writes whole at `tags`.
    fn tags_schema() -> Value {
        serde_json::json!({
            "$ref": "#/$defs/Settings",
            "$defs": {
                "Settings": {
                    "type": "object",
                    "properties": {
                        "tags": {
                            "type": "array",
                            "items": { "type": "string", "minLength": 3, "pattern": "^[a-z-]+$" },
                        },
                    },
                },
            },
        })
    }

    fn tags_pane() -> ConfigPane {
        ConfigPane::sheep_dog(
            "web".into(),
            "jobs".into(),
            &tags_schema(),
            serde_json::Map::new(),
        )
    }

    /// The array is one value, so an element is not a thing the wire can
    /// name.
    #[test]
    fn editing_one_element_sends_the_whole_array() {
        let mut pane = ConfigPane::sheep(web_with_args(&["--port", "8080"]));
        pane.move_to_key("args");
        pane.open_list();
        pane.list_mut().expect("open").move_to(1);
        pane.file_list_element("9090".into()).expect("filed");
        assert_eq!(
            filed(&pane, "args"),
            Some(serde_json::json!(["--port", "9090"]))
        );
    }

    /// Derived, unlike every other type in this file that touches a value.
    /// The screen renders its elements, so a `{:?}` that hid them would
    /// withhold what the operator is already reading. `ConfigPane`'s own
    /// `Debug` still names no element (`the_panes_debug_names_no_value_it_holds`).
    #[test]
    fn a_list_panes_debug_names_no_element() {
        let list = ListPane::new(
            "args".into(),
            ListItem::Text,
            Bounds::default(),
            false,
            vec!["--token".into(), "hunter2".into()],
        );
        assert_eq!(
            format!("{list:?}"),
            r#"ListPane { key: "args", item: Text, secret: false, elements: 2, typing: none }"#
        );
    }

    /// The operator is mid-word, not wrong, so the editor stays open and
    /// nothing is filed. The same rule `ConfigPane::apply_typing` follows
    /// for an integer field, and `stop_exit_codes` is the list the Flockfile
    /// schema puts no bound on its items at all.
    #[test]
    fn an_integer_element_that_does_not_parse_keeps_the_editor_open() {
        let mut pane = ConfigPane::sheep(web_with_args(&[]));
        pane.move_to_key("stop_exit_codes");
        pane.open_list();
        let list = pane.list_mut().expect("open");
        list.move_to_last();
        list.begin_typing();
        list.type_char('-');
        let refused = list.apply_typing().expect_err("- is not a whole number");
        assert_eq!(
            refused.text,
            "stop_exit_codes is \"-\", which is not a whole number shep accepts"
        );
        assert!(list.typing().is_some(), "the editor is still open");
        list.type_char('1');
        assert_eq!(
            list.apply_typing().ok().flatten().as_deref(),
            Some("-1"),
            "and a buffer that parses is still accepted"
        );
    }

    /// `minLength` on an item is the rule a dog writes for "not blank, and
    /// not two characters either". The refused element stays in the editor
    /// and files nothing, exactly as a scalar field's own refusal does.
    #[test]
    fn a_list_element_under_its_item_min_length_files_nothing() {
        let mut pane = tags_pane();
        pane.move_to_key("tags");
        pane.open_list();
        let refused = {
            let list = pane.list_mut().expect("the sub-screen opens");
            list.move_to_last();
            list.begin_typing();
            for typed in "ab".chars() {
                list.type_char(typed);
            }
            list.apply_typing()
                .expect_err("two characters is under three")
        };
        assert_eq!(
            refused.text,
            "tags is \"ab\"; tags needs at least 3 characters"
        );
        assert!(
            pane.list().expect("still open").typing().is_some(),
            "the editor stays open on what was typed"
        );
        assert!(pane.edits().is_empty(), "nothing was filed");

        let text = {
            let list = pane.list_mut().expect("still open");
            list.type_char('c');
            list.apply_typing()
                .ok()
                .flatten()
                .expect("three characters is the bound itself")
        };
        pane.file_list_element(text).expect("filed");
        assert_eq!(filed(&pane, "tags"), Some(serde_json::json!(["abc"])));
    }

    /// A `pattern` an item carries is the dog's own grammar, enforced as the
    /// dog wrote it, and `Request::SetSheepDogSettings` has no gate behind
    /// this one: it replaces the table without reading it.
    #[test]
    fn a_list_element_that_fails_its_item_pattern_files_nothing() {
        let mut pane = tags_pane();
        pane.move_to_key("tags");
        pane.open_list();
        let refused = {
            let list = pane.list_mut().expect("the sub-screen opens");
            list.move_to_last();
            list.begin_typing();
            for typed in "Ab1".chars() {
                list.type_char(typed);
            }
            list.apply_typing()
                .expect_err("uppercase is outside the pattern")
        };
        assert_eq!(
            refused.text,
            "tags is \"Ab1\", which it does not take; try it matches ^[a-z-]+$"
        );
        assert!(pane.edits().is_empty(), "nothing reached the table");

        let text = {
            let list = pane.list_mut().expect("still open");
            for _ in 0..3 {
                list.type_backspace();
            }
            for typed in "ops-team".chars() {
                list.type_char(typed);
            }
            list.apply_typing()
                .ok()
                .flatten()
                .expect("the pattern takes it")
        };
        pane.file_list_element(text).expect("filed");
        assert_eq!(filed(&pane, "tags"), Some(serde_json::json!(["ops-team"])));
    }

    /// A list the schema marks secret is a `Vec` of tokens, and the editor
    /// draws what is typed: the refusal is the one place that value would be
    /// repeated, so it names the marker the row already draws (IR-41).
    #[test]
    fn a_secret_lists_refusal_prints_the_marker_and_not_the_element() {
        let mut list = ListPane::new(
            "tokens".into(),
            ListItem::Text,
            Bounds {
                pattern: Some("^sk-".to_owned()),
                ..Default::default()
            },
            true,
            Vec::new(),
        );
        list.begin_typing();
        for typed in "tok_HUNTER2".chars() {
            list.type_char(typed);
        }
        let refused = list.apply_typing().expect_err("the pattern refuses it");
        assert_eq!(
            refused.text,
            "tokens is \"<set>\", which it does not take; try it matches ^sk-"
        );
        assert!(list.typing().is_some(), "the editor stays open");
    }

    /// The control on the test above: an element of a list that is not a
    /// credential is quoted back in the sentence.
    #[test]
    fn an_ordinary_list_refusal_quotes_the_element() {
        let mut list = ListPane::new(
            "regions".into(),
            ListItem::Text,
            Bounds {
                pattern: Some("^eu-".to_owned()),
                ..Default::default()
            },
            false,
            Vec::new(),
        );
        list.begin_typing();
        for typed in "Europe".chars() {
            list.type_char(typed);
        }
        let refused = list.apply_typing().expect_err("Europe does not match");
        assert!(refused.text.contains("Europe"), "{}", refused.text);
    }

    /// The whole array goes out, so a removal and a reorder are the same
    /// kind of write an element edit is, and the reorder acts on what the
    /// removal left rather than on the array the shepherd sent.
    #[test]
    fn removing_and_reordering_file_the_whole_array_too() {
        let mut pane = ConfigPane::sheep(web_with_args(&["a", "b", "c"]));
        pane.move_to_key("args");
        pane.open_list();
        pane.list_mut().expect("open").move_to(1);
        pane.file_list_removal().expect("filed");
        assert_eq!(filed(&pane, "args"), Some(serde_json::json!(["a", "c"])));

        pane.file_list_reorder(-1).expect("filed");
        assert_eq!(
            filed(&pane, "args"),
            Some(serde_json::json!(["c", "a"])),
            "the cursor is on `c` now, and moving it up files one entry, not two"
        );
        assert_eq!(pane.edits().len(), 1, "one field, one entry");
    }

    /// Two keystrokes in one sub-screen compose. The set holds one entry
    /// per field, so the second action has to build on the array the first
    /// one filed; recomputing from what the shepherd sent would throw the
    /// first keystroke away.
    #[test]
    fn a_second_list_keystroke_builds_on_the_first() {
        let mut pane = ConfigPane::sheep(web_with_args(&[]));
        pane.move_to_key("args");
        pane.open_list();
        pane.list_mut().expect("open").move_to_last();
        pane.file_list_element("abc".into()).expect("filed");
        pane.list_mut().expect("open").move_to_last();
        pane.file_list_element("def".into()).expect("filed");
        assert_eq!(
            filed(&pane, "args"),
            Some(serde_json::json!(["abc", "def"]))
        );
        assert_eq!(pane.edits().len(), 1, "one field, one entry");
    }

    /// The sub-screen draws what is filed, not what the shepherd last
    /// sent: nothing writes until the pane closes, so an operator who
    /// leaves the array and comes back has to find their own work in it.
    #[test]
    fn re_opening_the_sub_screen_shows_what_was_filed() {
        let mut pane = ConfigPane::sheep(web_with_args(&["a", "b"]));
        pane.move_to_key("args");
        pane.open_list();
        pane.list_mut().expect("open").move_to(0);
        pane.file_list_removal().expect("filed");
        assert_eq!(pane.list().expect("open").elements(), ["b"]);
        pane.close_list();
        pane.open_list();
        assert_eq!(pane.list().expect("re-opened").elements(), ["b"]);
    }

    /// `J`'s direction. Only `-1` is exercised above, and the two share
    /// one arm in `file_list_reorder`.
    #[test]
    fn moving_an_element_down_files_the_whole_array() {
        let mut pane = ConfigPane::sheep(web_with_args(&["a", "b", "c"]));
        pane.move_to_key("args");
        pane.open_list();
        pane.list_mut().expect("open").move_to(0);
        pane.file_list_reorder(1).expect("filed");
        assert_eq!(
            filed(&pane, "args"),
            Some(serde_json::json!(["b", "a", "c"]))
        );
    }

    /// The `+ new` row holds no element, and neither end has anywhere to
    /// move to, so neither keystroke files anything at all.
    #[test]
    fn a_removal_or_a_move_with_nothing_to_act_on_files_nothing() {
        let mut pane = ConfigPane::sheep(web_with_args(&["a", "b"]));
        pane.move_to_key("args");
        pane.open_list();
        pane.list_mut().expect("open").move_to_last();
        pane.file_list_removal().expect("filed");
        assert!(pane.edits().is_empty(), "the `+ new` row holds no element");
        pane.list_mut().expect("open").move_to_first();
        pane.file_list_reorder(-1).expect("filed");
        assert!(pane.edits().is_empty(), "the first element cannot move up");
    }

    /// An integer array's elements render as digits and travel back as
    /// numbers, which is what tells `stop_exit_codes` apart from `args`.
    #[test]
    fn an_integer_array_travels_as_numbers() {
        let mut pane = ConfigPane::sheep(web_with_args(&[]));
        pane.move_to_key("stop_exit_codes");
        pane.open_list();
        assert_eq!(pane.list().expect("open").elements(), ["0", "143"]);
        pane.list_mut().expect("open").move_to(0);
        pane.file_list_element("2".into()).expect("filed");
        assert_eq!(
            filed(&pane, "stop_exit_codes"),
            Some(serde_json::json!([2, 143]))
        );
    }
}
