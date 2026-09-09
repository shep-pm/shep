//! An open config pane: what it is editing, its fields, and its cursor.
//!
//! The pane is a [`FieldSet`] over one target, plus the values that target
//! currently holds and a [`Viewport`] over the rows. It writes too, and it
//! writes once: every keystroke files a [`PaneEdit`] into [`Edits`], and
//! the whole set leaves together when the pane closes, each entry as a
//! `Request::SetSheepField` or, for `env`, a `Request::SetSheepEnv`. Both
//! write an operator override for one key; neither pretends to be a
//! template. See [`PaneEdit`] for why not `Request::ApplyConfig`.

use std::path::PathBuf;

use serde_json::{Map, Value};
use shep_core::config::{ApplyGroup, GROUP_ORDER, apply_group, flockfile_schema_json};
use shep_core::protocol::{EnvValue, SheepConfigView};
use shep_core::values::{MemSize, UpDuration};

use super::edits::{EditKey, Edits};
use super::field::{FieldKind, FieldSet, ListItem, ValueKind};
use super::viewport::Viewport;

/// Which thing the pane is editing.
///
/// Two things, and they are not the same shape of edit. A sheep's config is
/// shep's own document, so shep knows what every field costs; a dog's
/// section belongs to the dog, so shep publishes the change and the dog
/// decides what to reload, which is what [`ConfigPane::cost`]'s [`Option`]
/// is for.
///
/// `Debug` is derived (IR-41): a name and a binary's path, neither of which
/// is a value the pane withholds. A dog's section can carry a credential and
/// is held on [`ConfigPane`] instead, behind that type's own redacted
/// `Debug`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneTarget {
    /// One sheep, by name.
    Sheep {
        /// The sheep.
        name: String,
    },
    /// One dog, by name, with the binary its schema was probed from.
    Dog {
        /// The dog.
        name: String,
        /// The adopted binary, or [`None`] for a built-in, whose schema
        /// comes from `crate::dog::builtin_schema`, since a built-in dog is
        /// this same binary. Kept so a re-probe asks the same path the pane
        /// opened on.
        adopted_path: Option<PathBuf>,
    },
}

impl PaneTarget {
    /// The target's name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Sheep { name } | Self::Dog { name, .. } => name,
        }
    }
}

/// Why a row cannot be edited from the pane.
///
/// Two different facts, and an operator has to be able to tell them apart:
/// one says the field is beyond editing anywhere, the other says only that
/// this screen has no widget for its shape and a Flockfile still can.
/// Collapsing them into `Field::editable` alone is what made six rows claim
/// the wrong one.
///
/// `Debug` is derived (IR-41): a bare variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    /// shep itself refuses a config write. Identity or flock shape rather
    /// than a runtime knob, so no surface changes it: `name` and
    /// `instances`, whose count moves through `shep stock` instead.
    Refused,
    /// The pane has no widget for this shape, and nothing more than that.
    /// `shep start <Flockfile>` writes these perfectly well, and
    /// [`ConfigPane::cost`] still reports what doing so would cost.
    NoWidget,
}

/// One row of the pane.
///
/// Three variants, not one: the env sub-screen is gone, and its keys now
/// walk the same cursor as every field, so this enum has to name a row in
/// either territory. Named rather than left as a bare index anyway, a
/// `usize` travelling between [`ConfigPane::rows`], the viewport and the
/// renderer says nothing about what it indexes, and this one says.
///
/// `Debug` is derived (IR-41): an index, or a bare variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneRow {
    /// Index into [`ConfigPane::fields`].
    Field(usize),
    /// Index into [`ConfigPane::env_key_names`].
    Env(usize),
    /// The row that adds a new env key.
    AddEnv,
}

/// One config field's new value, on its way out of the pane.
///
/// A newtype for the reason [`EnvValue`] is one: `cwd` and `script`
/// routinely hold a home directory and `args` holds a token, so
/// [`ConfigPane`]'s own `Debug` already withholds the map these come out
/// of. A bare [`Value`] travels on into `Sent::ApplyField`, which derives
/// `Debug`, so the newtype rides with the value wherever it goes rather
/// than depending on every type along the way to redact it separately.
///
/// The wire field itself (`Request::SetSheepField`) stays a bare
/// [`Value`] deliberately: `env` is the one field `AppConfig`'s own
/// `Debug` redacts, and `cwd` prints in the clear on every request that
/// carries a whole config, so a newtype there would protect one copy of
/// a value the protocol prints three other ways. This one guards
/// lookout, where a live value must never print.
///
/// `Debug` is manual and redacted (IR-41), exact-string-tested below. It
/// names the JSON type and nothing else, which is what a diagnostic
/// needs and is not a value.
#[derive(Clone, PartialEq, Eq)]
pub struct FieldValue(Value);

impl FieldValue {
    /// The value, for the request that carries it.
    #[must_use]
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    /// The value, printed, when printing it cannot leak anything: a bool or
    /// a number can never hold a secret, a token or a home directory
    /// (IR-41). Every other kind stays [`None`], for the same reason
    /// [`Debug`](core::fmt::Debug) above never prints one either.
    ///
    /// Exists so a notice that reports a field being set can say which way
    /// it moved (`reuse_port set to true` versus `false`) without touching
    /// the kinds this type exists to guard.
    #[must_use]
    pub fn safe_summary(&self) -> Option<String> {
        match &self.0 {
            Value::Bool(value) => Some(value.to_string()),
            Value::Number(value) => Some(value.to_string()),
            Value::Null | Value::String(_) | Value::Array(_) | Value::Object(_) => None,
        }
    }
}

impl From<Value> for FieldValue {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

/// Prints the JSON type and never the value. See the type doc for why.
/// Exact-string-tested below (`a_field_values_debug_names_no_value`) so a
/// future `#[derive(Debug)]` fails that test instead of silently reopening
/// the leak.
impl core::fmt::Debug for FieldValue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let kind = match &self.0 {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        write!(f, "FieldValue(<{kind}>)")
    }
}

