# Lookout pane 1e: the editing pane, implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Redraw the config pane as a field list beside an explanation panel, and rewire it from one write per edit to one batch on close.

**Architecture:** A new `lookout/edits.rs` owns the pending change set that frames 1g and 1k will consume. `ConfigPane` holds one and loses its arm-then-confirm path for field edits. `view/pane.rs` grows a right-hand explanation panel and a group tab row. Field metadata comes from the live Flockfile schema, extended with three new `init` keys and a self-verifying type table.

**Tech Stack:** Rust 1.88, edition 2024, ratatui, schemars, insta.

**Spec:** [docs/brainstorming/specs/2026-09-08-lookout-1e-editing-pane-design.md](../../brainstorming/specs/2026-09-08-lookout-1e-editing-pane-design.md)

## Global constraints

- MSRV 1.88, edition 2024.
- `#![forbid(unsafe_code)]` is live in shep-core, shep-client and shep-cli. Nothing in this plan needs unsafe.
- Read `docs/idiomatic-rust.md` through the `shep-idiomatic-rust` skill before writing Rust. Every new public item needs docs and a deliberate `Debug` decision (IR-41), and every fallible public function needs an `# Errors` section.
- Conventional commit subjects, `type(scope): summary`, with `!` on the commit that breaks something, in the crate that breaks. Only these types are read by release-plz: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `ci`, `chore`, `style`. Never `revert` or `build`.
- **One cargo shape for the whole plan: `--workspace`.** Iterate with `cargo test --workspace --lib --bins --all-features -- --skip ::slow::`. Do not add a `-p <crate>` run "to catch failures early": the workspace shares one target dir and switching shapes rebuilds.
- Prose that a person reads carries no em dashes and no en dashes. That includes doc comments, help text and the docs site.
- Terminology: one managed process is a `sheep`, the plural is `flock`, plugin processes are `dogs`, a sheep's children are `lambs`. Never bare "sheeps", never "instance" for a lamb.
- No absolute local paths anywhere, in code, comments or commit messages. Repo-relative only.
- **Two assertion bars, because Task 1's review caught the plan violating both.** A redacted `Debug` gets an exact-string test, not a `contains` that only proves one word is absent: IR-41 states the bar that way and `FieldValue`, `PanePending` and `EnvPane` all meet it. And an accessor test asserts the value it got back, never only that it got something: `is_some()` passes for a lookup that returns the wrong entry.
- **Every code snippet below that describes existing code is a guess written from a survey, not a quotation.** Grep the real file before editing. Where the plan and the code disagree, the code wins and the plan is wrong. Say so in your report.

## Fixtures this plan adds

`crates/shep-cli/src/lookout/view/fixtures.rs` already carries
`app_in_sheep_pane`, `app_in_sheep_pane_with_control`, `app_in_dog_pane`,
`sheep_config_view`, `draw_lines` and `render_all`. Everything below is new,
and the task that first names one adds it. **Every builder reaches its state
by driving real key presses**, the way `bleats_pane_with_filters` does, never
by reaching into the pane.

| Fixture | First used | What it returns |
|---|---|---|
| `app_in_sheep_pane_with_two_edits()` | T5 | a pane with `cwd` and `max_memory` filed, one of them a respawn |
| `app_in_dog_pane_with_two_edits()` | T9 | the same for a dog |
| `app_in_sheep_pane_with_env(&[(&str, &str)])` | T8 | a pane whose sheep declares those env keys |
| `app_with_plain_palette_in_sheep_pane()` | T7 | the same pane under the `plain` palette, for the colour-redundancy checks |
| `bark_pane()` | T9 | the existing bark section fixture from `pane.rs`, lifted here |
| `a_refusal()` | T5 | the crate's own error type, for a refused reply |
| `select_field(&mut App, &str)` | T5 | presses `SelectDown` until that field is under the cursor |
| `select_env_key(&mut App, &str)` | T8 | the same for an env row |
| `type_into_the_open_editor(&mut App, &str)` | T8 | `Confirm`, then the characters, then `TextApply` |

Five return a bounded slice of the rendered frame rather than the whole thing,
and they exist so no assertion in this plan searches a frame end to end. A
frame-wide `contains("respawn")` passes off the legend row, which is how a test
that pins nothing gets written.

| Slice fixture | First used | The rows it returns |
|---|---|---|
| `config_pane_title_band_for_tests(&App, u16)` | T6 | row 0 only |
| `config_pane_field_rows_for_tests(&App)` | T6 | the active group's rows, no headers |
| `config_pane_pending_rows_for_tests(&App)` | T6 | the rows under the `pending edits` rule |
| `config_pane_env_rows_for_tests(&App)` | T8 | the rows under the `env` rule |
| `config_pane_panel_for_tests(&App, u16)` | T7 | the right column only |
| `config_pane_panel_focused_on(&App, &str, u16)` | T7 | the same, after moving the cursor to that field |
| `config_pane_row_for_tests(&App, &str)` | T6 | one field's row |

## Dependency graph

Tasks 1, 2, 3 and 4 have no dependencies on each other and can be dispatched in parallel.

```
T1 edits module ──┐
T4 keys ──────────┼── T5 batching ──┬── T8 env fold
T2 field metadata ┤                 └── T9 dogs
T3 init keys ─────┘
T4 ── T6 tabs and columns ── T7 panel ── T10 responsive ── T11 scenes ── T12 docs
```

---

### Task 1: The pending edit set

**Files:**
- Create: `crates/shep-cli/src/lookout/edits.rs`
- Modify: `crates/shep-cli/src/lookout/mod.rs` (add `mod edits;`)

