//! The config pane's pending change set.
//!
//! Nothing leaves the pane until it closes, so the set is what the write
//! is built from and what the close dialog asks about. It lives in its own
//! module because two other panes read it and `pane.rs` is long enough.

use std::collections::BTreeMap;

use shep_core::config::ApplyGroup;

use super::pane::PaneEdit;

/// Which field or env key an entry is filed under.
///
/// Two arms rather than one string, because a config field key and an env
/// key are both a `String` and `env` is itself an `AppConfig` field name.
/// `Field` sorts first, so the two never interleave.
///
/// Derived from the [`PaneEdit`] on the way in rather than passed
/// separately, so a `Field` value can never be filed under an `Env` key.
///
/// `Debug` is derived (IR-41): a key name, never a value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum EditKey {
    /// A config field, by its Flockfile name.
    Field(String),
    /// An env key, by its name.
    Env(String),
}

impl EditKey {
    /// Where `edit` files.
    fn of(edit: &PaneEdit) -> Self {
        match edit {
            PaneEdit::Set { key, .. } => Self::Field(key.clone()),
            PaneEdit::SetEnv { key, .. } => Self::Env(key.clone()),
        }
    }
}

/// One pending change: the write it will become, and what sending it costs.
///
/// [`PaneEdit`] rather than a value type of this module's own, because it
/// is already the shape a write takes and already carries both a config
/// field's [`FieldValue`](super::pane::FieldValue) and an env key's
/// [`EnvValue`](super::pane::EnvValue). A second enum beside it would say
/// the same thing twice and make a mismatched pair expressible.
///
/// `Debug` is derived, safe because `PaneEdit`'s own `Debug` withholds
/// both value types (IR-41).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    edit: PaneEdit,
    impact: Option<ApplyGroup>,
}

impl Edit {
    /// The write this entry becomes.
    #[must_use]
    pub const fn edit(&self) -> &PaneEdit {
        &self.edit
    }

    /// What sending it costs, and [`None`] for a dog, which has no
    /// `apply_group` table.
    #[must_use]
    pub const fn impact(&self) -> Option<ApplyGroup> {
        self.impact
    }
}

/// The pane's whole pending change set.
///
/// Two collections rather than one: `entries` gives render order, which is
/// by key, and `order` gives undo order, which is by when it was touched.
/// The same shape `super::pane_bleats::Filters` uses so `esc` can drop its
/// newest chip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Edits {
    entries: BTreeMap<EditKey, Edit>,
    order: Vec<EditKey>,
}

impl Edits {
    /// Files an edit, replacing any entry already under its key and moving
    /// it to the end of the undo order.
    pub fn set(&mut self, edit: PaneEdit, impact: Option<ApplyGroup>) {
        let key = EditKey::of(&edit);
        self.order.retain(|filed| filed != &key);
        self.order.push(key.clone());
        self.entries.insert(key, Edit { edit, impact });
    }

    /// Drops the entry under `key`, if there is one.
    ///
    /// This is what an edit back to the stored value calls: an entry that
    /// changes nothing would still be counted by the title band and still
    /// be asked about on close.
    pub fn remove(&mut self, key: &EditKey) {
        self.entries.remove(key);
        self.order.retain(|filed| filed != key);
    }

    /// Drops the most recently touched entry and names it.
    ///
    /// Nothing is restored: the row falls back to the stored value it was
    /// already reading, so there is no previous value to keep in sync.
    pub fn undo(&mut self) -> Option<EditKey> {
        let key = self.order.pop()?;
        self.entries.remove(&key);
        Some(key)
    }

    /// The entry under `key`.
    #[must_use]
    pub fn get(&self, key: &EditKey) -> Option<&Edit> {
        self.entries.get(key)
    }

    /// How many entries are filed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is filed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every entry, by key.
    pub fn iter(&self) -> impl Iterator<Item = (&EditKey, &Edit)> {
        self.entries.iter()
    }

