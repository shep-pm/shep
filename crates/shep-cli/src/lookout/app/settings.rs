//! The settings screen's rows, its pending edit, and the sentence each confirm
//! shows.

use super::*;
use crate::commands::settings::{STYLE_LEVEL_KEY, Shape, dotted};
use crate::lookout::field::{Child, Field, FieldKind, shep_toml_shape};
use std::collections::BTreeSet;

/// One row the settings screen's cursor can sit on.
///
/// The indexes are into [`shep_toml_shape`]: a table of its outline, or a
/// field of its field set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    /// A section, which folds.
    Table(usize),
    /// One key, or a map, which folds over its entries.
    Field(usize),
    /// One entry of the map at field `map`, by its place in key order.
    Entry {
        /// The map's field.
        map: usize,
        /// Which entry, in key order.
        index: usize,
    },
    /// The row under a map's entries that adds one.
    AddEntry(usize),
    /// Index into [`SettingsSnapshot::dogs`].
    Dog(usize),
}

/// The settings screen's own state. `None` on [`App`] is the dashboard.
#[derive(Debug, Clone)]
pub struct Settings {
    pub(super) snapshot: SettingsSnapshot,
    /// The dotted keys of the tables and maps the operator folded. Everything
    /// else is open, so a key added to the schema shows up unfolded.
    pub(super) folded: BTreeSet<String>,
    /// The cursor and, once a terminal has said how tall the body is, the
    /// scroll offset. Clamped on every read rather than kept pre-clamped: a
    /// refresh can shrink the dog list out from under a cursor already
    /// sitting past its new end.
    pub(super) view: Viewport,
    /// The edit this screen is showing, or `None`. One field rather than
    /// several `Option`s, so typing, armed and sent cannot overlap on
    /// screen.
    ///
    /// Not the same as the one write in flight. [`Pending::Sent`] eats no
    /// key, so a second edit can be armed and sent over it, and closing the
    /// screen abandons it without cancelling anything: either leaves a
    /// write with no prompt waiting for it. [`Self::resolve`] is what keeps
    /// those answers off the edit that is here now.
    pub(super) pending: Option<Pending>,
}

/// The settings screen's own in-flight edit.
#[derive(Debug, Clone)]
pub(super) enum Pending {
    /// A free-text edit under construction, seeded with the key's on-disk
    /// value.
    Typing {
        /// Where the text goes.
        target: Target,
        /// What the operator has typed so far.
        buffer: TextBox,
    },
    /// Armed: waiting for the operator's `Enter`. Nothing has gone out yet.
    Armed {
        /// The candidate, ready to send.
        edit: SettingEdit,
        /// The question this candidate reads as, rendered once at arm time.
        text: String,
        /// When it was armed. Only an armed edit expires.
        at: Instant,
    },
    /// [`Self::Armed`] for a [`DogEdit`] on a [`SettingsRow::Dog`] row, which
    /// [`App::confirm_setting`] sends through [`Effect::WriteDog`].
    DogArmed {
        /// The candidate toggle, ready to send.
        edit: DogEdit,
        /// The question this candidate reads as, rendered once at arm time.
        text: String,
        /// When it was armed. Only an armed edit expires.
        at: Instant,
    },
    /// [`Effect::WriteSetting`] or [`Effect::WriteDog`] is in flight. Carries
    /// no `edit`: every match site reads the landing message's own copy.
    Sent {
        /// The same rendered question, so the prompt line does not change
        /// wording between the question and its own answer.
        text: String,
        /// The write this is waiting on, by the ticket it went out with.
        /// What [`Settings::resolve`] matches a reply against, so a reply
        /// from a write the screen has moved on from cannot answer this
        /// one.
        ticket: u64,
    },
}

/// What a free-text edit writes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Target {
    /// The value at `path`, parsed as `shape`.
    Value {
        /// The key's segments.
        path: Vec<String>,
        /// What the typed text becomes.
        shape: Shape,
    },
    /// A new entry of the map at `path`, typed as `name=value`.
    NewEntry {
        /// The map's segments.
        path: Vec<String>,
    },
}

impl Target {
    /// What the status bar calls the edit.
    pub(super) fn label(&self) -> String {
        match self {
            Self::Value { path, .. } => dotted(path),
            Self::NewEntry { path } => format!("new {} entry", dotted(path)),
        }
    }