**Interfaces:**
- Consumes: `shep_core::config::ApplyGroup`, and `FieldValue` / `EnvValue` from `crates/shep-cli/src/lookout/pane.rs`.
- Produces: `Edits`, `EditKey`, `Edit`, with the method set below. Task 5 wires it into `ConfigPane`. Frames 1g and 1k read `worst_impact` and `len`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(key: &str, value: serde_json::Value) -> PaneEdit {
        PaneEdit::Set { key: key.to_owned(), value: FieldValue::from(value) }
    }

    fn env(key: &str, value: Option<EnvValue>) -> PaneEdit {
        PaneEdit::SetEnv { key: key.to_owned(), value }
    }

    #[test]
    fn an_edit_is_readable_by_the_key_it_was_filed_under() {
        let mut edits = Edits::default();
        edits.set(field("cwd", json!("/srv/app")), Some(ApplyGroup::NeedsRespawn));
        assert_eq!(edits.len(), 1);
        assert!(edits.get(&EditKey::Field("cwd".to_owned())).is_some());
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
        edits.set(field("script", json!("a.js")), Some(ApplyGroup::NeedsRespawn));
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
    fn debug_names_no_value_on_an_edit() {
        let mut edits = Edits::default();
        edits.set(field("cwd", json!("/home/someone/secret")), Some(ApplyGroup::NeedsRespawn));
        let printed = format!("{edits:?}");
        assert!(!printed.contains("secret"), "{printed}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace --lib --bins --all-features edits::`
Expected: FAIL, the module does not exist.

- [ ] **Step 3: Write the module**

Ordering note: `EditKey` needs `Ord` for the `BTreeMap`. Derive it, and put `Field` before `Env` in the enum so a config field sorts ahead of an env key of the same name.

```rust
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
    /// [`ApplyGroup::Structural`] cannot appear: those fields carry
    /// [`Lock::Refused`](super::pane::Lock::Refused) and no key reaches
    /// them.
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
```

Add `pub mod edits;` to `crates/shep-cli/src/lookout/mod.rs` beside the other pane modules. `std::collections::BTreeMap`, not `alloc`: shep-cli has no `extern crate alloc`, and the plan said `alloc` until a `cargo check` said otherwise.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace --lib --bins --all-features edits::`
Expected: PASS, 12 tests.

`cargo check` on this commit reports seven dead-code warnings, because nothing
outside the test module calls any of it until Task 5 wires it into
`ConfigPane`. That is expected and measured, not a mistake. Do not silence it
with an `allow`: a suppression added here is one nobody removes, and the
warnings go on their own with Task 5.

- [ ] **Step 5: Prove each test pins something**

For each of the 12 tests: change the implementation so the behaviour it names is wrong, confirm that test fails, restore. Two that are easy to get wrong and must be checked by hand:

- `an_unclassified_edit_does_not_become_the_lightest_impact` must fail if `worst_impact` maps `None` to `ApplyGroup::Live` instead of filtering it out.
- `re_editing_a_field_moves_it_to_the_end_of_the_undo_order` must fail if `set` skips the `retain` and pushes a duplicate. That is what the third assertion is for: with a stale key left in `order`, the last `undo` returns `Some` rather than `None`. A version of this test asserting only the first pop passes under that mutation, which is how a test that pins nothing gets written.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout/edits.rs crates/shep-cli/src/lookout/mod.rs
git commit -m "feat(cli): hold the config pane's pending edits in one set"
```

---

### Task 2: Field metadata the panel needs

**Files:**
- Modify: `crates/shep-cli/src/lookout/field.rs` (the `Field` struct near line 76, `field_from` near line 331)
- Create: `crates/shep-cli/src/lookout/validation.rs`
- Modify: `crates/shep-cli/src/lookout/mod.rs`

**Interfaces:**
- Consumes: the JSON Schema `Value` `field_from` already walks.
- Produces: `Field::example`, `Field::accepts`, `Field::refuses`, `Field::neighbours` (`Vec<Neighbour>`), and `validation::bullets(&Field) -> Bullets`. Task 7 renders all of them.

- [ ] **Step 1: Write the failing tests**

In `field.rs`, beside the existing schema tests:

```rust
#[test]
fn a_field_carries_its_example_from_the_init_block() {
    let set = FieldSet::from_properties(
        &props(json!({
            "cwd": { "type": ["string", "null"],
                     "init": { "example": "/srv/app", "group": "process" } }
        })),
        &Map::new(),
        &["process"],
    );
    assert_eq!(
        set.by_key("cwd").unwrap().example.as_deref(),
        Some("/srv/app")
    );
}

#[test]
fn a_field_carries_its_accepted_and_refused_forms() {
    let set = FieldSet::from_properties(
        &props(json!({
            "cwd": { "type": ["string", "null"], "init": {
                "group": "process",
                "accepts": ["an absolute or relative path", "~ expands, $VARS do not"],
                "refuses": ["a path the daemon's user cannot enter"]
            } }
        })),
        &Map::new(),
        &["process"],
    );
    let field = set.by_key("cwd").unwrap();
    assert_eq!(field.accepts.len(), 2);
    assert_eq!(field.refuses.len(), 1);
}

#[test]
fn a_neighbour_carries_a_field_name_and_a_note() {
    let set = FieldSet::from_properties(
        &props(json!({
            "cwd": { "type": ["string", "null"], "init": { "group": "process",
                "neighbours": [{ "field": "script", "note": "resolved against this cwd" }] } }
        })),
        &Map::new(),
        &["process"],
    );
    let neighbours = &set.by_key("cwd").unwrap().neighbours;
    assert_eq!(neighbours[0].field, "script");
    assert_eq!(neighbours[0].note, "resolved against this cwd");
}

/// An entry missing either half is dropped rather than half rendered.
#[test]
fn a_malformed_neighbour_entry_is_dropped() {
    let set = FieldSet::from_properties(
        &props(json!({
            "cwd": { "type": ["string", "null"], "init": { "group": "process",
                "neighbours": [{ "field": "script" }, { "note": "orphan" }] } }
        })),
        &Map::new(),
        &["process"],
    );
    assert!(set.by_key("cwd").unwrap().neighbours.is_empty());
}

/// A field carrying none of the three keys renders no headings, which is
/// the same "nothing rather than an empty one" rule the detail pane's
/// `cfg` cell follows.
#[test]
fn a_field_without_the_new_keys_carries_empty_lists() {
    let set = FieldSet::from_properties(
        &props(json!({ "cwd": { "type": ["string", "null"] } })),
        &Map::new(),
        &[],
    );
    let field = set.by_key("cwd").unwrap();
    assert!(field.example.is_none());
    assert!(field.accepts.is_empty());
    assert!(field.refuses.is_empty());
    assert!(field.neighbours.is_empty());
}

/// Every neighbour named by the real schema has to be a real field. This
/// is the only failure mode a hand written cross reference has.
#[test]
fn every_neighbour_in_the_real_schema_names_a_real_field() {
    let schema = shep_core::config::flockfile_schema_json().to_value();
    let props = schema
        .pointer("/$defs/AppConfig/properties")
        .and_then(serde_json::Value::as_object)
        .expect("app config properties must exist");
    let defs = schema
        .pointer("/$defs")
        .and_then(serde_json::Value::as_object)
        .expect("defs must exist");
    let set = FieldSet::from_properties(props, defs, shep_core::config::GROUP_ORDER);
    for field in set.fields() {
        for neighbour in &field.neighbours {
            assert!(
                set.by_key(&neighbour.field).is_some(),
                "{} names {}, which is not a field",
                field.key,
                neighbour.field
            );
        }
    }
}
```

In the new `validation.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use shep_core::config::{MemSize, UpDuration};

    /// The table is only safe away from the parsers because this test
    /// feeds it back through them. Every accepted form must parse and
    /// every refused form must not, so the table cannot go on claiming a
    /// form the parser has stopped taking.
    #[test]
    fn every_accepted_duration_form_parses() {
        for form in DURATION_FORMS {
            assert!(
                form.example.parse::<UpDuration>().is_ok(),
                "{} is listed as accepted but does not parse",
                form.example
            );
        }
    }

    #[test]
    fn every_refused_duration_form_fails_to_parse() {
        for form in DURATION_REFUSALS {
            assert!(
                form.example.parse::<UpDuration>().is_err(),
                "{} is listed as refused but parses",
                form.example
            );
        }
    }

    #[test]
    fn every_accepted_memory_form_parses() {
        for form in MEMORY_FORMS {
            assert!(
                form.example.parse::<MemSize>().is_ok(),
                "{} is listed as accepted but does not parse",
                form.example
            );
        }
    }

    #[test]
    fn a_field_with_its_own_accepts_replaces_the_type_table() {
        let mut field = text_field("cwd");
        field.value_kind = Some(ValueKind::UpDuration);
        field.accepts = vec!["only this".to_owned()];
        let bullets = bullets(&field);
        assert_eq!(bullets.accepts, vec!["only this".to_owned()]);
    }

    #[test]
    fn a_field_with_no_accepts_takes_the_type_table() {
        let mut field = text_field("min_uptime");
        field.value_kind = Some(ValueKind::UpDuration);
        let bullets = bullets(&field);
        assert!(bullets.accepts.len() > 1);
    }

    /// A plain string field the type table says nothing about renders no
    /// VALIDATION heading rather than an empty one.
    #[test]
    fn a_plain_text_field_with_no_annotation_has_no_bullets() {
        let bullets = bullets(&text_field("fold"));
        assert!(bullets.is_empty());
    }
}
```

Write `text_field` as a local helper building a `Field` with `FieldKind::Text` and every new member empty.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace --lib --bins --all-features field:: validation::`
Expected: FAIL, `Field` has no `example` and `validation` does not exist.

- [ ] **Step 3: Implement**

`Field` gains four members, documented:

```rust
    /// `init.example`, one concrete value a reader can copy.
    pub example: Option<String>,
    /// `init.accepts`, the forms this field takes, in the operator's
    /// words. Empty when the field carries none, in which case
    /// [`super::validation::bullets`] falls back to the type table.
    pub accepts: Vec<String>,
    /// `init.refuses`, the forms it turns down. Empty is the common case.
    pub refuses: Vec<String>,
    /// `init.neighbours`, the fields this one interacts with. Empty is the
    /// common case, and an entry missing either half is dropped.
    pub neighbours: Vec<Neighbour>,
```

```rust
/// One field this field interacts with, and how.
///
/// `Debug` is derived (IR-41): two names, no value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Neighbour {
    /// The other field's key. Tested to name a real field.
    pub field: String,
    /// What the interaction is, in one clause.
    pub note: String,
}
```

In `field_from`, read them from the same `init` the `blurb` and `suggest` reads already use. Write a `strings(init, "accepts")` helper mirroring `suggestions`, and a `neighbours(init)` that keeps only entries carrying both `field` and `note` as strings.

`validation.rs` holds the type table and the fallback:

```rust
//! What a field accepts, in the operator's words.
//!
//! Two sources, and the per-field one wins outright rather than merging:
//! a field writing its own `accepts` gets those and none from the table.
//! One rule beats merge semantics nobody can predict.
//!
//! The table lives here rather than beside the parsers because it is
//! rendering copy. It is safe there only because its own tests feed every
//! listed form back through the real parser.

/// One line of the table: what to print, and the string that proves it.
pub struct Form {
    /// What the panel prints.
    pub text: &'static str,
    /// A value the claim is checked against in this module's tests.
    pub example: &'static str,
}

pub const DURATION_FORMS: &[Form] = &[
    Form { text: "500ms, 2s, 1m30s", example: "1m30s" },
    Form { text: "a bare number is milliseconds", example: "250" },
];

pub const DURATION_REFUSALS: &[Form] = &[
    Form { text: "a negative", example: "-1s" },
    Form { text: "a unit shep does not know", example: "3 fortnights" },
];

pub const MEMORY_FORMS: &[Form] = &[
    Form { text: "512M, 2G", example: "512M" },
    Form { text: "a bare number is bytes", example: "1048576" },
];
```

Plus `BOOL_FORMS`, `INTEGER_FORMS`, and a `bullets(field: &Field) -> Bullets` that returns the per-field lists when non-empty and otherwise the table's, keyed on `field.value_kind` first and `field.kind` second. `Bullets` is a small struct of `accepts: Vec<String>` and `refuses: Vec<String>` with an `is_empty`.

Confirm before writing: `UpDuration` and `MemSize` may not implement `FromStr`. Grep for their parse entry point and use whatever the real one is; the tests above assume `parse()` and the plan may be wrong about that.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace --lib --bins --all-features field:: validation::`
Expected: PASS.

- [ ] **Step 5: Prove each test pins something**

Mutate and confirm red. The one to check carefully: `every_neighbour_in_the_real_schema_names_a_real_field` passes trivially while no field carries a `neighbours` block, which is true until Task 3 lands. Add a temporary bogus neighbour to one field, watch it fail, remove it.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout/field.rs crates/shep-cli/src/lookout/validation.rs crates/shep-cli/src/lookout/mod.rs
git commit -m "feat(cli): read a field's example, accepted forms and neighbours"
```

---

### Task 3: The new `init` keys on `AppConfig`

**Files:**
- Modify: `crates/shep-core/src/config/app.rs` (the field attributes, roughly lines 105 to 494)

**Interfaces:**
- Consumes: nothing.
- Produces: `accepts`, `refuses` and `neighbours` under `init` for the fields a parser cannot describe. Task 2's reader and Task 7's panel both read them.

**Scope.** The 22 fields whose shape a parser describes get nothing here: every `UpDuration`, `MemSize`, `bool`, and integer field is already covered by Task 2's table. The remaining 19 are the string, path, glob, list, map and probe fields. Annotate where it helps. Not every one of the 19 needs all three keys, and a field with nothing useful to say gets nothing.

- [ ] **Step 1: Write the failing test**

In `app.rs`'s test module:

```rust
/// The path fields are the ones an operator gets wrong, and the ones the
/// type table cannot describe. Each states what it takes.
#[test]
fn the_path_fields_state_what_they_accept() {
    let schema = crate::config::flockfile_schema_json().to_value();
    let props = schema
        .pointer("/$defs/AppConfig/properties")
        .and_then(serde_json::Value::as_object)
        .expect("app config properties must exist");
    for name in ["cwd", "script", "out_file", "err_file"] {
        let accepts = props[name]["init"]["accepts"].as_array();
        assert!(
            accepts.is_some_and(|forms| !forms.is_empty()),
            "{name} carries no accepted forms"
        );
    }
}

/// Every entry carries both halves, so nothing renders half a line.
#[test]
fn every_neighbour_entry_carries_a_field_and_a_note() {
    let schema = crate::config::flockfile_schema_json().to_value();
    let props = schema
        .pointer("/$defs/AppConfig/properties")
        .and_then(serde_json::Value::as_object)
        .expect("app config properties must exist");
    for (name, prop) in props {
        let Some(entries) = prop["init"]["neighbours"].as_array() else {
            continue;
        };
        for entry in entries {
            assert!(entry["field"].is_string(), "{name} has a neighbour with no field");
            assert!(entry["note"].is_string(), "{name} has a neighbour with no note");
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace --lib --bins --all-features --features schema config::app::`
Expected: FAIL on the first test, no `accepts` on `cwd`.

- [ ] **Step 3: Write the annotations**

`cwd` in full, as the shape to copy:

```rust
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "/srv/app",
        "group": "process",
        "blurb": "Where the process runs. Without it, the daemon's own directory",
        "accepts": ["an absolute or relative path, expanded from cwd",
                    "~ expands, $VARS do not"],
        "refuses": ["a path the daemon's user cannot enter"],
        "neighbours": [{"field": "script",   "note": "resolved against this cwd"},
                       {"field": "out_file", "note": "relative paths follow it too"},
                       {"field": "watch",    "note": "globs are rooted here"}]
    })))]
    pub cwd: Option<String>,
