//! The list sub-screen: one row per element of an array field.
//!
//! An array is edited element by element and filed whole. Every keystroke
//! that lands rebuilds the array and files one `PaneEdit::Set` over the
//! field, so the shepherd never sees a half-written list and the pane never
//! has to reconcile two representations of one field.

use serde_json::Value;

use super::super::edits::EditKey;
use super::super::field::{FieldKind, ListItem};
use super::super::viewport::Viewport;
use super::{ConfigPane, PaneEdit, PaneRow};

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

/// The list sub-screen: one array field's elements, and an editor over
/// them.
///
/// Values are drawn, unlike an env row: an array arrives with the
/// config, so hiding an element would leave the cursor unable to say
/// which one it holds. `Debug` is manual and redacted (IR-41),
/// exact-string-tested below, for the same reason as [`ConfigPane`]'s:
/// `args` can carry a token an operator typed. Elements are held as
/// text; [`ListItem`] turns the array back to JSON on write.
#[derive(Clone, PartialEq, Eq)]
pub struct ListPane {
    key: String,
    item: ListItem,
    elements: Vec<String>,
    view: Viewport,
    /// `Some((Some(index), buffer))` on an element, `Some((None, buffer))`
    /// on the `+ new` row.
    typing: Option<(Option<usize>, String)>,
}