    /// The edit `text` stands for, or why it stands for none.
    pub(super) fn edit(&self, text: &str) -> Result<SettingEdit, &'static str> {
        match self {
            Self::Value { path, .. } if text.is_empty() => {
                Ok(SettingEdit::Unset { path: path.clone() })
            }
            Self::Value { path, shape } => Ok(SettingEdit::Set {
                path: path.clone(),
                shape: *shape,
                value: text.to_owned(),
            }),
            Self::NewEntry { path } => {
                let (name, value) = text.split_once('=').ok_or("type name=value")?;
                let (name, value) = (name.trim(), value.trim());
                if name.is_empty() || value.is_empty() {
                    return Err("type name=value");
                }
                let mut path = path.clone();
                path.push(name.to_owned());
                Ok(SettingEdit::Set {
                    path,
                    shape: Shape::Text,
                    value: value.to_owned(),
                })
            }
        }
    }
}

/// What the settings screen's status line shows for its one in-flight edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsPrompt<'a> {
    /// The confirm sentence: what will change, and what applying it does and
    /// does not do.
    pub text: &'a str,
    /// False while it is a question, true once it has gone out.
    pub sent: bool,
}

impl Settings {
    /// A freshly opened screen, cursor on the first row, every section open.
    pub(super) fn new(snapshot: SettingsSnapshot) -> Self {
        Self {
            snapshot,
            folded: BTreeSet::new(),
            view: Viewport::new(),
            pending: None,
        }
    }