```

Keep the existing `example`, `group`, `blurb` and `suggest` values exactly as they are. Only add.

Rules while writing the copy:

- No em dashes, no en dashes. A comma, a colon or a second bullet instead.
- One clause per bullet, lowercase, no terminal period. These render as bullets beside a coloured block, not as sentences.
- A neighbour's note says what the interaction is, not that there is one. `resolved against this cwd`, never `related to cwd`.
- Do not invent a refusal shep does not make. If you cannot point at the code that refuses it, leave it out. Grep `crates/shep-core/src/config/` and `crates/shep-daemon/src/supervisor.rs` before claiming a refusal.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace --lib --bins --all-features --features schema config::`
Expected: PASS, including the existing `every_appconfig_field_has_a_group` and the scaffold drift tests.

- [ ] **Step 5: Check what the schema output moved**

Run: `cargo run -p shep -- schema | head -40` and confirm the new keys ride under `init`. If a golden test of `flockfile_schema_string` exists, update it in this commit and say so.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-core/src/config/app.rs
git commit -m "feat(core): say what each config field accepts and interacts with"
```

---

### Task 4: The four keys

**Files:**
- Modify: `crates/shep-cli/src/lookout/input.rs` (the `Normal` match, roughly lines 53 to 83)
- Modify: `crates/shep-cli/src/lookout/app.rs` (the `KeyPress` enum, and every `ListRemove` use)

**Interfaces:**
- Consumes: nothing.
- Produces: `KeyPress::NextGroup`, `KeyPress::Group(u8)`, `KeyPress::Undo`, and `KeyPress::Remove` replacing `KeyPress::ListRemove`. Tasks 5, 6 and 8 handle them in the reducer.

- [ ] **Step 1: Write the failing tests**

In `input.rs`'s test module, matching the shape of the bindings tests already there:

```rust
#[test]
fn tab_asks_for_the_next_group() {
    assert_eq!(press(KeyCode::Tab), Some(KeyPress::NextGroup));
}

