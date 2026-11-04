//! `shep.pane`: the status a dog builds for the operator to read.
//!
//! Lookout asks for it on the dog's shepherd channel with [`PANE_ACTION`], the
//! way a settings write asks [`VALIDATE_ACTION`](super::validate::VALIDATE_ACTION),
//! and draws the answer as written: every value is a string and lookout
//! formats nothing. A dog builds one with [`Pane`]'s builder or answers the
//! JSON in any language, and both ends go through [`Pane::body`] and
//! [`Pane::read`], so they cannot disagree about the JSON.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::validate::unreadable;

/// The action name lookout sends on a dog's shepherd channel to ask for its
/// pane.
///
/// A `shep-` name, reserved the way
/// [`VALIDATE_ACTION`](super::validate::VALIDATE_ACTION) is. Lookout sends it
/// as the operator, so it needs no grant the operator does not already hold.
pub const PANE_ACTION: &str = "shep.pane";

/// A dog's answer to [`PANE_ACTION`]: its sections, then the actions that need
/// no row to run.
///
/// Build one with [`new`](Self::new) and the builder methods, or read a dog's
/// answer back with [`read`](Self::read). Lookout decides the layout and the
/// styling. `Debug` prints each section's id and its row count, never a value.
#[derive(Clone, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Pane {
    /// The sections, in the order the dog wants them drawn.
    pub sections: Vec<Section>,
    /// The actions after the sections, each triggered with no params.
    #[serde(default)]
    pub actions: Vec<PaneAction>,
    /// How many sections were skipped because this shep does not know their
    /// kind.
    ///
    /// A later shep adds kinds and an older lookout still draws the rest, so
    /// an unknown kind is skipped rather than unreadable, and lookout says how
    /// many it did not draw. Not part of the answer a dog writes.
    #[serde(skip)]
    pub skipped: usize,
}

impl Pane {
    /// A pane with no sections and no actions.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a `fields` section `id`, titled `id`, holding `pairs`.
    pub fn fields<K, V>(
        mut self,
        id: impl Into<String>,
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        let id = id.into();
        self.sections.push(Section::Fields(Fields {
            fields: pairs
                .into_iter()
                .map(|(label, value)| Field::new(label, value))
                .collect(),
            id: id.clone(),
            title: id,
            refresh_on: Vec::new(),
        }));
        self
    }

    /// Add a `table` section `id`, titled `id`, with `columns` and `rows`.
    pub fn table<C>(
        mut self,
        id: impl Into<String>,
        columns: impl IntoIterator<Item = C>,
        rows: impl IntoIterator<Item = Row>,
    ) -> Self
    where
        C: Into<String>,
    {
        let id = id.into();
        self.sections.push(Section::Table(Table {
            columns: columns.into_iter().map(Into::into).collect(),
            id: id.clone(),
            refresh_on: Vec::new(),
            row_actions: Vec::new(),
            rows: rows.into_iter().collect(),
            title: id,
        }));
        self
    }

    /// Name the event kinds that make lookout ask for the pane again while the
    /// section added last is on screen. With no section added yet it adds
    /// nothing.
    pub fn refresh_on<K>(mut self, kinds: impl IntoIterator<Item = K>) -> Self
    where
        K: Into<String>,
    {
        let kinds: Vec<String> = kinds.into_iter().map(Into::into).collect();
        match self.sections.last_mut() {
            Some(Section::Fields(section)) => section.refresh_on = kinds,
            Some(Section::Table(section)) => section.refresh_on = kinds,
            None => {}
        }
        self
    }

    /// Add a row action to the table added last, triggered with
    /// `{"row": "<key>"}`. With no table added last it adds nothing.
    pub fn row_action(
        mut self,
        action: impl Into<String>,
        label: impl Into<String>,
        destructive: Destructive,
    ) -> Self {
        if let Some(Section::Table(section)) = self.sections.last_mut() {
            section
                .row_actions
                .push(PaneAction::new(action, label, destructive));
        }
        self
    }

    /// Add a pane action, triggered with no params.
    pub fn action(
        mut self,
        action: impl Into<String>,
        label: impl Into<String>,
        destructive: Destructive,
    ) -> Self {
        self.actions
            .push(PaneAction::new(action, label, destructive));
        self
    }

    /// The JSON a dog answers [`PANE_ACTION`] with.
    ///
    /// # Panics
    /// Never: every field is a string, a bool, or a list of them.
    #[must_use = "the body is what the dog writes back on its channel"]
    #[track_caller]
    pub fn body(&self) -> String {
        serde_json::to_string(self).expect("a pane holds only strings, bools and lists of them")
    }