    /// The armed candidate and its prompt, or `None`.
    #[must_use]
    pub fn pending(&self) -> Option<SettingsPrompt<'_>> {
        match &self.pending {
            Some(Pending::Armed { text, .. } | Pending::DogArmed { text, .. }) => {
                Some(SettingsPrompt { text, sent: false })
            }
            Some(Pending::Sent { text, .. }) => Some(SettingsPrompt { text, sent: true }),
            Some(Pending::Typing { .. }) | None => None,
        }
    }

    /// Whether `ticket` names the write this screen is still waiting on,
    /// clearing the prompt when it does.
    ///
    /// A reply whose ticket is not the one [`Pending::Sent`] holds belongs
    /// to a write the screen has moved on from: a second edit armed over
    /// the first, or a screen closed and reopened while the first was still
    /// in flight. Both leave a write in flight with nothing on screen
    /// waiting for it, and neither lets its answer touch the edit that is.
    pub(super) fn resolve(&mut self, ticket: u64) -> bool {
        let mine = matches!(
            self.pending,
            Some(Pending::Sent { ticket: waiting, .. }) if waiting == ticket
        );
        if mine {
            self.pending = None;
        }
        mine
    }

    /// Whether a candidate is waiting on `Enter`: the one state a stray key
    /// (movement, `Escape`, `Settings`, `Refresh`) eats rather than also doing
    /// its ordinary job.
    pub(super) fn is_armed(&self) -> bool {
        matches!(
            self.pending,
            Some(Pending::Armed { .. } | Pending::DogArmed { .. })
        )
    }

    /// The label and buffer of an in-flight free-text edit, or `None`.
    #[must_use]
    pub fn typing(&self) -> Option<(String, &TextBox)> {
        match &self.pending {
            Some(Pending::Typing { target, buffer }) => Some((target.label(), buffer)),
            _ => None,
        }
    }

    /// The next candidate for `field`, or `None` for a key that is not a
    /// cycle: only a boolean and a closed set of names are.
    ///
    /// Advances from a candidate already armed for this field, so a second
    /// `space` walks one step further along the cycle. From nothing armed the
    /// base is what the file says, which for `[style] level` is the file's
    /// value rather than the level in force: cycling the resolved level could
    /// propose a write that changes nothing.
    pub(super) fn next_candidate(&self, field: &Field) -> Option<String> {
        let armed_here = match &self.pending {
            Some(Pending::Armed {
                edit: SettingEdit::Set { value, .. },
                ..
            }) if self.armed_for(field) => Some(value.clone()),
            _ => None,
        };
        let base = armed_here
            .or_else(|| {
                self.snapshot.file_value(&field.key).map(|value| {
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned)
                })
            })
            .or_else(|| {
                field.default_value.as_ref().map(|value| {
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned)
                })
            })
            // The schema carries no default for `[style] level`; `full` is
            // what `style::resolve` falls back to.
            .or_else(|| {
                (field.key == STYLE_LEVEL_KEY).then(|| crate::style::StyleLevel::Full.to_string())
            });
        match &field.kind {
            FieldKind::Bool => Some((base.as_deref() != Some("true")).to_string()),
            FieldKind::Choice(names) => {
                let at = base
                    .and_then(|base| names.iter().position(|name| *name == base))
                    .map_or(0, |index| index + 1);
                names.get(at % names.len().max(1)).cloned()
            }
            _ => None,
        }
    }

    /// Whether the armed candidate is an edit of `field`.
    fn armed_for(&self, field: &Field) -> bool {
        matches!(
            &self.pending,
            Some(Pending::Armed { edit, .. }) if edit.key() == field.key
        )
    }

    /// What the screen reads off disk, and renders every row's value and source
    /// from.
    ///
    /// A landed write does not update this in place: it raises a fresh
    /// [`Effect::LoadSettings`], so `Set` and `Unset` land the same way and
    /// neither can drift from the rest of the document.
    #[must_use]
    pub fn snapshot(&self) -> &SettingsSnapshot {
        &self.snapshot
    }

    /// Every row the cursor can sit on: the sections and their keys in the
    /// order the screen draws them, a map's entries under it, then one row
    /// per candidate dog.
    #[must_use]
    pub fn rows(&self) -> Vec<SettingsRow> {
        let shape = shep_toml_shape();
        let mut rows = Vec::new();
        self.push_children(&shape.outline.roots, &mut rows);
        rows.extend((0..self.snapshot.dogs.len()).map(SettingsRow::Dog));
        rows
    }

    fn push_children(&self, children: &[Child], rows: &mut Vec<SettingsRow>) {
        let shape = shep_toml_shape();
        for child in children {
            match *child {
                Child::Table(index) => {
                    rows.push(SettingsRow::Table(index));
                    let table = &shape.outline.tables[index];
                    if !self.folded.contains(&table.path) {
                        self.push_children(&table.children, rows);
                    }
                }
                Child::Leaf(index) => {
                    rows.push(SettingsRow::Field(index));
                    let field = &shape.fields.fields()[index];
                    if field.kind == FieldKind::Map && !self.folded.contains(&field.key) {
                        let entries = self.snapshot.entries(&field.key).len();
                        rows.extend((0..entries).map(|entry| SettingsRow::Entry {
                            map: index,
                            index: entry,
                        }));
                        rows.push(SettingsRow::AddEntry(index));
                    }
                }
            }
        }
    }

    /// How many rows [`Self::rows`] would build.
    #[must_use]
    pub fn rows_len(&self) -> usize {
        self.rows().len()
    }

    /// The row the cursor sits on. `None` only if [`Self::rows`] is empty,
    /// which the schema's sections make unreachable.
    #[must_use]
    pub fn cursor(&self) -> Option<SettingsRow> {
        let rows = self.rows();
        rows.get(self.view.cursor().min(rows.len().saturating_sub(1)))
            .copied()
    }

    /// Folds the table or map under the cursor if it is open, opens it if it
    /// is folded. Answers whether the cursor was on one.
    pub(super) fn toggle_fold(&mut self) -> bool {
        let shape = shep_toml_shape();
        let key = match self.cursor() {
            Some(SettingsRow::Table(index)) => shape.outline.tables[index].path.clone(),
            Some(SettingsRow::Field(index))
                if shape.fields.fields()[index].kind == FieldKind::Map =>
            {
                shape.fields.fields()[index].key.clone()
            }
            _ => return false,
        };
        if !self.folded.remove(&key) {
            self.folded.insert(key);
        }
        true
    }

    /// Where an editor opened on the cursor's row writes, and what it opens
    /// with. `None` on a row that cycles, folds or is not a key.
    pub(super) fn editor_at_cursor(&self) -> Option<(Target, String)> {
        let shape = shep_toml_shape();
        let fields = shape.fields.fields();
        match self.cursor()? {
            SettingsRow::Field(index) => {
                let field = &fields[index];
                if !field.editable || !is_typed(&field.kind) {
                    return None;
                }
                let path = shape.paths.get(&field.key)?.clone();
                let kind = Shape::of(&field.kind)?;
                Some((
                    Target::Value { path, shape: kind },
                    self.snapshot.seed(field),
                ))
            }
            SettingsRow::Entry { map, index } => {
                let key = &fields[map].key;
                let (name, value) = self.snapshot.entries(key).into_iter().nth(index)?;
                Some((
                    Target::Value {
                        path: vec![key.clone(), name],
                        shape: Shape::Text,
                    },
                    value,
                ))
            }
            SettingsRow::AddEntry(map) => Some((
                Target::NewEntry {
                    path: shape.paths.get(&fields[map].key)?.clone(),
                },
                String::new(),
            )),
            SettingsRow::Table(_) | SettingsRow::Dog(_) => None,
        }
    }

    /// The edit that removes the cursor's row from `shep.toml`, or `None`
    /// where there is nothing to remove: a section, a dog, or a key the file
    /// never wrote.
    pub(super) fn removal_at_cursor(&self) -> Option<SettingEdit> {
        let shape = shep_toml_shape();
        let fields = shape.fields.fields();
        match self.cursor()? {
            SettingsRow::Field(index) => {
                let field = &fields[index];
                self.snapshot.file_value(&field.key)?;
                Some(SettingEdit::Unset {
                    path: shape.paths.get(&field.key)?.clone(),
                })
            }
            SettingsRow::Entry { map, index } => {
                let key = &fields[map].key;
                let (name, _) = self.snapshot.entries(key).into_iter().nth(index)?;
                Some(SettingEdit::Unset {
                    path: vec![key.clone(), name],
                })
            }
            SettingsRow::Table(_) | SettingsRow::AddEntry(_) | SettingsRow::Dog(_) => None,
        }
    }

    /// Whether the table or map at the dotted `key` shows what is inside it.
    #[must_use]
    pub fn is_open(&self, key: &str) -> bool {
        !self.folded.contains(key)
    }

    /// Moves the cursor by `delta` rows, clamped to [`Self::rows`], never
    /// wrapping.
    pub(super) fn move_by(&mut self, delta: isize) {
        let len = self.rows_len();
        self.view.move_by(delta, len);
    }

    pub(super) fn move_to_first(&mut self) {
        let len = self.rows_len();
        self.view.move_to(0, len);
    }

    pub(super) fn move_to_last(&mut self) {
        let len = self.rows_len();
        self.view.move_to(len.saturating_sub(1), len);
    }

    /// The viewport, for the renderer.
    #[must_use]
    pub fn view(&self) -> &Viewport {
        &self.view
    }

    /// Records the terminal's height.
    pub fn set_rows(&mut self, rows: usize) {
        let len = self.rows_len();
        self.view.set_rows(rows, len);
    }
}