#[test]
fn the_digits_one_through_eight_jump_to_a_group() {
    for (typed, wanted) in [('1', 1_u8), ('4', 4), ('8', 8)] {
        assert_eq!(press(KeyCode::Char(typed)), Some(KeyPress::Group(wanted)));
    }
}

/// Eight groups, so nine and zero are not group keys and stay free.
#[test]
fn nine_and_zero_are_unbound() {
    assert_eq!(press(KeyCode::Char('9')), None);
    assert_eq!(press(KeyCode::Char('0')), None);
}

#[test]
fn bare_u_undoes_and_ctrl_u_still_pages() {
    assert_eq!(press(KeyCode::Char('u')), Some(KeyPress::Undo));
    let ctrl_u = Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(map_key(&ctrl_u, InputMode::Normal), Some(KeyPress::PageUp));
}

/// A digit typed into a text box is text, not a group jump.
#[test]
fn a_digit_in_text_mode_is_typed() {
    let one = Event::Key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
    assert_eq!(map_key(&one, InputMode::Text), Some(KeyPress::TextChar('1')));
}

#[test]
fn d_removes() {
    assert_eq!(press(KeyCode::Char('d')), Some(KeyPress::Remove));
}
```

Write `press` as a local helper mapping a `KeyCode` in `InputMode::Normal` if one does not already exist.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace --lib --bins --all-features input::`
Expected: FAIL, the variants do not exist.

- [ ] **Step 3: Implement**

Add three variants to `KeyPress` with doc comments naming the key and what it does, and rename `ListRemove` to `Remove`. Add the arms to the `Normal` match. The digit arm needs to sit ahead of any general `Char` arm and match only `'1'..='8'`.

The rename is a source break inside the crate only: `KeyPress` is not part of any published API, so this is `refactor(cli)`, not `refactor(cli)!`. Confirm that by grepping for `KeyPress` outside `crates/shep-cli/`; if it escapes the crate, the commit takes a `!`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace --lib --bins --all-features input::`
Expected: PASS.

- [ ] **Step 5: Prove the digit guard pins something**

Widen the digit arm to `'0'..='9'`, confirm `nine_and_zero_are_unbound` fails, restore.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout/input.rs crates/shep-cli/src/lookout/app.rs
git commit -m "feat(cli): bind tab, the group digits and undo in the config pane"
```

---

### Task 5: Batch the writes

**Files:**
- Modify: `crates/shep-cli/src/lookout/pane.rs` (`ConfigPane`, `PanePending`, `cycle`, `apply_typing`, `take_armed`, `settle`)
- Modify: `crates/shep-cli/src/lookout/app.rs` (`on_pane_key` near line 2990, `Effect`, the `Sent::ApplyField` reply arms near lines 1645 and 1683)
- Modify: `crates/shep-cli/src/lookout/mod.rs` (the `Effect::Send` handler near line 409)

**Interfaces:**
- Consumes: `Edits` from Task 1, `KeyPress::Undo` and `KeyPress::Remove` from Task 4.
- Produces: `ConfigPane::edits() -> &Edits`, `ConfigPane::close() -> Edits`, whose `into_writes` builds the batch, and `Effect::SendAll(Vec<Sent>)`.

**This is the task that deletes things.** `PanePending::Armed` and `PanePending::Sent` go, and with them every test that pinned the arm-then-confirm path for a config field. List the deleted tests in the commit body by name and say what replaced each. A reviewer needs that list to tell a deliberate removal from a dropped requirement.

- [ ] **Step 1: Write the failing tests**

These drive `App`, not `ConfigPane`, because the door the operator uses is a keypress:

```rust
#[test]
fn cycling_a_bool_files_an_edit_and_sends_nothing() {
    let mut app = fixtures::app_in_sheep_pane();
    app.set_control_for_tests(Control::Allowed);
    let effect = app.update(Msg::Key(KeyPress::Cycle));
    assert!(matches!(effect, Effect::None), "{effect:?}");
    assert_eq!(app.config_pane().unwrap().edits().len(), 1);
}

#[test]
fn undo_drops_the_edit_it_filed() {
    let mut app = fixtures::app_in_sheep_pane();
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Cycle));
    app.update(Msg::Key(KeyPress::Undo));
    assert!(app.config_pane().unwrap().edits().is_empty());
}

#[test]
fn cycling_a_bool_back_to_its_stored_value_files_nothing() {
    let mut app = fixtures::app_in_sheep_pane();
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Cycle));
    app.update(Msg::Key(KeyPress::Cycle));
    assert!(
        app.config_pane().unwrap().edits().is_empty(),
        "a round trip back to the stored value is not an edit"
    );
}

#[test]
fn escape_sends_every_filed_edit_at_once() {
    let mut app = fixtures::app_in_sheep_pane_with_two_edits();
    let effect = app.update(Msg::Key(KeyPress::Escape));
    let Effect::SendAll(sent) = effect else {
        panic!("wanted a batch, got {effect:?}");
    };
    assert_eq!(sent.len(), 2);
    assert!(app.config_pane().is_none(), "the pane closes on the same key");
}

#[test]
fn escape_with_nothing_filed_closes_and_sends_nothing() {
    let mut app = fixtures::app_in_sheep_pane();
    let effect = app.update(Msg::Key(KeyPress::Escape));
    assert!(matches!(effect, Effect::None), "{effect:?}");
    assert!(app.config_pane().is_none());
}

/// Read-only refuses the first keypress, not the close. Building five
/// edits and losing them all at `esc` wastes the operator's time.
#[test]
fn read_only_refuses_the_first_edit_and_files_nothing() {
    let mut app = fixtures::app_in_sheep_pane();
    app.set_control_for_tests(Control::ReadOnly);
    app.update(Msg::Key(KeyPress::Cycle));
    assert!(app.config_pane().unwrap().edits().is_empty());
    let notice = app.notice().map(ToString::to_string).expect("a refusal says so");
    assert!(notice.contains("read-only"), "{notice}");
}

/// A refusal now lands after the pane has gone, so its arm must not
/// assume a pane is open.
#[test]
fn a_refused_write_notices_after_the_pane_has_closed() {
    let mut app = fixtures::app_in_sheep_pane_with_two_edits();
    let Effect::SendAll(mut sent) = app.update(Msg::Key(KeyPress::Escape)) else {
        panic!("wanted a batch");
    };
    let first = sent.remove(0);
    assert!(app.config_pane().is_none());
    app.update(Msg::Replied {
        sent: first,
        result: Err(fixtures::a_refusal()),
    });
    let notice = app.notice().map(ToString::to_string).expect("a refusal says so");
    assert!(notice.contains("cwd"), "the notice names the field: {notice}");
}

/// Validation runs on entry, so the set is always sendable. An integer
/// field mid-word holds the editor open rather than filing a bad value,
/// which is what `apply_typing` already does today.
#[test]
fn a_value_that_does_not_parse_never_joins_the_set() {
    let mut app = fixtures::app_in_sheep_pane();
    app.set_control_for_tests(Control::Allowed);
    fixtures::select_field(&mut app, "max_restarts");
    app.update(Msg::Key(KeyPress::Confirm));
    for typed in "not a number".chars() {
        app.update(Msg::Key(KeyPress::TextChar(typed)));
    }
    app.update(Msg::Key(KeyPress::TextApply));
    assert!(app.config_pane().unwrap().edits().is_empty());
    assert_eq!(app.mode(), InputMode::Text, "the editor stays open");
}

/// The set is the operator's and the values are the shepherd's.
#[test]
fn a_config_re_read_replaces_the_values_and_keeps_the_edits() {
    let mut app = fixtures::app_in_sheep_pane_with_two_edits();
    let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Refresh)) else {
        panic!("refresh asks the shepherd for the config again");
    };
    app.update(Msg::Replied {
        sent,
        result: Ok(fixtures::sheep_config_view().into()),
    });
    assert_eq!(app.config_pane().unwrap().edits().len(), 2);
}
```