    /// What a dog answered [`PANE_ACTION`] with.
    ///
    /// A section of a kind this shep does not know is skipped and counted in
    /// [`skipped`](Self::skipped): a later shep adds kinds and an older lookout
    /// still draws the rest. Everything else that is wrong makes the whole
    /// answer unreadable, because lookout keys its state and its actions by
    /// section id and row key.
    ///
    /// # Errors
    /// The sentence the operator reads: what is wrong with the answer, then the
    /// dog's own answer quoted at the cap
    /// [`shep.validate`](super::validate::Verdict::read) quotes an unreadable
    /// answer at.
    pub fn read(body: &str) -> Result<Self, String> {
        let Ok(Value::Object(mut root)) = serde_json::from_str(body) else {
            return Err(unreadable(body));
        };
        let sections = match root.remove("sections") {
            Some(Value::Array(sections)) => sections,
            Some(_not_a_list) => return Err(refused("sections is not a list", body)),
            None => Vec::new(),
        };
        let actions = match root.remove("actions") {
            Some(Value::Array(actions)) => actions,
            Some(_not_a_list) => return Err(refused("actions is not a list", body)),
            None => Vec::new(),
        };

        let mut read = Vec::with_capacity(sections.len());
        let mut skipped = 0;
        for section in sections {
            let Value::Object(mut section) = section else {
                return Err(refused("a section is not an object", body));
            };
            let Value::String(kind) = section.remove("kind").unwrap_or(Value::Null) else {
                return Err(refused("a section names no kind", body));
            };
            match kind.as_str() {
                "fields" => match serde_json::from_value::<Fields>(Value::Object(section)) {
                    Ok(section) => read.push(Section::Fields(section)),
                    Err(_) => return Err(refused("a fields section is unreadable", body)),
                },
                "table" => {
                    let table = match serde_json::from_value::<Table>(Value::Object(section)) {
                        Ok(table) => table,
                        Err(_) => return Err(refused("a table section is unreadable", body)),
                    };
                    let columns = table.columns.len();
                    if table.rows.iter().any(|row| row.cells.len() != columns) {
                        return Err(refused("a table row holds no cell for each column", body));
                    }
                    if let Some(key) = repeated(table.rows.iter().map(|row| row.key.as_str())) {
                        return Err(refused(
                            &format!("a table uses the row key `{key}` twice"),
                            body,
                        ));
                    }
                    read.push(Section::Table(table));
                }
                _other => skipped += 1,
            }
        }
        if let Some(id) = repeated(read.iter().map(|section| id_of(section).as_str())) {
            return Err(refused(&format!("two sections share the id `{id}`"), body));
        }
        let actions = match serde_json::from_value::<Vec<PaneAction>>(Value::Array(actions)) {
            Ok(actions) => actions,
            Err(_) => return Err(refused("a pane action is unreadable", body)),
        };

        Ok(Self {
            sections: read,
            actions,
            skipped,
        })
    }
}

impl core::fmt::Debug for Pane {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Pane")
            .field("sections", &SectionList(&self.sections))
            .field("actions", &self.actions.len())
            .field("skipped", &self.skipped)
            .finish()
    }
}

/// `Pane`'s sections, printed through their own `Debug`.
struct SectionList<'a>(&'a [Section]);

impl core::fmt::Debug for SectionList<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list().entries(self.0.iter()).finish()
    }
}

/// One block of a dog's pane, tagged by its `kind` on the wire.
///
/// `Debug` prints the id and the counts, never a value.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Section {
    /// A `fields` section: label and value pairs.
    Fields(Fields),
    /// A `table` section: named columns and rows of cells.
    Table(Table),
}

impl core::fmt::Debug for Section {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Fields(section) => f
                .debug_struct("Fields")
                .field("id", &section.id)
                .field("fields", &section.fields.len())
                .finish(),
            Self::Table(section) => f
                .debug_struct("Table")
                .field("id", &section.id)
                .field("rows", &section.rows.len())
                .field("row_actions", &section.row_actions.len())
                .finish(),
        }
    }
}

/// The `fields` section: label and value pairs.
///
/// `Debug` prints the id and the pair count. A [`Field`]'s value is what it
/// leaves out.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Fields {
    /// What lookout keys this section, and its actions, by.
    pub id: String,
    /// The line lookout draws above the section.
    pub title: String,
    /// The event kinds that make lookout ask for the pane again, from the
    /// manifest's `events`.
    #[serde(default)]
    pub refresh_on: Vec<String>,
    /// The pairs, in the order the dog wants them drawn.
    pub fields: Vec<Field>,
}

