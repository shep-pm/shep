//! The pane itself: what it is over, what it reads off that target, and
//! where the cursor is.
//!
//! One `FieldSet`, the values the target currently holds, and a `Viewport`
//! over the rows. Everything here answers a question; the answers to
//! keystrokes live in the four modules beside it.

use std::path::PathBuf;

use serde_json::{Map, Value};
use shep_core::config::{ApplyGroup, GROUP_ORDER, apply_group, reaches_running};
use shep_core::protocol::SheepConfigView;

use super::super::edits::{EditKey, Edits};
use super::super::field::{FieldKind, FieldSet};
use super::super::viewport::Viewport;
use super::fields::{render_json, resolved_display, sheep_fields};

// Link-only (IR-32): the unit grammars a row's resolved display goes
// through, and the field kind a locked row reports.
#[cfg(doc)]
use super::super::field::ValueKind;
use super::{EnvTyping, ListPane, Lock, PaneEdit, PaneRow, PaneTarget, PaneTyping};
#[cfg(doc)]
use shep_core::values::{MemSize, UpDuration};

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
    /// `Escape` closes whichever is up before the pane.
    pub(super) list: Option<ListPane>,
    /// The dog's `[<name>]` table as TOML text, and [`None`] for a sheep.
    ///
    /// Kept beside the parsed `values` rather than instead of them, because
    /// the two answer different questions: `values` is what the rows render,
    /// and this is what a write edits. `Request::SetDogConfig` replaces the
    /// whole section, so an edit that re-rendered it from `values` would
    /// throw away every comment the operator wrote. See
    /// [`Self::edited_section_with`].
    pub(super) section: Option<String>,
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
            section: Some(section),
        }
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
        Some(resolved_display(&self.fields, key, &raw))
    }

    /// [`Self::value`], resolved through its own grammar for a
    /// [`MemSize`]/[`UpDuration`] field: what a row draws instead of the
    /// digits an operator would otherwise have to already know the
    /// convention for.
    #[must_use]
    pub fn display_value(&self, key: &str) -> String {
        resolved_display(&self.fields, key, &self.value(key))
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
    pub(in crate::lookout) fn field_rows(&self) -> Vec<PaneRow> {
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

    pub(in crate::lookout) fn move_by(&mut self, delta: isize) {
        let len = self.rows().len();
        self.view.move_by(delta, len);
    }

    pub(in crate::lookout) fn move_to_first(&mut self) {
        let len = self.rows().len();
        self.view.move_to(0, len);
    }

    pub(in crate::lookout) fn move_to_last(&mut self) {
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
    pub(in crate::lookout) fn adopt_edits(&mut self, previous: Edits) {
        self.edits = previous;
    }

    /// Adopts a previous pane's cursor and offset, clamped to this one's
    /// own row count. What a refresh of an already-open pane rides on, so
    /// `r` does not throw the operator back to the first field.
    pub(in crate::lookout) fn adopt_view(&mut self, view: Viewport) {
        self.view = view;
        let len = self.rows().len();
        self.view.clamp(len);
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
    use super::super::fixtures::{bark_pane, web, web_with_args};
    use shep_core::values::{MemSize, UpDuration};

    use shep_core::config::{AppConfig, ProbeConfig, ProbeKind};

    use super::*;

    #[test]
    fn a_sheep_pane_has_forty_two_fields_in_eight_groups() {
        let pane = ConfigPane::sheep(web());
        assert_eq!(pane.fields().len(), 42);
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
            r#"ConfigPane { target: Sheep { name: "web" }, fields: 42, env_keys: 1, cursor: 0 }"#
        );
    }
}