Fill in the three commented placeholders from the real `Msg` and `Sent` shapes, and add the two fixtures to `crates/shep-cli/src/lookout/view/fixtures.rs` in the style of `bleats_pane_with_filters`, driving real key presses rather than reaching into the pane.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace --lib --bins --all-features app::`
Expected: FAIL.

- [ ] **Step 3: Implement**

- `ConfigPane` gains `edits: Edits` and loses nothing else yet.
- `PanePending` collapses to its `Typing` arm. Rename the type to name what it now is, and keep its redacted `Debug` and that `Debug`'s exact-string test.
- `cycle` and `apply_typing` file into `edits` instead of arming. Both compare the new value against `values` first and call `Edits::remove` when they match.
- `take_armed` and `settle` go.
- `ConfigPane::close(self) -> Edits` hands the set out, and `on_pane_key` turns it into writes with `Edits::into_writes`.
- `Effect::SendAll(Vec<Sent>)`, handled in `mod.rs` by looping the existing `Effect::Send` body. Do not hold a lock or block: it is the same `try_send` per item.
- `on_pane_key`'s `Escape` arm closes the pane and returns the batch. The control gate moves to the arms that file an edit.
- Check `Sent::ApplyField`'s two reply arms for an assumption that the pane is still open. The plan believes they reach into `config_pane_mut` to settle a ticket; grep and confirm before changing them.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace --lib --bins --all-features -- --skip ::slow::`
Expected: PASS. Existing snapshot tests of the pane will move; accept them only after reading each diff.

- [ ] **Step 5: Prove each test pins something**

Mutate and confirm red for all eight. Two worth attention:

- `escape_sends_every_filed_edit_at_once` must fail if `close` sends only the first edit. Assert the length, not just that a batch came back.
- `cycling_a_bool_back_to_its_stored_value_files_nothing` must fail if `set` is called unconditionally.

- [ ] **Step 6: Commit**

```bash
git add -A crates/shep-cli/src/lookout/
git commit -m "refactor(cli): write a config pane's edits once, when it closes"
```

The commit body lists every deleted test by name and what covers its behaviour now.

---

### Task 6: Group tabs and the two-column body

**Files:**
- Modify: `crates/shep-cli/src/lookout/pane.rs` (`ConfigPane` gains the active group)
- Modify: `crates/shep-cli/src/lookout/view/pane.rs` (`pane_lines` near line 590)
- Modify: `crates/shep-cli/src/lookout/app.rs` (`on_pane_key`, the `NextGroup` and `Group` arms)

**Interfaces:**
- Consumes: `KeyPress::NextGroup` and `KeyPress::Group(u8)` from Task 4.
- Produces: `ConfigPane::group() -> &str`, `ConfigPane::set_group(u8)` taking the one-based digit, `ConfigPane::next_group()`, and the left column's row layout Task 7 draws beside.

- [ ] **Step 1: Write the failing tests**

```rust
/// Eight groups, in GROUP_ORDER, and every one of them reachable. This is
/// the test that would have caught a filter axis nothing could set.
#[test]
fn tab_walks_every_group_and_each_one_shows_its_own_fields() {
    let mut app = fixtures::app_in_sheep_pane();
    let mut seen = Vec::new();
    for _ in 0..shep_core::config::GROUP_ORDER.len() {
        let group = app.config_pane().unwrap().group().to_owned();
        let rendered = fixtures::draw_lines(&app, 160, 48);
        let listed = fixtures::render_all(&rendered);
        assert!(
            listed.contains(&group),
            "the tab row does not name {group}"
        );
        seen.push(group);
        app.update(Msg::Key(KeyPress::NextGroup));
    }
    assert_eq!(seen, shep_core::config::GROUP_ORDER.to_vec());
}

#[test]
fn tab_wraps_from_the_last_group_to_the_first() {
    let mut app = fixtures::app_in_sheep_pane();
    for _ in 0..shep_core::config::GROUP_ORDER.len() {
        app.update(Msg::Key(KeyPress::NextGroup));
    }
    assert_eq!(app.config_pane().unwrap().group(), GROUP_ORDER[0]);
}

/// The digits reach the same eight groups tab does. A key that reaches
/// nothing is exactly the shape this plan exists to avoid.
#[test]
fn the_digits_reach_the_same_groups_tab_does() {
    for (index, wanted) in GROUP_ORDER.iter().enumerate() {
        let mut app = fixtures::app_in_sheep_pane();
        let digit = u8::try_from(index + 1).unwrap();
        app.update(Msg::Key(KeyPress::Group(digit)));
        assert_eq!(&app.config_pane().unwrap().group(), wanted);
    }
}

#[test]
fn the_list_shows_only_the_active_groups_fields() {
    let mut app = fixtures::app_in_sheep_pane();
    app.update(Msg::Key(KeyPress::Group(1)));
    let rows = fixtures::config_pane_field_rows_for_tests(&app);
    assert!(rows.iter().any(|row| row.contains("cwd")), "{rows:?}");
    assert!(
        !rows.iter().any(|row| row.contains("kill_timeout")),
        "a shutdown field is showing under process: {rows:?}"
    );
}

/// The pending section spans every group, which is the whole reason it is
/// a section rather than a marker.
#[test]
fn the_pending_section_lists_an_edit_from_another_group() {
    let mut app = fixtures::app_in_sheep_pane_with_two_edits();
    app.update(Msg::Key(KeyPress::Group(1)));
    let rows = fixtures::config_pane_pending_rows_for_tests(&app);
    assert!(rows.iter().any(|row| row.contains("max_memory")), "{rows:?}");
}

/// `pending` is the shepherd's word for a field written and parked. The
/// pane's own unsent count must not borrow it, or one word means two
/// things on two panes an operator moves between with one keypress.
#[test]
fn the_title_band_counts_edits_without_calling_them_pending() {
    let app = fixtures::app_in_sheep_pane_with_two_edits();
    let band = fixtures::config_pane_title_band_for_tests(&app, 160);
    assert!(band.contains("2 edits"), "{band}");
    assert!(!band.contains("pending"), "{band}");
}

/// An edited row shows its own change rather than borrowing the `!` the
/// shepherd's parked-field marker already owns.
#[test]
fn an_edited_row_shows_old_then_new_and_takes_no_marker() {
    let mut app = fixtures::app_in_sheep_pane_with_two_edits();
    let row = fixtures::config_pane_row_for_tests(&app, "max_memory");
    assert!(row.contains("->"), "{row}");
    assert!(!row.trim_start().starts_with('!'), "{row}");
}
```