    /// The whole set as wire edits, by key.
    ///
    /// This is what the pane sends when it closes. Consuming, because a set
    /// that has been written is not a set that is still pending.
    #[must_use]
    pub fn into_writes(self) -> Vec<PaneEdit> {
        self.entries.into_values().map(|entry| entry.edit).collect()
    }

    /// The heaviest impact in the set, and [`None`] when the set is empty
    /// or nothing in it is classified.
    ///
    /// This is what decides whether the close dialog appears at all.
    ///
    /// [`ApplyGroup::Structural`] cannot appear, and nothing in this
    /// module is what stops it. `ConfigPane::sheep` marks every
    /// Structural field not editable, `ConfigPane::lock` answers
    /// [`Lock::Refused`](super::pane::Lock::Refused) for them, and every
    /// door that files an edit checks that lock first. `pane.rs`'s
    /// `no_key_files_an_edit_for_a_structural_field` is what holds the
    /// claim up.
    #[must_use]
    pub fn worst_impact(&self) -> Option<ApplyGroup> {
        self.entries
            .values()
            .filter_map(Edit::impact)
            .max_by_key(|group| rank(*group))
    }
}

/// How heavy an apply group is, for [`Edits::worst_impact`].
///
/// A local ordering over one notion of cost rather than a second notion of
/// it: [`ApplyGroup`] is `#[non_exhaustive]` and derives no `Ord`, and a
/// total order asserted in shep-core would claim more than this needs.
const fn rank(group: ApplyGroup) -> u8 {
    match group {
        ApplyGroup::Live => 0,
        ApplyGroup::NextSpawn => 1,
        // An unknown group answers with the conservative rank, matching
        // `apply_group`'s own fallback.
        ApplyGroup::NeedsRespawn | ApplyGroup::Structural | _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use shep_core::protocol::EnvValue;

    use super::super::pane::FieldValue;

    fn field(key: &str, value: serde_json::Value) -> PaneEdit {
        PaneEdit::Set {
            key: key.to_owned(),
            value: FieldValue::from(value),
        }
    }

    fn env(key: &str, value: Option<EnvValue>) -> PaneEdit {
        PaneEdit::SetEnv {
            key: key.to_owned(),
            value,
        }
    }

    #[test]
    fn an_edit_is_readable_by_the_key_it_was_filed_under() {
        let mut edits = Edits::default();
        edits.set(
            field("cwd", json!("/srv/app")),
            Some(ApplyGroup::NeedsRespawn),
        );
        edits.set(field("autostart", json!(true)), Some(ApplyGroup::NextSpawn));
        assert_eq!(edits.len(), 2);
        // Two entries, filed under different keys and carrying different
        // impacts. One entry could not tell a key-directed lookup from a
        // lookup that hands back whichever entry it reaches first, since
        // the only entry present was the one being asked for.
        let cwd = edits
            .get(&EditKey::Field("cwd".to_owned()))
            .expect("entry filed under cwd");
        assert_eq!(cwd.impact(), Some(ApplyGroup::NeedsRespawn));
        match cwd.edit() {
            PaneEdit::Set { key, .. } => assert_eq!(key, "cwd"),
            other => panic!("expected a Set edit, got {other:?}"),
        }
        let autostart = edits
            .get(&EditKey::Field("autostart".to_owned()))
            .expect("entry filed under autostart");
        assert_eq!(autostart.impact(), Some(ApplyGroup::NextSpawn));
        match autostart.edit() {
            PaneEdit::Set { key, .. } => assert_eq!(key, "autostart"),
            other => panic!("expected a Set edit, got {other:?}"),
        }
    }

    #[test]
    fn a_field_key_and_an_env_key_of_the_same_name_are_two_entries() {
        let mut edits = Edits::default();
        edits.set(field("env", json!({})), None);
        edits.set(env("env", None), None);
        assert_eq!(edits.len(), 2);
    }

    #[test]
    fn worst_impact_is_the_heaviest_in_the_set() {
        let mut edits = Edits::default();
        edits.set(field("max_restarts", json!(4)), Some(ApplyGroup::Live));
        edits.set(
            field("script", json!("a.js")),
            Some(ApplyGroup::NeedsRespawn),
        );
        edits.set(field("autostart", json!(true)), Some(ApplyGroup::NextSpawn));
        assert_eq!(edits.worst_impact(), Some(ApplyGroup::NeedsRespawn));
    }

    #[test]
    fn worst_impact_of_an_empty_set_is_none() {
        assert_eq!(Edits::default().worst_impact(), None);
    }

    /// A dog carries no apply table, so its edits report no impact at all
    /// and must not be read as `Live`.
    #[test]
    fn an_unclassified_edit_does_not_become_the_lightest_impact() {
        let mut edits = Edits::default();
        edits.set(field("url", json!("http://x")), None);
        assert_eq!(edits.worst_impact(), None);
    }

    #[test]
    fn undo_pops_the_most_recently_touched_key() {
        let mut edits = Edits::default();
        edits.set(field("cwd", json!("/a")), None);
        edits.set(field("script", json!("b.js")), None);
        assert_eq!(edits.undo(), Some(EditKey::Field("script".to_owned())));
        assert_eq!(edits.len(), 1);
    }

    /// Re-editing moves a key to newest, so `u` undoes what was last
    /// touched rather than what was first touched.
    #[test]
    fn re_editing_a_field_moves_it_to_the_end_of_the_undo_order() {
        let mut edits = Edits::default();
        edits.set(field("cwd", json!("/a")), None);
        edits.set(field("script", json!("b.js")), None);
        edits.set(field("cwd", json!("/b")), None);
        // Walked to exhaustion on purpose. Asserting only the first pop
        // cannot tell "moved to the end" from "pushed again and happens to
        // be last", and the second of those leaves a stale key that a
        // third undo would report having undone.
        assert_eq!(edits.undo(), Some(EditKey::Field("cwd".to_owned())));
        assert_eq!(edits.undo(), Some(EditKey::Field("script".to_owned())));
        assert_eq!(edits.undo(), None);
    }

    #[test]
    fn undo_on_an_empty_set_reports_nothing_and_does_not_panic() {
        assert_eq!(Edits::default().undo(), None);
    }

    #[test]
    fn remove_drops_an_entry_and_its_place_in_the_undo_order() {
        let mut edits = Edits::default();
        edits.set(field("cwd", json!("/a")), None);
        edits.set(field("script", json!("b.js")), None);
        edits.remove(&EditKey::Field("script".to_owned()));
        assert_eq!(edits.undo(), Some(EditKey::Field("cwd".to_owned())));
        assert!(edits.is_empty());
    }

    #[test]
    fn iteration_is_by_key_not_by_undo_order() {
        let mut edits = Edits::default();
        edits.set(field("script", json!("b.js")), None);
        edits.set(field("cwd", json!("/a")), None);
        let keys: Vec<_> = edits.iter().map(|(key, _)| key.clone()).collect();
        assert_eq!(
            keys,
            vec![
                EditKey::Field("cwd".to_owned()),
                EditKey::Field("script".to_owned())
            ]
        );
    }

    #[test]
    fn into_writes_produces_one_wire_edit_per_entry() {
        let mut edits = Edits::default();
        edits.set(field("cwd", json!("/a")), None);
        edits.set(env("NODE_ENV", None), None);
        let writes = edits.into_writes();
        assert_eq!(writes.len(), 2);
        assert!(matches!(writes[0], PaneEdit::Set { .. }));
        assert!(matches!(writes[1], PaneEdit::SetEnv { .. }));
    }

    /// The set never prints a value: `cwd` holds a home directory and an
    /// env value is a secret (IR-41).
    #[test]
    fn debug_prints_the_whole_set_and_no_value_in_it() {
        let mut edits = Edits::default();
        edits.set(
            field("cwd", json!("/home/someone/secret")),
            Some(ApplyGroup::NeedsRespawn),
        );
        let printed = format!("{edits:?}");
        assert_eq!(
            printed,
            "Edits { entries: {Field(\"cwd\"): Edit { edit: Set { key: \"cwd\", value: FieldValue(<string>) }, impact: Some(NeedsRespawn) }}, order: [Field(\"cwd\")] }"
        );
    }
}