impl ListPane {
    /// A sub-screen over one array field, cursor at the top and nothing
    /// being typed.
    #[must_use]
    pub fn new(key: String, item: ListItem, elements: Vec<String>) -> Self {
        Self {
            key,
            item,
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
    pub fn item(&self) -> ListItem {
        self.item
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
    pub fn typing(&self) -> Option<(Option<usize>, &str)> {
        self.typing
            .as_ref()
            .map(|(index, buffer)| (*index, buffer.as_str()))
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
                .map(|element| (Some(index), element.clone())),
            Some(ListRow::New) => Some((None, String::new())),
            None => None,
        };
    }

    /// Appends one typed character.
    pub fn type_char(&mut self, typed: char) {
        if let Some((_, buffer)) = self.typing.as_mut() {
            buffer.push(typed);
        }
    }

    /// Removes the last typed character.
    pub fn type_backspace(&mut self) {
        if let Some((_, buffer)) = self.typing.as_mut() {
            buffer.pop();
        }
    }

    /// Drops the editor, leaving the sub-screen open.
    pub fn abandon_typing(&mut self) {
        self.typing = None;
    }

    /// Closes the editor and reads what it holds.
    ///
    /// [`None`] three ways, and only one of them closes the editor: an
    /// empty buffer is nothing to write and leaves the array alone, since
    /// `d` is the key that removes an element. An integer element whose
    /// buffer does not parse keeps the editor open, the same rule
    /// [`ConfigPane::apply_typing`] follows, because the operator is
    /// mid-word rather than wrong.
    pub fn apply_typing(&mut self) -> Option<String> {
        let (_, buffer) = self.typing.as_ref()?;
        if self.item == ListItem::Integer && buffer.parse::<i64>().is_err() && !buffer.is_empty() {
            return None;
        }
        let (_, buffer) = self.typing.take()?;
        (!buffer.is_empty()).then_some(buffer)
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
fn list_value(item: ListItem, elements: &[String]) -> Value {
    let element = |text: &String| match item {
        ListItem::Text => Value::String(text.clone()),
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
            "ListPane {{ key: {:?}, item: {:?}, elements: {}, typing: {} }}",
            self.key,
            self.item,
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
        self.list.as_ref()
    }

    pub(in crate::lookout) fn list_mut(&mut self) -> Option<&mut ListPane> {
        self.list.as_mut()
    }

    /// Opens the list sub-screen over the array field under the cursor.
    /// Does nothing on any other row.
    pub(in crate::lookout) fn open_list(&mut self) {
        let Some(PaneRow::Field(index)) = self.cursor() else {
            return;
        };
        let Some(field) = self.fields.fields().get(index) else {
            return;
        };
        let FieldKind::List(item) = field.kind else {
            return;
        };
        let key = field.key.clone();
        let elements = self.filed_elements_of(&key);
        self.list = Some(ListPane::new(key, item, elements));
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
        self.values.get(key).map(array_elements).unwrap_or_default()
    }

    /// `key`'s array as the operator has it: the elements of whatever is
    /// filed for the field, and the shepherd's own array when nothing is.
    ///
    /// The filed entry is the source of truth, not the stored value, and
    /// this is what makes a sub-screen compose. Nothing writes until the
    /// pane closes, so the stored value stays as the shepherd sent it for
    /// as long as the operator is editing: seeding from it would show an
    /// operator who leaves the array and comes back none of their own
    /// work, and would recompute the next keystroke from an array they
    /// have already changed.
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
    pub(in crate::lookout) fn file_list_element(&mut self, text: String) {
        let Some(elements) = self.list.as_ref().and_then(|list| list.with_element(text)) else {
            return;
        };
        self.file_list(elements);
    }

    /// Files the whole array without the element under the cursor.
    pub(in crate::lookout) fn file_list_removal(&mut self) {
        let Some(elements) = self.list.as_ref().and_then(ListPane::without_element) else {
            return;
        };
        self.file_list(elements);
    }

    /// Files the whole array with the element under the cursor moved
    /// `delta` places. Does nothing at either end, where there is nowhere
    /// to move.
    pub(in crate::lookout) fn file_list_reorder(&mut self, delta: isize) {
        let Some(elements) = self.list.as_ref().and_then(|list| list.reordered(delta)) else {
            return;
        };
        self.file_list(elements);
    }

    /// Files `elements` as the field's whole value, then makes the open
    /// sub-screen show them.
    ///
    /// The second half is what stops a keystroke being lost. Every filing
    /// door here rebuilds the whole array from the sub-screen's own
    /// elements, so a sub-screen left showing the shepherd's array would
    /// recompute the next keystroke from it and file an array missing this
    /// one. The write that used to refresh the screen is gone: the pane
    /// files and writes once, when it closes.
    fn file_list(&mut self, elements: Vec<String>) {
        let Some(list) = self.list.as_ref() else {
            return;
        };
        let key = list.key().to_owned();
        let value = list_value(list.item(), &elements);
        self.file_field(key, value);
        if let Some(list) = self.list.as_mut() {
            list.set_elements(elements);
        }
    }

    /// Re-opens the list sub-screen on the refreshed array, at the cursor
    /// and offset it had, so `r` does not slam the sub-screen shut on the
    /// operator.
    ///
    /// Seeded from [`Self::filed_elements_of`], not from the config that
    /// has just arrived: a refresh replaces the shepherd's values and keeps
    /// the operator's filed set, and the sub-screen has to keep showing the
    /// set.
    pub(in crate::lookout) fn adopt_list_view(&mut self, key: &str, view: Viewport) {
        let Some(item) = self.fields.by_key(key).and_then(|field| match field.kind {
            FieldKind::List(item) => Some(item),
            _ => None,
        }) else {
            return;
        };
        let mut list = ListPane::new(key.to_owned(), item, self.filed_elements_of(key));
        list.adopt_view(view);
        self.list = Some(list);
    }
}

#[cfg(test)]
mod tests {
    use super::super::Lock;
    use super::super::fixtures::{filed, web_with_args};
    use super::*;

    /// The array is one value, so an element is not a thing the wire can
    /// name.
    #[test]
    fn editing_one_element_sends_the_whole_array() {
        let mut pane = ConfigPane::sheep(web_with_args(&["--port", "8080"]));
        pane.move_to_key("args");
        pane.open_list();
        pane.list_mut().expect("open").move_to(1);
        pane.file_list_element("9090".into());
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
            vec!["--token".into(), "hunter2".into()],
        );
        assert_eq!(
            format!("{list:?}"),
            r#"ListPane { key: "args", item: Text, elements: 2, typing: none }"#
        );
    }

    /// The operator is mid-word, not wrong, so the editor stays open and
    /// nothing is filed. The same rule `ConfigPane::apply_typing` follows
    /// for an integer field.
    #[test]
    fn an_integer_element_that_does_not_parse_keeps_the_editor_open() {
        let mut pane = ConfigPane::sheep(web_with_args(&[]));
        pane.move_to_key("stop_exit_codes");
        pane.open_list();
        let list = pane.list_mut().expect("open");
        list.move_to_last();
        list.begin_typing();
        list.type_char('-');
        assert_eq!(list.apply_typing(), None);
        assert!(list.typing().is_some(), "the editor is still open");
        list.type_char('1');
        assert_eq!(list.apply_typing().as_deref(), Some("-1"));
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
        pane.file_list_removal();
        assert_eq!(filed(&pane, "args"), Some(serde_json::json!(["a", "c"])));

        pane.file_list_reorder(-1);
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
        pane.file_list_element("abc".into());
        pane.list_mut().expect("open").move_to_last();
        pane.file_list_element("def".into());
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
        pane.file_list_removal();
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
        pane.file_list_reorder(1);
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
        pane.file_list_removal();
        assert!(pane.edits().is_empty(), "the `+ new` row holds no element");
        pane.list_mut().expect("open").move_to_first();
        pane.file_list_reorder(-1);
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
        pane.file_list_element("2".into());
        assert_eq!(
            filed(&pane, "stop_exit_codes"),
            Some(serde_json::json!([2, 143]))
        );
    }

    /// A dog's write replaces its whole section, and `edited_section_with`
    /// has no rendering for an array, so the pane offers no editor for one.
    #[test]
    fn a_dogs_array_field_has_no_widget_here() {
        let schema = serde_json::json!({
            "properties": { "sinks": { "type": "array", "items": { "type": "string" } } }
        });
        let pane = ConfigPane::dog("bark".into(), None, schema, String::new());
        assert!(!pane.fields().by_key("sinks").expect("declared").editable);
        assert_eq!(pane.lock("sinks"), Some(Lock::NoWidget));
    }
}