`config_pane_field_rows_for_tests`, `config_pane_pending_rows_for_tests` and `config_pane_row_for_tests` are new fixtures returning a bounded slice of the rendered frame. **They exist so no assertion in this plan searches a whole frame.** A frame-wide `contains("respawn")` passes off the legend row, which is how a test that pins nothing gets written.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace --lib --bins --all-features -- --skip ::slow:: view::pane app::`
Expected: FAIL.

- [ ] **Step 3: Implement**

- `ConfigPane` gains `group: usize`, an index into `GROUP_ORDER`, defaulting to 0.
- `next_group` wraps. `set_group` takes the one-based digit the key carries and ignores anything past the list length, so a ninth group added later needs a key before it is reachable.
- `pane_lines` draws, in order: the butter title band, the provenance row, the tab row, a hairline, the header row, the body, a hairline, the legend.
- The tab row draws the active group as a paper-2 chip in ink and the rest in ink-3, then `tab next group   1…8 jump`.
- The body's left column draws the active group's fields, a blank row, the `pending edits` rule with every filed edit, a blank row, the `env` rule with its keys, then `+ add a key`.
- An edited row renders `old -> new` in its value cell in butter and keeps whatever `lock` and `flag` marker it already had.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace --lib --bins --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 5: Prove each test pins something**

Mutate and confirm red. Check specifically that `the_digits_reach_the_same_groups_tab_does` fails when `set_group` is off by one, and that `the_list_shows_only_the_active_groups_fields` fails when the filter is dropped.

- [ ] **Step 6: Commit**

```bash
git add -A crates/shep-cli/src/lookout/
git commit -m "feat(cli): group the config pane's fields behind tabs"
```

---

### Task 7: The explanation panel

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/pane.rs`

**Interfaces:**
- Consumes: `Field::example`, `accepts`, `refuses`, `neighbours` from Task 2; `validation::bullets` from Task 2; the `init` copy from Task 3; the two-column body from Task 6.
- Produces: the right-hand panel Task 10 makes responsive.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_panel_describes_the_focused_field_only() {
    let app = fixtures::app_in_sheep_pane();
    let panel = fixtures::config_pane_panel_for_tests(&app, 160);
    assert!(panel.iter().any(|row| row.contains("cwd")), "{panel:?}");
    assert!(
        !panel.iter().any(|row| row.contains("kill_timeout")),
        "the panel is showing a field that is not focused: {panel:?}"
    );
}

#[test]
fn the_panel_names_now_default_and_example() {
    let app = fixtures::app_in_sheep_pane();
    let panel = fixtures::config_pane_panel_for_tests(&app, 160);
    for label in ["now", "default", "example"] {
        assert!(
            panel.iter().any(|row| row.trim_start().starts_with(label)),
            "no {label} row: {panel:?}"
        );
    }
}

/// The panel follows the cursor, and does so on the keypress that moves
/// it rather than on the next redraw.
#[test]
fn the_panel_follows_the_selection() {
    let mut app = fixtures::app_in_sheep_pane();
    let first = fixtures::config_pane_panel_for_tests(&app, 160);
    app.update(Msg::Key(KeyPress::SelectDown));
    let second = fixtures::config_pane_panel_for_tests(&app, 160);
    assert_ne!(first, second);
}

#[test]
fn a_duration_field_takes_its_bullets_from_the_type_table() {
    let mut app = fixtures::app_in_sheep_pane();
    app.update(Msg::Key(KeyPress::Group(4)));
    let panel = fixtures::config_pane_panel_focused_on(&app, "min_uptime", 160);
    assert!(
        panel.iter().any(|row| row.contains("milliseconds")),
        "{panel:?}"
    );
}

/// A field with no accepted forms and no neighbours renders no heading,
/// not an empty one. Same rule as the detail pane's cfg cell.
#[test]
fn a_field_with_nothing_to_say_renders_no_headings() {
    let app = fixtures::app_in_sheep_pane();
    let panel = fixtures::config_pane_panel_focused_on(&app, "fold", 160);
    assert!(!panel.iter().any(|row| row.contains("VALIDATION")), "{panel:?}");
    assert!(!panel.iter().any(|row| row.contains("NEIGHBOURS")), "{panel:?}");
}

/// Colour is never the only carrier: a refused form has to read as
/// refused with every colour stripped.
#[test]
fn a_refused_form_reads_as_refused_without_colour() {
    let app = fixtures::app_with_plain_palette_in_sheep_pane();
    let panel = fixtures::config_pane_panel_for_tests(&app, 160);
    let refusal = panel
        .iter()
        .find(|row| row.contains("cannot enter"))
        .expect("cwd states a refusal");
    assert!(refusal.contains("refused"), "{refusal}");
}