/// One edit, ready to send.
///
/// Two variants, and they leave by their own doors: a [`Self::Set`] as a
/// `Request::SetSheepField` and a [`Self::SetEnv`] as a
/// `Request::SetSheepEnv`. Both record an operator override for one key,
/// never a template merge: a one-app `Request::ApplyConfig` at
/// `ResetDepth::File` would treat the edit as a template load, so it would
/// vanish from `overridden` the moment it landed and the pane's `*` marker
/// would never appear for it.
///
/// `Debug` is derived, safe because both value types redact themselves:
/// [`FieldValue`] names a JSON type and [`EnvValue`] names a byte count.
/// One mechanism on the value beats two on the types that carry it; see
/// [`FieldValue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEdit {
    /// Set the config field `key` to `value`.
    Set {
        /// The field.
        key: String,
        /// The new value, already typed to the field's kind.
        value: FieldValue,
    },
    /// Set the env key `key`, or with [`None`] remove it.
    SetEnv {
        /// The env key.
        key: String,
        /// The value, or [`None`] to remove the key.
        value: Option<EnvValue>,
    },
}

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
    /// Which field. Owns [`super::app::InputMode::Text`] for as long as
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

    pub(super) fn move_by(&mut self, delta: isize) {
        let len = self.rows().len();
        self.view.move_by(delta, len);
    }

    pub(super) fn move_to(&mut self, index: usize) {
        let len = self.rows().len();
        self.view.move_to(index, len);
    }

    pub(super) fn move_to_first(&mut self) {
        self.move_to(0);
    }

    pub(super) fn move_to_last(&mut self) {
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
    pub(super) fn adopt_view(&mut self, view: Viewport) {
        self.view = view;
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
    pub(super) fn with_element(&self, text: String) -> Option<Vec<String>> {
        let mut elements = self.elements.clone();
        match self.cursor()? {
            ListRow::Item(index) => *elements.get_mut(index)? = text,
            ListRow::New => elements.push(text),
        }
        Some(elements)
    }

    /// The elements without the one under the cursor. [`None`] on the `+
    /// new` row, which holds no element to remove.
    pub(super) fn without_element(&self) -> Option<Vec<String>> {
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
    pub(super) fn reordered(&self, delta: isize) -> Option<Vec<String>> {
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

/// A JSON value rendered the way a config row draws it: a scalar shows
/// bare, `null` shows `(unset)`, anything else shows compact JSON.
///
/// Shared by [`ConfigPane::value`], reading the stored value, and
/// [`ConfigPane::edited_value`], reading a filed one, so the two sides of
/// an `old -> new` cell are rendered by one rule rather than two that can
/// drift.
fn render_json(value: &Value) -> String {
    match value {
        Value::Null => "(unset)".to_owned(),
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        other => other.to_string(),
    }
}

/// The state of an open pane.
///
/// `Debug` is manual and redacted (IR-41): `values` is a sheep's config with
/// `env` already stripped by [`SheepConfigView::new`], but `args` and `cwd`
/// are still in it and routinely carry a token or a home directory.
/// `env_keys` is a key set, which is itself worth keeping out of a log, and
/// the same reasoning [`SheepConfigView`]'s own `Debug` gives applies here
/// unchanged: this type is a copy of that one's payload.
#[derive(Clone)]
pub struct ConfigPane {
    target: PaneTarget,
    fields: FieldSet,
    values: Map<String, Value>,
    env_keys: Vec<String>,
    overridden: Vec<String>,
    /// Field names parked until the next respawn, as the shepherd reported
    /// them. Nothing to do with [`Self::edits`], which is this pane's own
    /// set of unwritten changes; the two words come from opposite ends and
    /// the collision is the shepherd's.
    pending: Vec<String>,
    /// Index into [`GROUP_ORDER`]: which group's fields the field list
    /// draws. Meaningless for a dog pane, whose fields carry no group and
    /// so are visible under any of them; see [`Self::rows`].
    group: usize,
    view: Viewport,
    /// The open text editor, or [`None`].
    typing: Option<PaneTyping>,
    /// Everything the operator has changed and nothing has written yet.
    /// Emptied by [`Self::close`], which is the only door out.
    edits: Edits,
    /// The open env editor, or [`None`]. Never open at the same time as
    /// [`Self::list`]: each opens on a row of its own kind, and `Escape`
    /// closes whichever is up before the pane.
    env_typing: Option<EnvTyping>,
    /// The open list sub-screen. Never open at the same time as
    /// [`Self::env_typing`]: each opens on a row of its own kind, and
    /// `Escape` closes whichever is up before the pane.
    list: Option<ListPane>,
    /// Whether `h` is showing the selected field's own help text.
    help_open: bool,
    /// The dog's `[<name>]` table as TOML text, and [`None`] for a sheep.
    ///
    /// Kept beside the parsed `values` rather than instead of them, because
    /// the two answer different questions: `values` is what the rows render,
    /// and this is what a write edits. `Request::SetDogConfig` replaces the
    /// whole section, so an edit that re-rendered it from `values` would
    /// throw away every comment the operator wrote. See
    /// [`Self::edited_section_all`].
    section: Option<String>,
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

impl core::fmt::Debug for ConfigPane {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "ConfigPane {{ target: {:?}, fields: {}, env_keys: {}, cursor: {} }}",
            self.target,
            self.fields.len(),
            self.env_keys.len(),
            self.view.cursor()
        )
    }
}

impl ConfigPane {
    /// A pane over one sheep's config, read off the Flockfile schema.
    ///
    /// The schema is the field list: it already carries every property's
    /// type, default and group, so the pane reads the same document `shep
    /// init` scaffolds from rather than keeping a second list of 40 names
    /// in step with it.
    #[must_use]
    pub fn sheep(view: SheepConfigView) -> Self {
        let schema = flockfile_schema_json().to_value();
        let defs = schema
            .get("$defs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let properties = defs
            .get("AppConfig")
            .and_then(|app| app.get("properties"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let set = FieldSet::from_properties(&properties, &defs, GROUP_ORDER);
        // A Structural field is identity or flock shape, not a runtime knob:
        // `name` cannot drift without becoming a different sheep, and
        // `instances` is routed through `handle_scale` rather than through a
        // config write at all. Read-only here, so the pane never offers an
        // edit the daemon would refuse.
        let fields = FieldSet::from_fields(
            set.fields()
                .iter()
                .cloned()
                .map(|mut field| {
                    if apply_group(&field.key) == ApplyGroup::Structural {
                        field.editable = false;
                    }
                    field
                })
                .collect(),
            GROUP_ORDER,
        );
        let values = serde_json::to_value(&view.config)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        Self {
            target: PaneTarget::Sheep { name: view.name },
            fields,
            values,
            env_keys: view.env_keys,
            overridden: view.overridden,
            pending: view.pending,
            group: 0,
            view: Viewport::new(),
            typing: None,
            edits: Edits::default(),
            env_typing: None,
            list: None,
            help_open: false,
            section: None,
        }
    }

    /// A pane over one dog's `[<name>]` section.
    ///
    /// `schema` is the dog's own answer to the schema flag, probed at
    /// open rather than read from anywhere; `section` is the table as
    /// `Request::DogConfig` rendered it, empty when `dogs.toml` has none.
    ///
    /// Flat, in schema order, with no group headers: a dog's schema
    /// carries no `init.group`. A [`FieldKind::Map`] or
    /// [`FieldKind::List`] row is marked not editable and draws
    /// [`Lock::NoWidget`].
    #[must_use]
    pub fn dog(
        name: String,
        adopted_path: Option<PathBuf>,
        schema: Value,
        section: String,
    ) -> Self {
        let defs = schema
            .get("$defs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let set = FieldSet::from_properties(&properties, &defs, &[]);
        let fields = FieldSet::from_fields(
            set.fields()
                .iter()
                .cloned()
                .map(|mut field| {
                    if matches!(field.kind, FieldKind::Map | FieldKind::List(_)) {
                        field.editable = false;
                    }
                    field
                })
                .collect(),
            &[],
        );
        let values: Map<String, Value> = section
            .parse::<toml::Table>()
            .ok()
            .and_then(|table| serde_json::to_value(table).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        Self {
            target: PaneTarget::Dog { name, adopted_path },
            fields,
            values,
            env_keys: Vec::new(),
            overridden: Vec::new(),
            pending: Vec::new(),
            group: 0,
            view: Viewport::new(),
            typing: None,
            edits: Edits::default(),
            env_typing: None,
            list: None,
            help_open: false,
            section: Some(section),
        }
    }

    /// The section with every one of `edits` applied, in order, comments
    /// and key order intact, ready for `Request::SetDogConfig`.
    ///
    /// `toml_edit` rather than a re-render of [`Self::values`], and that is
    /// the whole reason this method exists: the request replaces the section
    /// wholesale, so a re-render would delete every comment in it on the
    /// operator's own keystroke.
    ///
    /// The plural is what a close actually sends: the request replaces the
    /// table, so two edits to one dog are one write, not two. Each is
    /// applied to the document the previous one produced.
    ///
    /// A `null` value removes the key, which is how the pane's empty buffer
    /// unsets one, and is what puts the dog back on its own default.
    ///
    /// [`None`] for a sheep pane, for an env edit, and for a section that
    /// does not parse, raised by any one entry: a partial section is worse
    /// than none, since the request would replace the table with it.
    #[must_use]
    pub fn edited_section_all(&self, edits: &[PaneEdit]) -> Option<String> {
        let section = self.section.as_deref()?;
        let mut doc: toml_edit::DocumentMut = section.parse().ok()?;
        for edit in edits {
            let PaneEdit::Set { key, value } = edit else {
                return None;
            };
            match value.as_value() {
                Value::Null => {
                    doc.remove(key);
                }
                Value::Bool(flag) => doc[key] = toml_edit::value(*flag),
                // A number that is neither an i64 nor an f64 is not
                // something TOML can hold, so the edit is refused rather
                // than rounded.
                Value::Number(number) => match (number.as_i64(), number.as_f64()) {
                    (Some(int), _) => doc[key] = toml_edit::value(int),
                    (None, Some(float)) => doc[key] = toml_edit::value(float),
                    (None, None) => return None,
                },
                Value::String(text) => doc[key] = toml_edit::value(text.as_str()),
                other => doc[key] = toml_edit::value(other.to_string()),
            }
        }
        Some(doc.to_string())
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
    pub(super) fn close(&mut self) -> Edits {
        core::mem::take(&mut self.edits)
    }

    /// Drops the most recently filed edit and names it, for `u`.
    pub(super) fn undo_edit(&mut self) -> Option<EditKey> {
        self.edits.undo()
    }

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
    pub(super) fn begin_env_typing(&mut self) {
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
    pub(super) fn cursor_env_key(&self) -> Option<Option<String>> {
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
    pub(super) fn adopt_env_cursor(&mut self, key: Option<&str>) {
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

    /// The open list sub-screen, or [`None`] when the field list is what is
    /// on screen.
    #[must_use]
    pub fn list(&self) -> Option<&ListPane> {
        self.list.as_ref()
    }

    pub(super) fn list_mut(&mut self) -> Option<&mut ListPane> {
        self.list.as_mut()
    }

    /// Opens the list sub-screen over the array field under the cursor.
    /// Does nothing on any other row.
    pub(super) fn open_list(&mut self) {
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
        let elements = self.elements_of(&key);
        self.list = Some(ListPane::new(key, item, elements));
    }

    /// Closes it, leaving the field list up.
    pub(super) fn close_list(&mut self) {
        self.list = None;
    }

    /// `key`'s array as one string per element, empty when the field holds
    /// no array. A non-scalar element renders as compact JSON, which is
    /// what an editor would have to type back.
    fn elements_of(&self, key: &str) -> Vec<String> {
        let Some(Value::Array(values)) = self.values.get(key) else {
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

    /// Files the whole array with `text` written at the sub-screen's
    /// cursor, appended when the cursor is on `+ new`.
    ///
    /// The whole array travels as one value: `Request::SetSheepField`
    /// carries one field, so an element is not a thing the wire can name.
    pub(super) fn file_list_element(&mut self, text: String) {
        let Some(elements) = self.list.as_ref().and_then(|list| list.with_element(text)) else {
            return;
        };
        self.file_list(elements);
    }

    /// Files the whole array without the element under the cursor.
    pub(super) fn file_list_removal(&mut self) {
        let Some(elements) = self.list.as_ref().and_then(ListPane::without_element) else {
            return;
        };
        self.file_list(elements);
    }

    /// Files the whole array with the element under the cursor moved
    /// `delta` places. Does nothing at either end, where there is nowhere
    /// to move.
    pub(super) fn file_list_reorder(&mut self, delta: isize) {
        let Some(elements) = self.list.as_ref().and_then(|list| list.reordered(delta)) else {
            return;
        };
        self.file_list(elements);
    }

    fn file_list(&mut self, elements: Vec<String>) {
        let Some(list) = self.list.as_ref() else {
            return;
        };
        let key = list.key().to_owned();
        let value = list_value(list.item(), &elements);
        self.file_field(key, value);
    }

    /// Whether `h` is showing the selected field's own help text.
    #[must_use]
    pub fn help_open(&self) -> bool {
        self.help_open
    }

    /// Flips it.
    pub(super) fn toggle_help(&mut self) {
        self.help_open = !self.help_open;
    }

    /// Dismisses it. A no-op when it is already closed, so `Escape` can
    /// call this unconditionally.
    pub(super) fn close_help(&mut self) {
        self.help_open = false;
    }

    /// Carries a previous pane's help visibility across a rebuild, the
    /// same reason [`Self::adopt_view`] carries the cursor: a re-read must
    /// not dismiss a note the operator has not dismissed.
    pub(super) fn set_help_open(&mut self, open: bool) {
        self.help_open = open;
    }

    /// The key under the cursor, and why the pane will not edit it, when it
    /// will not. [`None`] both for a row that edits and for no row at all.
    ///
    /// The one place a caller asks "may I edit what is selected", so a
    /// refusal is raised for the right one of [`Lock`]'s two reasons rather
    /// than for a generic third.
    #[must_use]
    pub fn cursor_lock(&self) -> Option<(&str, Lock)> {
        let PaneRow::Field(index) = self.cursor()? else {
            return None;
        };
        let field = self.fields.fields().get(index)?;
        self.lock(&field.key).map(|lock| (field.key.as_str(), lock))
    }

    /// The kind of widget the row under the cursor wants, or [`None`] for
    /// no row at all, or for one on an env row, which has no [`FieldKind`].
    #[must_use]
    pub fn cursor_kind(&self) -> Option<&FieldKind> {
        let PaneRow::Field(index) = self.cursor()? else {
            return None;
        };
        self.fields.fields().get(index).map(|field| &field.kind)
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
    /// The one door every config edit files through, which is what makes
    /// [`Edits::worst_impact`]'s claim about [`ApplyGroup::Structural`]
    /// checkable: every caller has already refused a locked row, and
    /// [`Self::lock`] locks exactly the Structural ones.
    fn file_field(&mut self, key: String, value: Value) {
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

    /// Files the buffer as an edit, typed to the field's kind.
    ///
    /// An empty buffer is `null`, which is how a nullable field is unset.
    /// An integer field whose buffer does not parse keeps the editor open
    /// rather than filing a string the daemon would refuse: the operator
    /// is mid-word, not wrong. Validation runs here, on the way in, so
    /// every entry in the set is one the pane is willing to send.
    pub fn apply_typing(&mut self) {
        let Some(PaneTyping { key, buffer }) = self.typing.take() else {
            return;
        };
        let kind = self.fields.by_key(&key).map(|field| field.kind.clone());
        let value = match (kind, buffer.as_str()) {
            (_, "") => Value::Null,
            (Some(FieldKind::Integer), text) => match text.parse::<i64>() {
                Ok(number) => Value::from(number),
                Err(_) => {
                    self.typing = Some(PaneTyping { key, buffer });
                    return;
                }
            },
            (_, text) => Value::String(text.to_owned()),
        };
        self.file_field(key, value);
    }

    /// Drops an editor under construction, leaving the pane open.
    pub fn abandon_typing(&mut self) {
        self.typing = None;
    }

    /// Files an env write from the sub-screen's own editor.
    ///
    /// Env files like everything else, and unlike everything else it is
    /// never compared against a stored value: `Request::SheepConfig`
    /// answers with the key names alone, so the pane has nothing to
    /// compare against and cannot tell a round trip from a change.
    pub(super) fn file_env(&mut self, key: String, value: Option<EnvValue>) {
        let impact = self.cost("env");
        self.edits.set(PaneEdit::SetEnv { key, value }, impact);
    }

    /// What is being edited.
    #[must_use]
    pub fn target(&self) -> &PaneTarget {
        &self.target
    }

    /// The form.
    #[must_use]
    pub fn fields(&self) -> &FieldSet {
        &self.fields
    }

    /// The current value of `key`, rendered for a cell.
    ///
    /// A scalar shows bare, an absent or `null` value shows `(unset)`, and
    /// anything else shows compact JSON. A sheep's `env` is the one field
    /// whose value this pane never holds, since the shepherd strips it on
    /// the way out, so it shows its key count instead, and the sub-screen
    /// shows the names.
    ///
    /// That special case is gated on the target, not on the key alone: a
    /// dog's schema is somebody else's, and one declaring a field named
    /// `env` reads its own section, not [`Self::env_keys`], which
    /// [`Self::dog`] always leaves empty.
    ///
    /// This is the parseable form: what [`Self::begin_typing`] seeds an
    /// editor with. [`Self::display_value`] is the one a row draws, and the
    /// two disagree exactly for a [`ValueKind`] field, where a seed has to
    /// stay something the daemon's own grammar still accepts.
    #[must_use]
    pub fn value(&self, key: &str) -> String {
        if key == "env" && matches!(self.target, PaneTarget::Sheep { .. }) {
            return match self.env_keys.len() {
                1 => "1 key".to_owned(),
                count => format!("{count} keys"),
            };
        }
        self.values
            .get(key)
            .map_or_else(|| render_json(&Value::Null), render_json)
    }

    /// The value a filed edit holds for `key`, rendered the way
    /// [`Self::value`] renders a stored one, or [`None`] when nothing is
    /// filed for it. Only ever answers for a config field: an env edit has
    /// no field key to be filed under.
    #[must_use]
    pub fn edited_value(&self, key: &str) -> Option<String> {
        let PaneEdit::Set { value, .. } = self.edits.get(&EditKey::Field(key.to_owned()))?.edit()
        else {
            return None;
        };
        let raw = render_json(value.as_value());
        Some(self.resolved_display(key, &raw))
    }

    /// [`Self::value`], resolved through its own grammar for a
    /// [`MemSize`]/[`UpDuration`] field: what a row draws instead of the
    /// digits an operator would otherwise have to already know the
    /// convention for.
    #[must_use]
    pub fn display_value(&self, key: &str) -> String {
        self.resolved_display(key, &self.value(key))
    }

    /// `raw` resolved through `key`'s own grammar, when `key` is one of
    /// shep-core's unit types. Display only: [`Self::value`] is what an
    /// editor still seeds and sends, so a suffix minted here never travels
    /// back out as part of a value.
    ///
    /// A `raw` that fails to parse, including whatever is mid-edit, comes
    /// back unchanged: this has no business guessing at a string shep is
    /// about to refuse on its own.
    ///
    /// Only a bare number is annotated. A value naming its own unit is the
    /// operator's spelling and survives as written, so a `60s` on disk is
    /// never redrawn as the `1m` its own `Display` would canonicalize it to.
    fn resolved_display(&self, key: &str, raw: &str) -> String {
        if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
            return raw.to_owned();
        }
        match self.fields.by_key(key).and_then(|field| field.value_kind) {
            Some(ValueKind::MemSize) => raw
                .parse::<MemSize>()
                .map_or_else(|_| raw.to_owned(), |_| format!("{raw} B")),
            Some(ValueKind::UpDuration) => raw
                .parse::<UpDuration>()
                .map_or_else(|_| raw.to_owned(), |_| format!("{raw}ms")),
            None => raw.to_owned(),
        }
    }

    /// What changing `key` costs.
    ///
    /// [`None`] is not reachable for a sheep and is not dead weight either:
    /// a dog decides for itself what a published change reloads, so the
    /// answer for one is "the pane does not know", and every caller already
    /// renders that as an empty cost cell rather than as a guess.
    #[must_use]
    pub fn cost(&self, key: &str) -> Option<ApplyGroup> {
        match self.target {
            PaneTarget::Sheep { .. } => Some(apply_group(key)),
            // The dog decides, not shep. Said once at the foot of the pane
            // rather than guessed per row.
            PaneTarget::Dog { .. } => None,
        }
    }

    /// Why the pane will not edit `key`, or [`None`] when it will.
    ///
    /// [`Lock::Refused`] outranks [`Lock::NoWidget`]: a Structural field
    /// that also happened to have no widget is still refused by shep, which
    /// is the fact that survives the pane gaining every widget it lacks.
    #[must_use]
    pub fn lock(&self, key: &str) -> Option<Lock> {
        if self.cost(key) == Some(ApplyGroup::Structural) {
            return Some(Lock::Refused);
        }
        match self.fields.by_key(key) {
            Some(field) if !field.editable => Some(Lock::NoWidget),
            _ => None,
        }
    }

    /// Whether an operator has overridden `key`.
    #[must_use]
    pub fn is_overridden(&self, key: &str) -> bool {
        self.overridden.iter().any(|name| name == key)
    }

    /// Whether `key` is parked until the next respawn.
    #[must_use]
    pub fn is_pending(&self, key: &str) -> bool {
        self.pending.iter().any(|name| name == key)
    }

    /// How many fields wait for a reload or a restart.
    ///
    /// Every one of them is already written to the override store, so this
    /// counts what the running process has not taken yet, never what an
    /// operator could lose.
    #[must_use]
    pub fn parked_count(&self) -> usize {
        self.pending.len()
    }

    /// Whether a reload of this sheep overlaps its replacement or runs
    /// serially. Always [`ReloadKind::Overlap`] for a dog, which has no
    /// such fields to read.
    #[must_use]
    pub fn reload_kind(&self) -> ReloadKind {
        let flag = |key: &str| {
            self.values
                .get(key)
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        let has_probe = self
            .values
            .get("readiness_probe")
            .is_some_and(|probe| !probe.is_null());
        reload_mode(flag("wait_ready"), has_probe, flag("reuse_port"))
    }

    /// The cursor and offset.
    #[must_use]
    pub fn view(&self) -> &Viewport {
        &self.view
    }

    /// Records the terminal's height, in rows of data.
    pub fn set_rows(&mut self, rows: usize) {
        let len = self.rows().len();
        self.view.set_rows(rows, len);
    }

    /// The active group's name, one of the eight in [`GROUP_ORDER`],
    /// defaulting to the first.
    ///
    /// Meaningless for a dog pane in the sense that nothing filters on it:
    /// a dog's schema carries no `init.group`, so every one of its fields
    /// is visible under any name this returns. See [`Self::rows`].
    #[must_use]
    pub fn group(&self) -> &'static str {
        GROUP_ORDER
            .get(self.group)
            .copied()
            .unwrap_or(GROUP_ORDER[0])
    }

    /// Walks to the next group, wrapping from the last back to the first.
    pub fn next_group(&mut self) {
        self.group = (self.group + 1) % GROUP_ORDER.len();
    }

    /// Jumps to the `digit`th group, one-based, the way `1`..`8` name them
    /// on the tab row. A digit past [`GROUP_ORDER`]'s length is ignored, so
    /// a ninth group added later needs a key of its own before it is
    /// reachable.
    pub fn set_group(&mut self, digit: u8) {
        let Some(index) = usize::from(digit).checked_sub(1) else {
            return;
        };
        if index < GROUP_ORDER.len() {
            self.group = index;
        }
    }

    /// One row per field of the active group, in display order, then this
    /// sheep's own env keys and the row that adds one. A field carrying no
    /// group at all (every field on a dog pane, whose schema declares
    /// none) is visible regardless of which group is active, which is
    /// what keeps a dog's flat list undisturbed by a control meant for a
    /// sheep's eight.
    ///
    /// The `env` field itself is left out of the field portion for a
    /// sheep: its own [`PaneRow::Env`] rows are what replaced the sub-screen
    /// it used to open, and a row that still showed the field's own "N
    /// keys" summary beside them would be the same fact said twice. A
    /// dog's schema is somebody else's and can declare a field named
    /// `env` of its own, which stays.
    #[must_use]
    pub fn rows(&self) -> Vec<PaneRow> {
        let group = self.group();
        let is_sheep = matches!(self.target, PaneTarget::Sheep { .. });
        let mut rows: Vec<PaneRow> = self
            .fields
            .fields()
            .iter()
            .enumerate()
            .filter(|(_, field)| !is_sheep || field.key != "env")
            .filter(|(_, field)| field.group.as_deref().is_none_or(|g| g == group))
            .map(|(index, _)| PaneRow::Field(index))
            .collect();
        if is_sheep {
            rows.extend((0..self.env_keys.len()).map(PaneRow::Env));
            rows.push(PaneRow::AddEnv);
        }
        rows
    }

    /// [`Self::rows`]'s own [`PaneRow::Field`] entries, alone: what the
    /// field body's own scroll walk lays out. [`Self::rows`] is the
    /// cursor's whole walk, env rows included; this is the narrower list
    /// the renderer needs to lay the field portion out on its own, since
    /// [`super::view::scroll::to_cursor`] has to know how many rows exist
    /// in the body it is walking, not in the cursor's wider one.
    #[must_use]
    pub(super) fn field_rows(&self) -> Vec<PaneRow> {
        self.rows()
            .into_iter()
            .filter(|row| matches!(row, PaneRow::Field(_)))
            .collect()
    }

    /// The row under the cursor, or `None` for an empty form.
    #[must_use]
    pub fn cursor(&self) -> Option<PaneRow> {
        let rows = self.rows();
        rows.get(self.view.cursor()).copied()
    }

    pub(super) fn move_by(&mut self, delta: isize) {
        let len = self.rows().len();
        self.view.move_by(delta, len);
    }

    pub(super) fn move_to_first(&mut self) {
        let len = self.rows().len();
        self.view.move_to(0, len);
    }

    pub(super) fn move_to_last(&mut self) {
        let len = self.rows().len();
        self.view.move_to(len.saturating_sub(1), len);
    }

    /// Adopts a previous pane's pending set, so a refresh does not
    /// silently drop changes the operator has not written yet.
    ///
    /// The set is the operator's and the values are the shepherd's: a
    /// re-read replaces every value on screen and keeps every edit filed
    /// over them.
    ///
    /// An open editor is deliberately not carried: its buffer was seeded
    /// from a value this refresh may have just changed, so keeping it
    /// would put the operator halfway through editing something that is no
    /// longer there. `App::release_text_mode_if_unowned` is what puts the
    /// keyboard back when that happens.
    pub(super) fn adopt_edits(&mut self, previous: Edits) {
        self.edits = previous;
    }

    /// Adopts a previous pane's cursor and offset, clamped to this one's
    /// own row count. What a refresh of an already-open pane rides on, so
    /// `r` does not throw the operator back to the first field.
    pub(super) fn adopt_view(&mut self, view: Viewport) {
        self.view = view;
        let len = self.rows().len();
        self.view.clamp(len);
    }

    /// Re-opens the list sub-screen on the refreshed array, at the cursor
    /// and offset it had. Setting an element re-reads the whole config,
    /// and without this the sub-screen would slam shut on the operator's
    /// own keystroke.
    pub(super) fn adopt_list_view(&mut self, key: &str, view: Viewport) {
        let Some(item) = self.fields.by_key(key).and_then(|field| match field.kind {
            FieldKind::List(item) => Some(item),
            _ => None,
        }) else {
            return;
        };
        let mut list = ListPane::new(key.to_owned(), item, self.elements_of(key));
        list.adopt_view(view);
        self.list = Some(list);
    }

    /// Switches to `key`'s own group, then walks the cursor onto it, the
    /// way an operator reaches a field in a different group than the one
    /// the pane opened on. A no-op for a name no field carries.
    #[cfg(test)]
    pub(crate) fn move_to_key(&mut self, key: &str) {
        let Some(field) = self.fields.by_key(key) else {
            return;
        };
        if let Some(group) = field.group.clone()
            && let Some(index) = GROUP_ORDER.iter().position(|known| *known == group)
        {
            self.group = index;
        }
        if let Some(row_index) = self.rows().iter().position(|row| match row {
            PaneRow::Field(field_index) => self.fields.fields()[*field_index].key == key,
            PaneRow::Env(_) | PaneRow::AddEnv => false,
        }) {
            let len = self.rows().len();
            self.view.move_to(row_index, len);
        }
    }
}

/// Which of the daemon's two reloads an app takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadKind {
    /// The replacement is spawned alongside the instance it replaces.
    Overlap,
    /// The instance being replaced is drained first, so the app is down for
    /// the length of the drain.
    Serial,
}

impl ReloadKind {
    /// The word the pane's own menu prints for it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Overlap => "overlapping",
            Self::Serial => "serial",
        }
    }
}

/// Whether a reload of this app overlaps or runs serially.
///
/// The daemon decides through `ReadinessSource::of`, which answers for
/// `wait_ready` before it reads `readiness_probe`, so an app with both
/// overlaps.
const fn reload_mode(wait_ready: bool, has_probe: bool, reuse_port: bool) -> ReloadKind {
    if !wait_ready && has_probe && !reuse_port {
        ReloadKind::Serial
    } else {
        ReloadKind::Overlap
    }
}

#[cfg(test)]
mod tests {
    use shep_core::config::{AppConfig, ProbeConfig, ProbeKind};

    use super::*;

    fn web() -> SheepConfigView {
        let mut config = AppConfig {
            name: "web".into(),
            max_restarts: 32,
            ..AppConfig::default()
        };
        config
            .env
            .insert("DB_HOST".into(), "{{shared:DB_HOST}}".into());
        SheepConfigView::new(config, vec!["max_restarts".into()], vec!["env".into()])
    }

    /// The value the pane has filed for the config field `key`, or
    /// [`None`] when nothing is filed for it.
    fn filed(pane: &ConfigPane, key: &str) -> Option<Value> {
        match pane.edits().get(&EditKey::Field(key.to_owned()))?.edit() {
            PaneEdit::Set { value, .. } => Some(value.as_value().clone()),
            PaneEdit::SetEnv { .. } => None,
        }
    }

    /// What the pane recorded that filed edit as costing.
    fn filed_impact(pane: &ConfigPane, key: &str) -> Option<ApplyGroup> {
        pane.edits().get(&EditKey::Field(key.to_owned()))?.impact()
    }

    fn web_with_args(args: &[&str]) -> SheepConfigView {
        let config = AppConfig {
            name: "web".into(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            stop_exit_codes: vec![0, 143],
            ..AppConfig::default()
        };
        SheepConfigView::new(config, Vec::new(), Vec::new())
    }

    #[test]
    fn a_sheep_pane_has_forty_one_fields_in_eight_groups() {
        let pane = ConfigPane::sheep(web());
        assert_eq!(pane.fields().len(), 41);
        assert!(!pane.fields().is_empty());
        let mut groups: Vec<&str> = Vec::new();
        for field in pane.fields().fields() {
            let group = field.group.as_deref().expect("every field carries a group");
            if groups.last() != Some(&group) {
                groups.push(group);
            }
        }
        assert_eq!(
            groups,
            [
                "process",
                "logging",
                "inputs",
                "restart",
                "readiness",
                "shutdown",
                "watch",
                "cron"
            ]
        );
    }

    #[test]
    fn a_value_renders_bare_for_a_scalar_and_as_a_count_for_env() {
        let pane = ConfigPane::sheep(web());
        assert_eq!(pane.value("max_restarts"), "32");
        assert_eq!(pane.value("autorestart"), "true");
        assert_eq!(pane.value("cwd"), "(unset)");
        assert_eq!(pane.value("env"), "1 key");
    }

    #[test]
    fn cost_comes_from_apply_group_for_a_sheep() {
        let pane = ConfigPane::sheep(web());
        assert_eq!(pane.cost("max_restarts"), Some(ApplyGroup::Live));
        assert_eq!(pane.cost("kill_signal"), Some(ApplyGroup::NextSpawn));
        assert_eq!(pane.cost("script"), Some(ApplyGroup::NeedsRespawn));
        assert_eq!(pane.cost("instances"), Some(ApplyGroup::Structural));
    }

    #[test]
    fn structural_fields_are_not_editable_and_the_rest_are() {
        let pane = ConfigPane::sheep(web());
        for key in ["name", "instances"] {
            assert!(!pane.fields().by_key(key).unwrap().editable, "{key}");
        }
        assert!(pane.fields().by_key("max_restarts").unwrap().editable);
    }

    #[test]
    fn the_menu_counts_the_parked_fields_once() {
        let mut config = AppConfig {
            name: "web".into(),
            ..AppConfig::default()
        };
        config.env.insert("DB_HOST".into(), "db.internal".into());
        let view =
            SheepConfigView::new(config, Vec::new(), vec!["env".into(), "kill_signal".into()]);
        assert_eq!(ConfigPane::sheep(view).parked_count(), 2);
        assert_eq!(ConfigPane::sheep(web_with_args(&[])).parked_count(), 0);
    }

    #[test]
    fn a_probe_without_reuse_port_is_the_only_serial_reload() {
        assert_eq!(reload_mode(false, true, false), ReloadKind::Serial);
        assert_eq!(reload_mode(true, true, false), ReloadKind::Overlap);
        assert_eq!(reload_mode(false, true, true), ReloadKind::Overlap);
        assert_eq!(reload_mode(false, false, false), ReloadKind::Overlap);
    }

    #[test]
    fn a_pane_reads_its_reload_kind_off_the_three_fields_it_turns_on() {
        let probed = |wait_ready, reuse_port| {
            let config = AppConfig {
                name: "web".into(),
                wait_ready,
                reuse_port,
                readiness_probe: Some(ProbeConfig {
                    kind: ProbeKind::Tcp,
                    target: "127.0.0.1:8080".into(),
                    interval: UpDuration::from_millis(10_000),
                    timeout: UpDuration::from_millis(5_000),
                    failure_threshold: 3,
                }),
                ..AppConfig::default()
            };
            ConfigPane::sheep(SheepConfigView::new(config, Vec::new(), Vec::new())).reload_kind()
        };
        assert_eq!(probed(false, false), ReloadKind::Serial);
        assert_eq!(probed(true, false), ReloadKind::Overlap);
        assert_eq!(probed(false, true), ReloadKind::Overlap);
        assert_eq!(
            ConfigPane::sheep(web()).reload_kind(),
            ReloadKind::Overlap,
            "no probe, so nothing an outgoing instance could answer"
        );
    }

    #[test]
    fn overridden_and_pending_are_read_off_the_view() {
        let pane = ConfigPane::sheep(web());
        assert!(pane.is_overridden("max_restarts"));
        assert!(!pane.is_overridden("autorestart"));
        assert!(pane.is_pending("env"));
        assert!(!pane.is_pending("max_restarts"));
    }

    #[test]
    fn the_cursor_walks_the_rows_and_clamps_at_both_ends() {
        let mut pane = ConfigPane::sheep(web());
        assert_eq!(pane.cursor(), Some(PaneRow::Field(0)));
        pane.move_by(1);
        assert_eq!(pane.cursor(), Some(PaneRow::Field(1)));
        pane.move_by(-5);
        assert_eq!(pane.cursor(), Some(PaneRow::Field(0)));
        pane.move_to_last();
        // `process`, the group a fresh pane opens on, has ten fields, at
        // indices `0..10` since it sorts first, then `web`'s own one env
        // key and the row that adds another.
        assert_eq!(pane.cursor(), Some(PaneRow::AddEnv));
        assert_eq!(
            pane.fields().fields()[9].key,
            "user",
            "the last field is process's own last one"
        );
        pane.move_to_first();
        assert_eq!(pane.cursor(), Some(PaneRow::Field(0)));
    }

    #[test]
    fn a_refreshed_pane_keeps_the_cursor_it_had() {
        let mut pane = ConfigPane::sheep(web());
        pane.set_rows(10);
        pane.move_to_last();
        let carried = pane.view().clone();
        let mut fresh = ConfigPane::sheep(web());
        fresh.adopt_view(carried);
        assert_eq!(fresh.cursor(), Some(PaneRow::AddEnv));
    }

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
        pane.apply_typing();
        assert_eq!(filed(&pane, "history_bytes"), Some(serde_json::json!(8192)));
        assert_eq!(filed_impact(&pane, "history_bytes"), None);
    }

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

    /// The invariant [`Edits::worst_impact`]'s own doc rests on: nothing a
    /// keystroke can do files a `Structural` edit, because
    /// [`ConfigPane::sheep`] marks those fields not editable and every
    /// filing door checks [`ConfigPane::lock`] first.
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
            pane.apply_typing();
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
        pane.apply_typing();
        assert_eq!(filed(&pane, "max_restarts"), Some(serde_json::json!(40)));
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
        pane.apply_typing();
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

    /// `kill_timeout` defaults to 1600ms, which `UpDuration::Display`
    /// prints as the bare digits `"1600"`; `listen_timeout` defaults to
    /// 3s, which it already prints with a unit. `value` stays the raw,
    /// parseable form an editor would seed from.
    #[test]
    fn a_bare_up_duration_shows_its_resolved_unit_and_a_suffixed_one_is_unchanged() {
        let pane = ConfigPane::sheep(web());
        assert_eq!(pane.display_value("kill_timeout"), "1600ms");
        assert_eq!(pane.value("kill_timeout"), "1600");
        assert_eq!(pane.display_value("listen_timeout"), "3s");
    }

    /// A dog's values come from the section its operator wrote, so a
    /// spelling that `UpDuration::Display` would canonicalize to `1m`
    /// stays as `60s`. A sheep's arrive already canonicalized, `UpDuration`
    /// serializing through its own `Display`, so this is the pane that can
    /// tell the difference.
    #[test]
    fn a_dogs_own_spelling_of_a_duration_survives_the_pane() {
        let schema = serde_json::json!({
            "properties": {
                "poll": { "type": "string", "$ref": "#/$defs/UpDuration" }
            },
            "$defs": { "UpDuration": { "type": "string" } }
        });
        let pane = ConfigPane::dog("bark".into(), None, schema, "poll = \"60s\"\n".into());
        assert_eq!(pane.display_value("poll"), "60s");
    }

    /// 64 bytes is not a whole `K`/`M`/`G`, so `MemSize::Display` prints
    /// the bare digits `"64"`; 512 mebibytes already prints with a unit.
    #[test]
    fn a_bare_mem_size_shows_its_resolved_unit_and_a_suffixed_one_is_unchanged() {
        for (bytes, want) in [(64, "64 B"), (512 << 20, "512M")] {
            let config = AppConfig {
                name: "web".into(),
                max_memory: Some(MemSize::from_bytes(bytes)),
                ..AppConfig::default()
            };
            let pane = ConfigPane::sheep(SheepConfigView::new(config, Vec::new(), Vec::new()));
            assert_eq!(pane.display_value("max_memory"), want, "{bytes} bytes");
        }
    }

    /// A field with no `MemSize`/`UpDuration` grammar is untouched by
    /// `display_value`, the same as `value`.
    #[test]
    fn a_field_with_no_unit_grammar_displays_the_same_either_way() {
        let pane = ConfigPane::sheep(web());
        assert_eq!(pane.display_value("cwd"), pane.value("cwd"));
        assert_eq!(
            pane.display_value("max_restarts"),
            pane.value("max_restarts")
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
        for typed in ["64", "banana"] {
            let mut pane = ConfigPane::sheep(web());
            pane.move_to_key("max_memory");
            pane.begin_typing();
            for c in typed.chars() {
                pane.type_char(c);
            }
            pane.apply_typing();
            assert_eq!(
                filed(&pane, "max_memory"),
                Some(serde_json::json!(typed)),
                "{typed}"
            );
        }
    }

    #[test]
    fn toggling_help_flips_it_and_closing_it_is_idempotent() {
        let mut pane = ConfigPane::sheep(web());
        assert!(!pane.help_open());
        pane.toggle_help();
        assert!(pane.help_open());
        pane.toggle_help();
        assert!(!pane.help_open());
        pane.toggle_help();
        pane.close_help();
        assert!(!pane.help_open());
        pane.close_help();
        assert!(
            !pane.help_open(),
            "closing an already-closed help is a no-op"
        );
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

    /// The bark section every dog test below reads: a comment, a scalar,
    /// and a sink carrying a credential.
    fn bark_section() -> String {
        "# how often\npoll = \"60s\"\nhistory_bytes = 4096\n\n[sinks.ops]\nkind = \"slack\"\nurl = \"https://hooks.example/x\"\n".to_owned()
    }

    fn bark_pane() -> ConfigPane {
        let schema = crate::dog::builtin_schema("bark").expect("bark is a built-in");
        ConfigPane::dog("bark".into(), None, schema, bark_section())
    }

    /// A dog's schema carries no group, so the pane draws no headers, and
    /// shep does not classify a dog's field cost: `cost` answers `None`
    /// for one.
    #[test]
    fn a_dog_pane_is_flat_in_schema_order_and_marks_the_secret() {
        let pane = bark_pane();
        assert!(
            pane.fields()
                .fields()
                .iter()
                .all(|field| field.group.is_none()),
            "a dog's schema carries no group, so the pane draws no headers"
        );
        let keys: Vec<&str> = pane
            .fields()
            .fields()
            .iter()
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(
            keys,
            ["history_bytes", "poll", "rules", "sink_timeout", "sinks"],
            "schema order, which for a serde_json map is alphabetical"
        );
        assert!(
            pane.fields()
                .by_key("sinks")
                .expect("sinks is a field")
                .secret
        );
        assert_eq!(pane.value("poll"), "60s");
        assert_eq!(pane.cost("poll"), None);
    }

    /// Shep writes a `sinks` table happily; this screen simply has no
    /// widget for a table of tables.
    #[test]
    fn a_dogs_map_field_is_locked_for_want_of_a_widget_and_not_refused() {
        let pane = bark_pane();
        assert_eq!(pane.lock("sinks"), Some(Lock::NoWidget));
        assert_eq!(pane.lock("rules"), Some(Lock::NoWidget));
        assert_eq!(pane.lock("poll"), None);
    }

    /// A dog whose schema declares one secret string. No built-in has one:
    /// bark's only secret is a map, and a map has no editor to type a
    /// secret into, so the leak the confirm sentence could carry was not
    /// reachable from any fixture the pane already had.
    fn secret_dog_pane() -> ConfigPane {
        let schema = serde_json::json!({
            "properties": {
                "webhook": { "type": "string", "x-shep-secret": true },
            }
        });
        ConfigPane::dog(
            "pydog".into(),
            None,
            schema,
            "webhook = \"https://hook/OLD\"\n".to_owned(),
        )
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

    /// Re-rendering from parsed values would delete every comment in a
    /// file shep does not author.
    #[test]
    fn an_edited_section_keeps_its_comments_and_changes_one_key() {
        let pane = bark_pane();
        let out = pane
            .edited_section_all(&[PaneEdit::Set {
                key: "poll".into(),
                value: serde_json::json!("30s").into(),
            }])
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
        let out = pane
            .edited_section_all(&[PaneEdit::Set {
                key: "history_bytes".into(),
                value: serde_json::Value::Null.into(),
            }])
            .expect("the fixture section parses");
        assert!(!out.contains("history_bytes"), "{out}");
        assert!(out.contains("# how often"), "{out}");
    }

    /// A section here would send a sheep's config out through a dog's
    /// door.
    #[test]
    fn a_sheep_pane_has_no_section_to_edit() {
        let pane = ConfigPane::sheep(web());
        assert_eq!(
            pane.edited_section_all(&[PaneEdit::Set {
                key: "cwd".into(),
                value: serde_json::json!("/srv").into(),
            }]),
            None
        );
    }

    /// `cwd` and `script` routinely hold a home directory and `args`
    /// holds a token, which is why `ConfigPane`'s own `Debug` withholds
    /// the map these come out of. Asserted on `FieldValue` itself, not
    /// on a type that carries it, since a wrapper could redact while the
    /// value underneath still prints.
    #[test]
    fn a_field_values_debug_names_no_value() {
        for (value, want) in [
            (serde_json::json!("/home/ada/secret-project"), "<string>"),
            (serde_json::json!(40), "<number>"),
            (serde_json::json!(false), "<bool>"),
            (serde_json::json!(null), "<null>"),
            (serde_json::json!(["--token", "hunter2"]), "<array>"),
            (serde_json::json!({ "a": 1 }), "<object>"),
        ] {
            let wrapped: FieldValue = value.into();
            assert_eq!(format!("{wrapped:?}"), format!("FieldValue({want})"));
        }
    }

    /// `Debug` is derived, safe only because both value types redact
    /// themselves; this test pins that fact.
    #[test]
    fn a_pane_edits_debug_names_no_value() {
        let set = PaneEdit::Set {
            key: "cwd".into(),
            value: serde_json::json!("/home/ada/secret-project").into(),
        };
        assert_eq!(
            format!("{set:?}"),
            r#"Set { key: "cwd", value: FieldValue(<string>) }"#
        );
        let env = PaneEdit::SetEnv {
            key: "DB_PASSWORD".into(),
            value: Some("hunter2".to_owned().into()),
        };
        assert_eq!(
            format!("{env:?}"),
            r#"SetEnv { key: "DB_PASSWORD", value: Some(EnvValue(<7 bytes>)) }"#
        );
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

    /// A dog's section is the single most credential-dense thing this
    /// screen loads: bark's own `sinks` table is a webhook URL with a
    /// bearer token in it. The redaction is on the pane, not on each
    /// type that carries the text.
    #[test]
    fn a_dog_panes_debug_names_no_section() {
        let pane = bark_pane();
        assert_eq!(
            format!("{pane:?}"),
            r#"ConfigPane { target: Dog { name: "bark", adopted_path: None }, fields: 5, env_keys: 0, cursor: 0 }"#
        );
    }

    /// `args` and `cwd` live in `values` and routinely carry a token or a
    /// home directory (IR-41).
    #[test]
    fn the_panes_debug_names_no_value_it_holds() {
        let mut config = AppConfig {
            name: "web".into(),
            cwd: Some("/home/ada/secret-project".into()),
            args: vec!["--token".into(), "hunter2".into()],
            ..AppConfig::default()
        };
        config.env.insert("DB_PASSWORD".into(), "hunter2".into());
        let pane = ConfigPane::sheep(SheepConfigView::new(config, Vec::new(), Vec::new()));
        assert_eq!(
            format!("{pane:?}"),
            r#"ConfigPane { target: Sheep { name: "web" }, fields: 41, env_keys: 1, cursor: 0 }"#
        );
    }

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
    /// kind of write an element edit is.
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
            Some(serde_json::json!(["b", "a", "c"])),
            "the second keystroke replaces the entry rather than adding one"
        );
        assert_eq!(pane.edits().len(), 1, "one field, one entry");
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

    /// A dog's write replaces its whole section, and `edited_section_all` has
    /// no rendering for an array, so the pane offers no editor for one.
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