impl core::fmt::Debug for Fields {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Fields")
            .field("id", &self.id)
            .field("title", &self.title)
            .field("refresh_on", &self.refresh_on)
            .field("fields", &self.fields.len())
            .finish()
    }
}

/// The `table` section: named columns and rows.
///
/// `Debug` prints the id, the row count and the row action count. A
/// [`Row`]'s cells are what it leaves out.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Table {
    /// What lookout keys this section, and its actions, by.
    pub id: String,
    /// The line lookout draws above the section.
    pub title: String,
    /// The event kinds that make lookout ask for the pane again, from the
    /// manifest's `events`.
    #[serde(default)]
    pub refresh_on: Vec<String>,
    /// The column headings, one per cell.
    pub columns: Vec<String>,
    /// The rows, each holding one cell per column.
    pub rows: Vec<Row>,
    /// The actions a row is needed to run, each triggered with
    /// `{"row": "<key>"}`.
    #[serde(default)]
    pub row_actions: Vec<PaneAction>,
}

impl core::fmt::Debug for Table {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Table")
            .field("id", &self.id)
            .field("title", &self.title)
            .field("refresh_on", &self.refresh_on)
            .field("columns", &self.columns)
            .field("rows", &self.rows.len())
            .field("row_actions", &self.row_actions)
            .finish()
    }
}

/// One label and value pair of a [`Fields`] section.
///
/// `Debug` prints the label and never the value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Field {
    /// The word lookout draws beside it.
    pub label: String,
    /// What the dog says. Lookout formats nothing.
    pub value: String,
}

impl Field {
    /// A pair labelled `label` holding `value`.
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

impl core::fmt::Debug for Field {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Field")
            .field("label", &self.label)
            .field("value", &format_args!("<elided>"))
            .finish()
    }
}

/// One row of a [`Table`] section.
///
/// `Debug` prints the key and the cell count, never a cell.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Row {
    /// What lookout keys this row by, and triggers its row actions with.
    pub key: String,
    /// One cell per column, in column order.
    pub cells: Vec<String>,
}

impl Row {
    /// A row keyed `key`, holding `cells`.
    pub fn new<C>(key: impl Into<String>, cells: impl IntoIterator<Item = C>) -> Self
    where
        C: Into<String>,
    {
        Self {
            key: key.into(),
            cells: cells.into_iter().map(Into::into).collect(),
        }
    }
}

impl core::fmt::Debug for Row {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Row")
            .field("key", &self.key)
            .field("cells", &self.cells.len())
            .finish()
    }
}

/// One action of a pane: a name from the manifest's `actions`, a label, and
/// whether it asks first.
///
/// Lookout skips an action the manifest does not declare and says so in the
/// pane's footer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PaneAction {
    /// The action name `shep trigger <dog> <action>` sends.
    pub action: String,
    /// The line lookout shows beside its key.
    pub label: String,
    /// Whether lookout asks the operator before triggering it.
    pub destructive: bool,
}

impl PaneAction {
    /// An action `action`, labelled `label`, asked for first or not.
    pub fn new(
        action: impl Into<String>,
        label: impl Into<String>,
        destructive: Destructive,
    ) -> Self {
        Self {
            action: action.into(),
            label: label.into(),
            destructive: matches!(destructive, Destructive::Yes),
        }
    }
}

/// Whether a pane action asks the operator before it runs.
///
/// A two-variant enum rather than a `bool` at the call site, where `true` says
/// nothing of itself. Not `#[non_exhaustive]`: it mirrors a bool, and a variant
/// no one can name would force every reader to match a case that cannot
/// happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destructive {
    /// Lookout asks "Run <label> on <row key>?" before triggering it.
    Yes,
    /// Lookout triggers it on its key.
    No,
}

/// The id of a section, for the duplicate check.
fn id_of(section: &Section) -> &String {
    match section {
        Section::Fields(section) => &section.id,
        Section::Table(section) => &section.id,
    }
}

/// The first value `values` holds twice, and `None` when each is held once.
fn repeated<'a>(values: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let mut seen: Vec<&str> = Vec::new();
    for value in values {
        if seen.contains(&value) {
            return Some(value);
        }
        seen.push(value);
    }
    None
}

/// The reason an answer is refused: `fault` names what is wrong, and the dog's
/// own answer follows it, quoted as `shep.validate` quotes one.
fn refused(fault: &str, body: &str) -> String {
    unreadable(&format!("{fault}: {body}"))
}

#[cfg(test)]
mod tests;