#[test]
fn the_blurb_wraps_rather_than_truncating() {
    let app = fixtures::app_in_sheep_pane();
    let panel = fixtures::config_pane_panel_for_tests(&app, 160);
    assert!(
        panel.iter().all(|row| row.chars().count() <= 72),
        "a panel row overflows its column: {panel:?}"
    );
    assert!(panel.iter().filter(|row| !row.trim().is_empty()).count() > 3);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace --lib --bins --all-features view::pane`
Expected: FAIL.

- [ ] **Step 3: Implement**

Panel regions, top to bottom, each omitted when its source is empty:

1. a `FOCUSED` chip on butter, then the field name in ink, then its type in ink-3
2. the blurb, wrapped at 66 columns
3. blank, then `now`, `default` and `example` as label-value rows, labels ink-3
4. blank, then the impact tag and its sentence: `▲ respawn  the sheep must stop and start again`, with `pick the timing when you close the pane` on the next row, right aligned
5. blank, a hairline, then `VALIDATION`, one row per accepted form behind a meadow `█` and one per refused form behind a bark `█`, with the refused rows carrying the word `refused`
6. blank, then `NEIGHBOURS`, one row per entry: the field name then the note

Check `▲` and `●` against the width rule `view/flock.rs:46` applies: the file already rejected `▸` for being East Asian Ambiguous, and every glyph this panel adds needs the same check with the same test.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace --lib --bins --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 5: Prove each test pins something**

Mutate and confirm red. `a_refused_form_reads_as_refused_without_colour` is the one that catches a colour-only cell; check it fails when the word `refused` is dropped and only the bark block remains.

- [ ] **Step 6: Commit**

```bash
git add -A crates/shep-cli/src/lookout/view/
git commit -m "feat(cli): explain the focused config field beside the list"
```

---

### Task 8: Fold env into the list

**Files:**
- Modify: `crates/shep-cli/src/lookout/pane.rs` (`EnvPane`, `PaneRow`, `ConfigPane::rows`)
- Modify: `crates/shep-cli/src/lookout/view/pane.rs` (`env_lines` near line 281 goes)
- Modify: `crates/shep-cli/src/lookout/app.rs` (`on_pane_key`'s env arms)

**Interfaces:**
- Consumes: the batch from Task 5, the row layout from Task 6.
- Produces: `PaneRow::Env(usize)` and `PaneRow::AddEnv`, walked by the same cursor as `PaneRow::Field`.

- [ ] **Step 1: Write the failing tests**

```rust
/// The wire carries no env value for any key, Flockfile or store, so
/// every one renders the same way. SheepConfigView::new clears the map.
#[test]
fn every_env_value_renders_as_set_and_never_as_itself() {
    let app = fixtures::app_in_sheep_pane_with_env(&[("NODE_ENV", "production")]);
    let rows = fixtures::config_pane_env_rows_for_tests(&app);
    assert!(rows.iter().any(|row| row.contains("NODE_ENV")), "{rows:?}");
    assert!(
        rows.iter().all(|row| !row.contains("production")),
        "an env value reached the pane: {rows:?}"
    );
    assert!(rows.iter().any(|row| row.contains("(set)")), "{rows:?}");
}

#[test]
fn one_cursor_walks_from_the_last_field_into_the_env_keys() {
    let mut app = fixtures::app_in_sheep_pane_with_env(&[("NODE_ENV", "x")]);
    app.update(Msg::Key(KeyPress::SelectLast));
    assert!(matches!(
        app.config_pane().unwrap().rows().last(),
        Some(PaneRow::AddEnv)
    ));
}

#[test]
fn setting_an_env_key_files_an_edit_rather_than_sending() {
    let mut app = fixtures::app_in_sheep_pane_with_env(&[("NODE_ENV", "x")]);
    app.set_control_for_tests(Control::Allowed);
    fixtures::select_env_key(&mut app, "NODE_ENV");
    app.update(Msg::Key(KeyPress::Confirm));
    for typed in "staging".chars() {
        app.update(Msg::Key(KeyPress::TextChar(typed)));
    }
    let effect = app.update(Msg::Key(KeyPress::TextApply));
    assert!(matches!(effect, Effect::None), "{effect:?}");
    assert_eq!(app.config_pane().unwrap().edits().len(), 1);
}

/// An env edit and a config field of the same name are two entries, which
/// is the whole reason EditKey has two arms.
#[test]
fn an_env_edit_does_not_collide_with_the_env_config_field() {
    let mut app = fixtures::app_in_sheep_pane_with_env(&[("env", "x")]);
    app.set_control_for_tests(Control::Allowed);
    fixtures::select_env_key(&mut app, "env");
    fixtures::type_into_the_open_editor(&mut app, "y");
    assert_eq!(app.config_pane().unwrap().edits().len(), 1);
    assert!(
        app.config_pane()
            .unwrap()
            .edits()
            .get(&EditKey::Env("env".to_owned()))
            .is_some()
    );
}

#[test]
fn the_add_a_key_row_opens_an_editor() {
    let mut app = fixtures::app_in_sheep_pane_with_env(&[]);
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::SelectLast));
    app.update(Msg::Key(KeyPress::Confirm));
    assert_eq!(app.mode(), InputMode::Text);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace --lib --bins --all-features -- --skip ::slow:: pane`
Expected: FAIL.

- [ ] **Step 3: Implement**

- `PaneRow` gains `Env(usize)` and `AddEnv`. Its doc comment currently says it stays one variant because the env sub-screen went a different way; rewrite that comment, since the reason has stopped being true.
- `ConfigPane::rows` returns the active group's fields, then the pending rows as non-selectable display rows, then the env rows, then `AddEnv`.
- `EnvPane`'s own viewport and cursor go. Its typing helpers move onto `ConfigPane` or stay as a smaller value type; pick whichever leaves fewer moving parts and say which in the report.
- `env_lines` and the sub-screen's title go with it. `the_env_sub_screen_never_renders_a_value` is replaced by `every_env_value_renders_as_set_and_never_as_itself`, which pins the same fact on the surface that now shows it. Name that swap in the commit body.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace --lib --bins --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 5: Prove each test pins something**

Mutate and confirm red. `every_env_value_renders_as_set_and_never_as_itself` is the important one: it must fail if the renderer ever prints the value it was handed.

- [ ] **Step 6: Commit**

```bash
git add -A crates/shep-cli/src/lookout/
git commit -m "feat(cli): list a sheep's env keys in the config pane itself"
```

---

### Task 9: One section write for a dog

**Files:**
- Modify: `crates/shep-cli/src/lookout/pane.rs` (`edited_section` near line 930)

**Interfaces:**
- Consumes: `Edits` from Task 1, `ConfigPane::close` from Task 5.
- Produces: `ConfigPane::edited_section_with(&Edits) -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_dog_section_takes_every_filed_edit_in_one_write() {
    let mut pane = fixtures::bark_pane();
    let mut edits = Edits::default();
    edits.set(field("url", json!("http://a")), None);
    edits.set(field("timeout", json!(30)), None);
    let section = pane.edited_section_with(&edits).expect("the fixture parses");
    assert!(section.contains("http://a"), "{section}");
    assert!(section.contains("30"), "{section}");
}

/// Comments and key order survive, which is the whole reason a dog write
/// replaces the section through toml_edit rather than re-rendering it.
#[test]
fn a_dog_section_keeps_its_comments_through_a_batch() {
    let pane = fixtures::bark_pane();
    let mut edits = Edits::default();
    edits.set(field("url", json!("http://a")), None);
    edits.set(field("timeout", json!(30)), None);
    let section = pane.edited_section_with(&edits).expect("the fixture parses");
    assert!(section.contains("# "), "a comment was dropped: {section}");
}

#[test]
fn a_null_edit_removes_the_key_from_a_batched_section() {
    let pane = fixtures::bark_pane();
    let mut edits = Edits::default();
    edits.set(field("url", json!(null)), None);
    let section = pane.edited_section_with(&edits).expect("the fixture parses");
    assert!(!section.contains("url"), "{section}");
}

/// An env edit has no home in a dog's section and must not silently
/// become a key in it.
#[test]
fn an_env_edit_is_ignored_by_a_dog_section() {
    let pane = fixtures::bark_pane();
    let mut edits = Edits::default();
    edits.set(PaneEdit::SetEnv { key: "SECRET".to_owned(), value: None }, None);
    assert_eq!(pane.edited_section_with(&edits), None);
}

