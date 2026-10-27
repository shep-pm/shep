//! The pane itself: what it is over, and what it reads off that target.
//!
//! One `FieldSet`, the values the target currently holds, and a `Viewport`
//! over the rows. Everything here answers a question; the answers to
//! keystrokes live in the modules beside it, and the rows themselves and the
//! cursor's walk through them in [`super::walk`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Map, Value};
use shep_core::config::{ApplyGroup, apply_group, reaches_running};
use shep_core::protocol::SheepConfigView;

use super::super::edits::{EditKey, Edits};
use super::super::field::{
    Field, FieldKind, FieldSet, Flattened, Outline, flatten_values, flattened,
};
use super::super::viewport::Viewport;
use super::entries::{EntryBody, EntryRows};
use super::entry_remove::Reshape;
use super::fields::{entries_text, render_json, resolved_by_kind, resolved_display, sheep_fields};
use super::sheep_dog::SheepDogTable;

// Link-only (IR-32): the unit grammars a row's resolved display goes
// through, the field kind a locked row reports, and the group order this
// type's own field indexes into.
#[cfg(doc)]
use super::super::field::ValueKind;
use super::dogs::{DogsTab, dog_names};
use super::{EnvTyping, ListPane, Lock, PaneEdit, PaneRow, PaneTarget, PaneTyping, PathStep};
#[cfg(doc)]
use shep_core::config::GROUP_ORDER;
#[cfg(doc)]
use shep_core::values::{MemSize, UpDuration};

/// One dog's own table and the schema its rows were built from: the two
/// halves of what `Request::SetDogConfig` replaces.
///
/// `Debug` is manual and redacted (IR-41): a schema can carry a `default`
/// that is a token, and a section is the operator's text, comments and all.
/// Neither is printed, which is the reason [`crate::lookout::pane::SheepDogEntry`]'s
/// `Debug` withholds its schema too.
#[derive(Clone)]
pub(super) struct DogSection {
    /// The `[<name>]` section as the shepherd last sent it.
    pub(super) toml: String,
    /// The dog's answer to the schema flag, probed at open and kept nowhere
    /// else.
    pub(super) schema: Value,
}