/// Whether a key of this kind is typed into an editor, rather than flipped,
/// walked through a closed set, or folded.
fn is_typed(kind: &FieldKind) -> bool {
    matches!(
        kind,
        FieldKind::Integer
            | FieldKind::Number
            | FieldKind::Text
            | FieldKind::Suggested(_)
            | FieldKind::List(_)
    )
}

/// What a refused write reopens the editor with: where the text goes and the
/// text the operator typed. `None` for an edit that had no editor to reopen:
/// a flip, a walk through names, or a removed entry.
pub(super) fn reopened_editor(edit: &SettingEdit) -> Option<(Target, TextBox)> {
    let shape = shep_toml_shape();
    let field = shape.fields.by_key(&edit.key());
    match (edit, field) {
        (
            SettingEdit::Set {
                path,
                shape: kind,
                value,
            },
            Some(field),
        ) if is_typed(&field.kind) => Some((
            Target::Value {
                path: path.clone(),
                shape: *kind,
            },
            TextBox::from(value.as_str()),
        )),
        (
            SettingEdit::Set {
                path,
                shape: kind,
                value,
            },
            None,
        ) if is_entry(path) => Some((
            Target::Value {
                path: path.clone(),
                shape: *kind,
            },
            TextBox::from(value.as_str()),
        )),
        (SettingEdit::Unset { path }, Some(field)) if is_typed(&field.kind) => Some((
            Target::Value {
                path: path.clone(),
                shape: Shape::of(&field.kind)?,
            },
            TextBox::new(),
        )),
        _ => None,
    }
}

/// Whether `path` names one entry of a map.
fn is_entry(path: &[String]) -> bool {
    let [map, _] = path else {
        return false;
    };
    shep_toml_shape()
        .fields
        .by_key(map)
        .is_some_and(|field| field.kind == FieldKind::Map)
}

impl App {
    /// The settings screen's own state, or `None` while the dashboard is
    /// showing: [`Body::settings`] over this app's body.
    #[must_use]
    pub fn settings(&self) -> Option<&Settings> {
        self.body.settings()
    }

    /// [`Self::settings`]'s mutable twin, for the settings keymap and the
    /// handlers that update a field or a pending edit in place.
    ///
    /// A caller that still needs another `App` field while it holds the
    /// screen calls [`Body::settings_mut`] on `self.body` instead, which
    /// borrows no more than the body.
    pub(super) fn settings_mut(&mut self) -> Option<&mut Settings> {
        self.body.settings_mut()
    }