#[test]
fn closing_a_dog_pane_sends_one_write_for_two_edits() {
    let mut app = fixtures::app_in_dog_pane_with_two_edits();
    let Effect::SendAll(sent) = app.update(Msg::Key(KeyPress::Escape)) else {
        panic!("wanted a batch");
    };
    assert_eq!(sent.len(), 1, "a dog takes one section write, not two");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace --lib --bins --all-features pane::`
Expected: FAIL.

- [ ] **Step 3: Implement**

`edited_section_with` parses the section once and applies every `EditKey::Field` entry to the same `toml_edit::DocumentMut` in key order, then renders. `EditKey::Env` entries are skipped: a dog has no env store. Returning `None` when the set holds no field edit keeps the existing "nothing to write" path.

Keep the existing single-edit `edited_section` only if something still calls it. If nothing does, delete it and say so.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace --lib --bins --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 5: Prove each test pins something**

Mutate and confirm red. `a_dog_section_keeps_its_comments_through_a_batch` must fail if the implementation re-parses the section per edit and drops the accumulated document.

- [ ] **Step 6: Commit**

```bash
git add -A crates/shep-cli/src/lookout/
git commit -m "feat(cli): apply a dog's whole batch to one section write"
```

---

### Task 10: The responsive ladder

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/pane.rs` (`widths` near line 94, `pane_lines` near line 590)

**Interfaces:**
- Consumes: the panel from Task 7.
- Produces: `panel_width(u16) -> Option<u16>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_design_target_splits_eighty_eight_and_seventy_two() {
    assert_eq!(panel_width(160), Some(72));
}

/// The panel and the LANDS cell say the same thing, so exactly one of them
/// is on screen at any width. This is the invariant the whole ladder rests
/// on, and it is asserted at every width rather than at three of them.
#[test]
fn the_panel_and_the_lands_cell_are_never_both_present() {
    for width in view::MIN_TERM_WIDTH..=200 {
        let panel = panel_width(width).is_some();
        let (_, _, cost) = widths(body_width(width));
        assert!(
            !(panel && cost > 0),
            "both are drawn at {width} columns"
        );
    }
}

#[test]
fn the_panel_goes_below_ninety_columns() {
    assert_eq!(panel_width(90).is_some(), true);
    assert_eq!(panel_width(89), None);
}

#[test]
fn the_panel_never_drops_below_its_own_floor() {
    for width in 90..=200 {
        let panel = panel_width(width).expect("the panel is drawn above 89");
        assert!((50..=72).contains(&panel), "{panel} at {width}");
    }
}

#[test]
fn the_left_column_never_falls_below_its_own_floor() {
    for width in 90..=200 {
        let panel = panel_width(width).expect("the panel is drawn above 89");
        assert!(width - panel >= 40, "{} left at {width}", width - panel);
    }
}

/// Every width the pane can be drawn at draws inside itself. This is the
/// same sweep the existing pane tests run and it stays.
#[test]
fn every_row_fits_the_width_it_was_drawn_for() {
    for width in view::MIN_TERM_WIDTH..=200 {
        let app = fixtures::app_in_sheep_pane();
        for row in fixtures::draw_lines(&app, width, 48) {
            assert!(
                fixtures::render_all(&[row.clone()]).chars().count() <= usize::from(width),
                "a row overflows at {width}"
            );
        }
    }
}

#[test]
fn a_short_body_sheds_the_legend_before_the_tab_row() {
    let app = fixtures::app_in_sheep_pane();
    let rows = fixtures::render_all(&fixtures::draw_lines(&app, 160, 12));
    assert!(rows.contains("tab next group"), "the tab row must survive");
    assert!(!rows.contains("changed by you"), "the legend must go first");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace --lib --bins --all-features view::pane`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
/// The narrowest left column that still draws a marker, a key and a value.
const LEFT_MIN: u16 = 40;

/// The narrowest panel that holds a wrapped blurb and a validation list.
const PANEL_MIN: u16 = 50;

/// The panel at the design target, and the ceiling everywhere else.
const PANEL_MAX: u16 = 72;

/// How wide the explanation panel is, and [`None`] when it does not draw.
///
/// 45% of 160 is 72, so the design target falls out of the formula rather
/// than being special cased.
fn panel_width(width: u16) -> Option<u16> {
    let wanted = (u32::from(width) * 45 / 100) as u16;
    let panel = wanted.clamp(PANEL_MIN, PANEL_MAX);
    (width.saturating_sub(panel) >= LEFT_MIN).then_some(panel)
}
```

`pane_lines` calls it once and passes the remainder to the existing `widths`. When the panel draws, `widths` is called with the COST cell suppressed. When it does not, `widths` is called unchanged.

Body rows shed in order: legend and its hairline, provenance row, header row, then the existing `cursor_only` floor.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace --lib --bins --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 5: Prove each test pins something**

Mutate and confirm red. `the_panel_and_the_lands_cell_are_never_both_present` must fail the moment the COST suppression is dropped; check that by removing it.

- [ ] **Step 6: Commit**

```bash
git add -A crates/shep-cli/src/lookout/view/
git commit -m "feat(cli): trade the config pane's cost cell for its panel"
```

---

### Task 11: Scenes and the gallery

**Files:**
- Modify: `crates/shep-cli/src/lookout/frames.rs` (the `scenes!` list near line 162, `scene_with` near line 624)
- Create: four files under `crates/shep-cli/src/lookout/snapshots/`

**Interfaces:**
- Consumes: everything above.
- Produces: four `Scene` variants and their pinned snapshots.

- [ ] **Step 1: Add the scenes**

Four variants, each reached by real key presses the way `Scene::Bleats` is, never by reaching into the pane:

| Variant | What it shows | How it is reached |
|---|---|---|
| `EditPane` | a fresh pane at 160x48 | `KeyPress::Edit` |
| `EditPaneEdited` | two filed edits, one of them a respawn | `Edit`, then `Cycle`, then `Group(4)` and a typed value |
| `EditPaneSqueezed` | 120 columns, panel present, no cost cell | `Edit` at 120 wide |
| `EditPaneNarrow` | 88 columns, no panel, cost cell back | `Edit` at 88 wide |

Add a `label()` and `caption()` arm for each, matching the existing ones.

- [ ] **Step 2: Run the pinned test and accept the snapshots**

Run: `cargo test --workspace --lib --bins --all-features frames::`
Expected: FAIL first, with four new snapshots offered. Read each rendered frame against `docs/lookout/design-files/screenshots/1e-editing.png` before accepting. Check by eye: the tab row names eight groups, the panel is 72 columns at 160, the legend line has no `!` claiming to mean "changed by you", and no row overflows.

- [ ] **Step 3: Regenerate the gallery**

```bash
cargo test -p shep --lib --all-features -- --ignored write_the_gallery
```

This is the one place the plan's single cargo shape is broken, because the ignored test is named that way in the repo's own docs. It writes `docs/lookout/frames.txt` and `docs/lookout/frames.ansi`, and it is a write rather than a check, so the rebuild it causes is paid once.

- [ ] **Step 4: Commit**

```bash
git add crates/shep-cli/src/lookout/frames.rs crates/shep-cli/src/lookout/snapshots/ docs/lookout/frames.txt docs/lookout/frames.ansi
git commit -m "test(cli): pin the editing pane at four widths"
```

---

### Task 12: Docs

**Files:**
- Modify: `docs/lookout/README.md`
- Modify: whichever of `web/src/pages/docs/*.astro` mention the config pane or the lookout keymap
- Modify: `web/src/content/docs/cli-reference` output, only if the generator moves it

- [ ] **Step 1: Write the `What 1e settled` section**

Append to `docs/lookout/README.md`, matching the five sections already there in shape and length. Cover: nothing is written until the pane closes, `u` undoes the newest edit, `tab` and the digits move between eight groups, env keys live in the list and never show a value, read-only refuses the first edit rather than the close, and the panel and the cost cell are never both on screen.

No em dashes. Bullets, one claim each.

- [ ] **Step 2: Grep the site before assuming it is fine**

```bash
grep -rn "config pane\|lookout\|arm a confirm\|env sub-screen" web/src/pages/docs/
```

Read every hit. The arm-then-confirm sentence is now wrong for config edits and right for `x`, `R` and `L`, so it needs splitting rather than deleting.

- [ ] **Step 3: Regenerate the CLI reference**

```bash
cargo build --release
```
```bash
./web/scripts/generate-cli-reference.sh
```

Then `git diff`. No verb or flag moved, so this should produce nothing. If it does, read what changed before committing it.

- [ ] **Step 4: Build and check the site**

```bash
cd web && npx astro build
```
```bash
cd web && npx astro check
```

Both. `check` is the one that catches a wrong prop, and `build` stays green while a component silently loses a variant class.

- [ ] **Step 5: Commit**

```bash
git add docs/lookout/README.md web/
git commit -m "docs(lookout): describe the editing pane and its batched write"
```

---

## Final gate

Run these one at a time, each from its own command, never through a pipe:

```bash
cargo fmt --all --check
```
```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```
```bash
cargo test --workspace --all-features
```
```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
```

Then the phase checks, which the local gate does not cover:

```bash
cargo check -p shep-daemon --all-targets --all-features --target x86_64-unknown-linux-gnu
```
```bash
cargo check --workspace --all-targets --all-features --target x86_64-pc-windows-gnu
```

## Review

Task 5 deletes an established write path and its tests, and Task 8 deletes a whole sub-screen. Both exceed the 200-line deletion mark, so each gets two reviewers with distinct lenses rather than one: a spec lens reading the diff against this plan, and an empirical lens that builds the binary, runs `shep lookout` against a real shepherd under a short `SHEP_HOME`, opens the pane on a real sheep, files two edits, closes it, and confirms the writes landed. Reading a diff cannot catch a pane that no longer opens.

A short `SHEP_HOME` matters: the control socket path has to fit `SUN_LEN`, so use `mktemp -d` rather than a scratchpad path.