impl core::fmt::Debug for DogSection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "DogSection {{ <section>, <schema> }}")
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
    pub(super) target: PaneTarget,
    pub(super) fields: FieldSet,
    pub(super) values: Map<String, Value>,
    pub(super) env_keys: Vec<String>,
    pub(super) overridden: Vec<String>,
    /// Field names parked until the next respawn, as the shepherd reported
    /// them. Nothing to do with [`Self::edits`], which is this pane's own
    /// set of unwritten changes; the two words come from opposite ends and
    /// the collision is the shepherd's.
    pub(super) pending: Vec<String>,
    /// Index into [`GROUP_ORDER`]: which group's fields the field list
    /// draws. Meaningless for a dog pane, whose fields carry no group and
    /// so are visible under any of them; see [`Self::rows`].
    pub(super) group: usize,
    pub(super) view: Viewport,
    /// The open text editor, or [`None`].
    pub(super) typing: Option<PaneTyping>,
    /// Everything the operator has changed and nothing has written yet.
    /// Emptied by [`Self::close`], which is the only door out.
    pub(super) edits: Edits,
    /// The open env editor, or [`None`]. Never open at the same time as
    /// [`Self::list`]: each opens on a row of its own kind, and `Escape`
    /// closes whichever is up before the pane.
    pub(super) env_typing: Option<EnvTyping>,
    /// The open list sub-screen. Never open at the same time as
    /// [`Self::env_typing`]: each opens on a row of its own kind, and
    /// `Escape` closes whichever is up before the pane. `Box`: one is open
    /// at a time, and inline it would put a whole sub-screen in every move
    /// of the pane.
    pub(super) list: Option<Box<ListPane>>,
    /// The dogs a sheep's `dogs` tab lists, and the tables opened under
    /// them. Empty for any other target.
    pub(super) dogs_tab: Box<DogsTab>,
    /// The dog's `[<name>]` table as TOML text, with the schema its rows
    /// were built from, and [`None`] for a sheep.
    ///
    /// Kept beside the parsed `values` rather than instead of them, because
    /// the two answer different questions: `values` is what the rows render,
    /// and this is what a write edits. `Request::SetDogConfig` replaces the
    /// whole section, so an edit that re-rendered it from `values` would
    /// throw away every comment the operator wrote. See
    /// [`Self::edited_section_with`].
    ///
    /// The schema rides along in the same box because it is the other half
    /// of the same question: it is what says a proposed section may go out,
    /// and see [`crate::dog_settings::touched_refusal`]. Boxed, like
    /// [`Self::dog_table`]: a `Value` this size does not belong in every move
    /// of a pane that sits in [`crate::lookout::app::Body`] unboxed.
    pub(super) section: Option<Box<DogSection>>,
    /// A dog's table on a sheep, whole, and where each dotted row lives in
    /// it. [`None`] for every other target. See [`Self::sheep_dog`].
    pub(super) dog_table: Option<Box<SheepDogTable>>,
    /// Each dotted row key of a dog pane to the real property path it was
    /// built from. Empty for any other target.
    ///
    /// The rows are [`flattened`]'s, so a key is dotted for a nested table
    /// and quoted for a property literally named with a dot. The write walks
    /// this map rather than the key: see [`Self::edited_section_with`].
    /// Shared, so the pane stays small enough to sit in `Body` unboxed.
    pub(super) section_paths: Arc<BTreeMap<String, Vec<String>>>,
    /// The tables a dog pane's rows nest in. Empty for a sheep. Shared, so
    /// the pane stays small enough to sit in `Body` unboxed.
    pub(super) outline: Arc<Outline>,
    /// The rows under each array or map of tables. Empty for a sheep.
    pub(super) entries: Arc<EntryRows>,
    /// The tables currently drawn open, by their dotted path. See
    /// [`Self::rows`].
    pub(super) open: BTreeSet<String>,
    /// What each entry removal or addition replaced in the set, newest last,
    /// so `u` can hand it back. See [`Self::file_whole`]. Shared, like
    /// [`Self::entries`], so the pane stays small enough to sit in `Body`.
    pub(super) reshapes: Arc<Vec<Reshape>>,
    /// The rows under each union of a dog's table on a sheep, as editor
    /// fields keyed by the name their edits file under. Empty for any other
    /// target. See [`super::union`].
    pub(super) arms: Arc<BTreeMap<String, Field>>,
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
        let (fields, values) = sheep_fields(&view.config);
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
            dogs_tab: Box::default(),
            section: None,
            dog_table: None,
            reshapes: Arc::default(),
            arms: Arc::default(),
            section_paths: Arc::default(),
            outline: Arc::new(Outline::default()),
            entries: Arc::default(),
            open: BTreeSet::new(),
        }
    }

    /// A pane over one dog's `[<name>]` section.
    ///
    /// `schema` is the dog's own answer to the schema flag, probed at
    /// open rather than read from anywhere; `section` is the table as
    /// `Request::DogConfig` rendered it, empty when `dogs.toml` has none.
    ///
    /// In schema order, with no group headers: a dog's schema carries no
    /// `init.group`. A nested table is a row that opens and closes, with its
    /// keys under it, as it is on that dog's table over a sheep; see
    /// [`Self::rows`]. A [`FieldKind::List`] row opens the list sub-screen,
    /// as a sheep's `args` does. A [`FieldKind::Map`] row, and a list
    /// [`flattened`] closed to [`FieldKind::Opaque`], is marked not editable
    /// and draws [`Lock::NoWidget`].
    #[must_use]
    pub fn dog(
        name: String,
        adopted_path: Option<PathBuf>,
        schema: Value,
        section: String,
    ) -> Self {
        let Flattened {
            fields,
            paths,
            outline,
            shapes,
        } = flattened(&schema);
        let fields = FieldSet::from_fields(
            fields
                .fields()
                .iter()
                .cloned()
                .map(|mut field| {
                    if field.kind == FieldKind::Map {
                        field.editable = false;
                    }
                    field
                })
                .collect(),
            &[],
        );
        let table: Map<String, Value> = toml_table(&section);
        let values = flatten_values(&table, &paths);
        let entries = EntryRows::build(&fields, &paths, &values, shapes);
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
            dogs_tab: Box::default(),
            section: Some(Box::new(DogSection {
                toml: section,
                schema,
            })),
            dog_table: None,
            reshapes: Arc::default(),
            arms: Arc::default(),
            section_paths: Arc::new(paths),
            open: outline.top_level_paths(),
            outline: Arc::new(outline),
            entries: Arc::new(entries),
        }
    }

    /// The schema these rows were built from, for the gate a write goes
    /// through. See [`crate::dog_settings::touched_refusal`].
    ///
    /// [`None`] for a sheep pane, whose write is one field per request and
    /// whose schema is the Flockfile's, and for a dog the shepherd never
    /// answered a schema for.
    #[must_use]
    pub(in crate::lookout) fn schema(&self) -> Option<&Value> {
        self.section
            .as_ref()
            .map(|held| &held.schema)
            .or_else(|| self.dog_table.as_ref().map(|table| &table.schema))
    }

    /// The dog's section as the shepherd last sent it, and [`None`] for any
    /// other target.
    #[must_use]
    pub(in crate::lookout) fn section_text(&self) -> Option<&str> {
        self.section.as_ref().map(|held| held.toml.as_str())
    }

    /// Where the field edits in `edits` land in the table a write carries, as
    /// property paths, for [`crate::dog_settings::touched_refusal`].
    ///
    /// The same walk [`Self::write_path`] does on the way into a dog section:
    /// an entry leaf at its own index, every other row at the path
    /// [`flattened`] built its key out of. A key this pane holds no path for
    /// names nothing the write changes, so it names nothing to check.
    #[must_use]
    pub(in crate::lookout) fn touched_paths(&self, edits: &Edits) -> BTreeSet<Vec<String>> {
        let mut paths = BTreeSet::new();
        for (key, _) in edits.iter() {
            let EditKey::Field(row) = key else {
                continue;
            };
            if let Some(leaf) = self.entries.leaf(row) {
                let path: Vec<String> = leaf
                    .at
                    .iter()
                    .map(|step| match step {
                        PathStep::Key(name) => name.clone(),
                        PathStep::Index(index) => index.to_string(),
                    })
                    .collect();
                paths.insert(path);
            } else if let Some(path) = self.section_paths.get(row).or_else(|| {
                self.dog_table
                    .as_ref()
                    .and_then(|table| table.paths.get(row))
            }) {
                paths.insert(path.clone());
            }
        }
        paths
    }

    /// Whether `section` is the text this dog pane last took from the
    /// shepherd, comments included. False for any other target.
    #[must_use]
    pub(in crate::lookout) fn holds_section(&self, section: &str) -> bool {
        self.section_text() == Some(section)
    }

    /// The key under the cursor, and why the pane will not edit it, when it
    /// will not. [`None`] both for a row that edits and for no row at all.
    ///
    /// The one place a caller asks "may I edit what is selected", so a
    /// refusal is raised for the right one of [`Lock`]'s two reasons rather
    /// than for a generic third.
    #[must_use]
    pub fn cursor_lock(&self) -> Option<(&str, Lock)> {
        let field = match self.cursor()? {
            PaneRow::Field(index) => self.fields.fields().get(index)?,
            PaneRow::Entry(index) => match &self.entries.row(index)?.body {
                EntryBody::Leaf { field, .. } => field,
                EntryBody::Node { .. } | EntryBody::New { .. } => return None,
            },
            PaneRow::Choice { field, .. } => self.fields.fields().get(field)?,
            PaneRow::Arm { field, .. } => self.fields.fields().get(field)?,
            PaneRow::Table(_)
            | PaneRow::Env(_)
            | PaneRow::AddEnv
            | PaneRow::Dog(_)
            | PaneRow::Under { .. } => return None,
        };
        if let Some(arm) = self.arm_field(self.cursor()?) {
            return self.lock(&arm.key).map(|lock| (field.key.as_str(), lock));
        }
        self.lock(&field.key).map(|lock| (field.key.as_str(), lock))
    }

    /// The field under the cursor, for a row that holds a value: a config
    /// field, or a field inside an entry, whose key is its full path.
    pub(in crate::lookout) fn cursor_field(&self) -> Option<&Field> {
        match self.cursor()? {
            PaneRow::Field(index) => self.fields.fields().get(index),
            PaneRow::Entry(index) => match &self.entries.row(index)?.body {
                EntryBody::Leaf { field, .. } => Some(field),
                EntryBody::Node { .. } | EntryBody::New { .. } => None,
            },
            PaneRow::Arm { .. } => self.arm_field(self.cursor()?),
            PaneRow::Table(_)
            | PaneRow::Choice { .. }
            | PaneRow::Env(_)
            | PaneRow::AddEnv
            | PaneRow::Dog(_)
            | PaneRow::Under { .. } => None,
        }
    }

    /// The field filed under `key`: one of [`Self::fields`], or a field
    /// inside an entry.
    pub(super) fn field_of(&self, key: &str) -> Option<&Field> {
        self.fields
            .by_key(key)
            .or_else(|| self.entries.leaf(key).map(|leaf| leaf.field))
            .or_else(|| self.arm_field_of(key))
    }

    /// What the target holds for `key`, or [`None`] for a key it holds
    /// nothing for. A field inside an entry reads [`Value::Null`] when the
    /// entry lacks it.
    pub(super) fn stored_of(&self, key: &str) -> Option<&Value> {
        self.values
            .get(key)
            .or_else(|| self.entries.leaf(key).map(|leaf| leaf.stored))
            .or_else(|| self.arm_value(key))
    }

    /// The kind of widget the row under the cursor wants, or [`None`] for
    /// no row at all, or for one on an env row, which has no [`FieldKind`].
    #[must_use]
    pub fn cursor_kind(&self) -> Option<&FieldKind> {
        match self.cursor()? {
            PaneRow::Field(index) => self.fields.fields().get(index).map(|field| &field.kind),
            PaneRow::Entry(index) => match &self.entries.row(index)?.body {
                EntryBody::Leaf { field, .. } => Some(&field.kind),
                EntryBody::Node { .. } | EntryBody::New { .. } => None,
            },
            PaneRow::Arm { .. } => self.arm_field(self.cursor()?).map(|field| &field.kind),
            PaneRow::Table(_)
            | PaneRow::Choice { .. }
            | PaneRow::Env(_)
            | PaneRow::AddEnv
            | PaneRow::Dog(_)
            | PaneRow::Under { .. } => None,
        }
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
    /// anything else shows compact JSON. On a dog's pane an array or map of
    /// tables shows how many entries it holds and nothing of them, since an
    /// entry can hold a secret. A sheep's `env` is the one field
    /// whose value this pane never holds, since the shepherd strips it on
    /// the way out, so it shows its key count instead, and the sub-screen
    /// shows the names. A sheep's `dogs` shows dog names and no value: a
    /// table's secrets are marked only by a schema this pane does not hold.
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
        if key == "dogs" && matches!(self.target, PaneTarget::Sheep { .. }) {
            return dog_names(self.values.get("dogs"));
        }
        if !matches!(self.target, PaneTarget::Sheep { .. })
            && let Some(FieldKind::Entries(_)) = self.fields.by_key(key).map(|field| &field.kind)
        {
            return entries_text(self.values.get(key));
        }
        if let Some(text) = self.union_text(key).or_else(|| self.arm_text(key)) {
            return text;
        }
        if let Some(text) = self.checklist_text(key, self.values.get(key)) {
            return text;
        }
        if let Some(EntryBody::Leaf { text, .. }) = self.entries.leaf_body(key) {
            return text.clone();
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
        if let Some(text) = self.union_text(key) {
            return Some(text);
        }
        let raw = if self.entries_field(key).is_some() {
            entries_text(Some(value.as_value()))
        } else {
            self.checklist_text(key, Some(value.as_value()))
                .unwrap_or_else(|| render_json(value.as_value()))
        };
        Some(self.resolved(key, &raw))
    }

    /// [`Self::value`], resolved through its own grammar for a
    /// [`MemSize`]/[`UpDuration`] field: what a row draws instead of the
    /// digits an operator would otherwise have to already know the
    /// convention for.
    #[must_use]
    pub fn display_value(&self, key: &str) -> String {
        self.resolved(key, &self.value(key))
    }

    fn resolved(&self, key: &str, raw: &str) -> String {
        match (self.entries.leaf(key), self.arm_field_of(key)) {
            (Some(leaf), _) => resolved_by_kind(leaf.field.value_kind, raw),
            (None, Some(arm)) => resolved_by_kind(arm.value_kind, raw),
            (None, None) => resolved_display(&self.fields, key, raw),
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
            PaneTarget::Dog { .. } | PaneTarget::SheepDog { .. } => None,
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
        if let Some(place) = self.place(key) {
            return self.lock(place.union()).or_else(|| {
                self.arm_field_of(key)
                    .is_none_or(|field| !field.editable)
                    .then_some(Lock::NoWidget)
            });
        }
        if let Some(leaf) = self.entries.leaf(key) {
            let edits_here = matches!(self.target, PaneTarget::Dog { .. })
                && matches!(
                    leaf.field.kind,
                    FieldKind::Bool
                        | FieldKind::Integer
                        | FieldKind::Number
                        | FieldKind::Text
                        | FieldKind::Choice(_)
                        | FieldKind::Suggested(_)
                );
            return (!edits_here || !leaf.field.editable).then_some(Lock::NoWidget);
        }
        match self.fields.by_key(key) {
            Some(field) if !field.editable => Some(Lock::NoWidget),
            Some(field)
                if matches!(field.kind, FieldKind::Union(_))
                    && !matches!(self.target, PaneTarget::SheepDog { .. }) =>
            {
                Some(Lock::NoWidget)
            }
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

    /// The filed edits a respawn is what applies, by field name, in the
    /// set's own key order.
    ///
    /// An env key counts: `env` is `ApplyGroup::NeedsRespawn` and every
    /// value is baked into the child at exec.
    #[must_use]
    pub(in crate::lookout) fn unsent_fields_needing_a_respawn(&self) -> Vec<String> {
        self.edits
            .iter()
            .filter_map(|(key, _)| match key {
                EditKey::Field(name) if !reaches_running(name) => Some(name.clone()),
                EditKey::Env(name) => Some(format!("env {name}")),
                EditKey::Field(_) => None,
            })
            .collect()
    }

    /// How many filed edits the running sheep already takes without a
    /// respawn: the complement of [`Self::unsent_fields_needing_a_respawn`]
    /// within the same set. An env key never counts, for the same reason it
    /// always counts on the other side: `env` is `ApplyGroup::NeedsRespawn`.
    ///
    /// What the close dialog's "everything else you changed is already
    /// live" sentence draws on: a filed set holding only fields a respawn
    /// applies has nothing else to say that about.
    #[must_use]
    pub(in crate::lookout) fn live_edit_count(&self) -> usize {
        self.edits
            .iter()
            .filter(|(key, _)| matches!(key, EditKey::Field(name) if reaches_running(name)))
            .count()
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
        let readiness = if flag("wait_ready") {
            ConfiguredReadiness::Channel
        } else if has_probe {
            ConfiguredReadiness::Probe
        } else {
            ConfiguredReadiness::Heuristic
        };
        reload_mode(readiness, flag("reuse_port"))
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
    pub(in crate::lookout) fn adopt_edits(&mut self, previous: Edits) {
        self.edits = previous;
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

/// Which readiness signal the pane's values ask for, in the daemon's order.
///
/// The daemon reaches the same three-way choice through
/// `ReadinessSource::of`, which answers for `wait_ready` before it reads
/// `readiness_probe`. The pane holds the kind alone: it never parses a
/// probe's target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfiguredReadiness {
    /// `wait_ready = true`: the shepherd channel's `{"kind":"ready"}`.
    Channel,
    /// `readiness_probe` is set, and `wait_ready` did not answer first.
    Probe,
    /// Neither is configured: readiness is the deadline elapsing.
    Heuristic,
}

/// Whether a reload of this app overlaps or runs serially.
///
/// The daemon's own `ReloadMode::of` reads the same two facts the same way:
/// only a `Probe` is answerable by the instance being replaced, so only a
/// `Probe` serialises, and `reuse_port` spares even that app the
/// `EADDRINUSE` an overlap would take.
pub(super) const fn reload_mode(readiness: ConfiguredReadiness, reuse_port: bool) -> ReloadKind {
    match (readiness, reuse_port) {
        (ConfiguredReadiness::Probe, false) => ReloadKind::Serial,
        _ => ReloadKind::Overlap,
    }
}

/// A dog's section text as the JSON table [`crate::dog_settings::touched_refusal`]
/// reads, or an empty one for text that does not parse.
///
/// The same conversion the pane runs on the section it opens on, so the table
/// a write is checked against is built the way the table the pane's rows were
/// read out of is. A section that does not parse reads as empty and refuses
/// nothing: [`crate::lookout::pane::SectionWrite::Refused`] is what stops that
/// write, in the sentence
/// that names the file.
pub(in crate::lookout) fn toml_table(text: &str) -> Map<String, Value> {
    text.parse::<toml::Table>()
        .ok()
        .and_then(|table| serde_json::to_value(table).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}