    /// The resolved style level and which layer chose it, which the STYLE LEVEL
    /// row reads rather than re-resolving.
    #[must_use]
    pub fn style(&self) -> (StyleLevel, StyleSource) {
        self.style
    }

    /// Sets the resolved style level and its source. `App` reads no files, so
    /// it cannot resolve this itself.
    pub(crate) fn set_style(&mut self, style: (StyleLevel, StyleSource)) {
        self.style = style;
    }

    /// Marks this lookout as watching the shepherd on `dest` over `--host`.
    ///
    /// From here the screens that read this machine say so instead of opening:
    /// see [`NOT_OVER_HOST`].
    pub(crate) fn set_remote_host(&mut self, dest: &str) {
        self.remote_host = Some(dest.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::field::PER_DOG_KEYS;
    use crate::lookout::view::fixtures;

    /// Every leaf of `shep.toml`'s schema, as a dotted key, walked straight
    /// off the JSON: a section is a property that resolves to an object with
    /// properties, anything else (a map, a list, a scalar) is one leaf.
    fn schema_leaves(node: &serde_json::Value, defs: &serde_json::Value, at: &str) -> Vec<String> {
        let mut leaves = Vec::new();
        let properties = node["properties"].as_object().into_iter().flatten();
        for (name, property) in properties {
            let key = if at.is_empty() {
                name.clone()
            } else {
                format!("{at}.{name}")
            };
            let target = property["$ref"]
                .as_str()
                .and_then(|reference| reference.strip_prefix("#/$defs/"))
                .map_or(property, |def| &defs[def]);
            if target["properties"].is_object() {
                leaves.extend(schema_leaves(target, defs, &key));
            } else {
                leaves.push(key);
            }
        }
        leaves
    }

    /// A key added to `DaemonConfig` shows up in the schema, so this fails
    /// until the screen draws it. The four per-dog keys are the exception:
    /// the dog's own row carries them.
    #[test]
    fn the_tree_has_a_row_for_every_leaf_of_the_schema_but_the_per_dog_keys() {
        let schema = shep_core::config::daemon_config_schema_json().to_value();
        let mut expected = schema_leaves(&schema, &schema["$defs"], "");
        expected.retain(|key| !PER_DOG_KEYS.contains(&key.as_str()));
        expected.sort();

        let app = fixtures::app_in_settings();
        let settings = app.settings().unwrap();
        let fields = crate::lookout::field::shep_toml_shape().fields.fields();
        let mut drawn: Vec<String> = settings
            .rows()
            .into_iter()
            .filter_map(|row| match row {
                SettingsRow::Field(index) => Some(fields[index].key.clone()),
                _ => None,
            })
            .collect();
        drawn.sort();

        assert!(
            expected.len() > 10,
            "the walk found the schema: {expected:?}"
        );
        assert_eq!(drawn, expected);
    }

    #[test]
    fn set_style_round_trips_exactly() {
        let mut app = fixtures::full_app();
        assert_eq!(
            app.style(),
            (StyleLevel::Full, StyleSource::Default),
            "the default before anyone calls set_style"
        );
        app.set_style((StyleLevel::Bare, StyleSource::Flag));
        assert_eq!(app.style(), (StyleLevel::Bare, StyleSource::Flag));
    }

    /// Against a real file whose `[style] level` names a third, different
    /// level: the row reports the value threaded onto `App`, not one re-derived
    /// from the file.
    #[test]
    fn the_style_set_on_the_app_reaches_the_settings_row_undropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shep.toml");
        std::fs::write(&path, "[style]\nlevel = \"bare\"\n").unwrap();
        let socket_default = dir.path().join("run").join("shep.sock");

        let mut app = fixtures::full_app();
        app.set_style((StyleLevel::Plain, StyleSource::Flag));

        let result = crate::commands::settings::load_settings(&path, &socket_default, app.style())
            .map_err(|err| err.to_string());
        let _ = app.update(Msg::Settings { result });

        let field = crate::lookout::field::shep_toml_shape()
            .fields
            .by_key("style.level")
            .unwrap();
        let row = app.settings().unwrap().snapshot().view(field);
        assert_eq!(
            row.source,
            StyleSource::Flag,
            "the flag beats the file rather than being dropped by it"
        );
        assert_eq!(row.value, "plain");
    }
}
