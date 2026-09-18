# Lookout pane design pass Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the eight decisions of the lookout pane design pass: dogs in their own dashboard section, `e` on a dog row, a `CFG` legend, eight field groups, an array list sub-screen, schema-declared suggestions, `--allow-control` inverted, and a menu on close when config is parked.

**Architecture:** Everything lives in `crates/shep-cli/src/lookout/` except the field regrouping, which is schema annotations in `crates/shep-core/src/config/app.rs` plus one const in `scaffold.rs`. Nothing changes on the wire: the one decision that looked like it needed a field reads data `ProcessInfo::dog` already carries, and an array value travels through `SetSheepField`'s existing `serde_json::Value`.

**Tech Stack:** Rust 2024, MSRV 1.88. ratatui for the TUI, insta for snapshot tests, schemars for the Flockfile JSON Schema.

**Spec:** [docs/brainstorming/specs/2026-09-05-pane-design-pass.md](../../brainstorming/specs/2026-09-05-pane-design-pass.md)

## Global Constraints

- **No em dashes anywhere**, including code comments and commit messages. Not `--` used as punctuation either. Use a comma, a colon, a period, or parentheses.
- **IR-47 governs comments**: a comment says only what the code cannot. Never history, never a rejected alternative, never a review argument, never a paraphrase of the next line. `//` is one or two lines, four at most. `///` is one summary line plus at most six body lines, twelve counting `# Errors`. No capitals for emphasis. A test whose name is a sentence needs no doc line.
- **Invoke the `shep-idiomatic-rust` skill before writing any Rust here.**
- **Conventional commit subjects**, one per task: `type(scope): summary`. Accepted types are feat, fix, perf, refactor, docs, test, ci, chore, style. Never `revert` or `build`. A `!` on the commit that breaks something.
- **Never write the maintainer's real name, personal email, or any absolute home directory path** into a committed file or commit message. Paths are repo-relative.
- **Vocabulary is deliberate**: `sheep` is one managed process, `flock` is the plural and never "sheeps", `dog` is a plugin process, `lamb` is a child of a sheep, `bleats` are logs, `shepherd` is the daemon, `lookout` is the dashboard.
- **One cargo command at a time.** The workspace shares one target-dir build lock.
- Inner loop: `cargo test -p shep --lib --all-features -- --skip ::slow::`
- Task gate, each with `$?` captured directly and never through a pipe: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`.
- **`web/` is published and part of the deliverable.** Any change to what an operator types or sees needs `web/src/pages/docs/lookout.astro` read and updated, then `cd web && npx astro build` and `cd web && npx astro check`, both exit 0.

---

## File Structure

**Changed:**

- `crates/shep-cli/src/lookout/app.rs`: `RowKey` gains a `Section` variant; `visible_rows` splices headers; the four movement handlers skip them; `ask_for_config` stops refusing a dog; `arm`'s control check inverts; a new close-menu state and its key handling.
- `crates/shep-cli/src/lookout/view/flock.rs`: renders a section header row.
- `crates/shep-cli/src/lookout/view/status.rs`: `hint_for` gains the `CFG` legend; `pane_hint` and `hint_for` lose their `Control::ReadOnly` asymmetry where it was only about the flag's default.
- `crates/shep-cli/src/lookout/field.rs`: `FieldKind::Suggested`, `FieldKind::List`, and the `init.suggest` reader.
- `crates/shep-cli/src/lookout/pane.rs`: a `ListPane` mirroring `EnvPane`; `cycle` and `begin_typing` learn `Suggested`; a parked-count accessor; the reload-mode computation.
- `crates/shep-cli/src/lookout/view/pane.rs`: renders the list sub-screen and the close menu.
- `crates/shep-cli/src/lookout/mod.rs`: `resolve_control` inverts.
- `crates/shep-cli/src/cli.rs`: `--allow-control` becomes `--read-only`.
- `crates/shep-core/src/config/app.rs`: `init.group` values change on 39 fields; `init.suggest` added to two.
- `crates/shep-core/src/config/scaffold.rs`: `GROUP_ORDER` gains four names.
- `web/src/pages/docs/lookout.astro`: the keys, the legend, and the flag.

**Not changed, deliberately:** every file under `crates/shep-core/src/protocol/`. This plan adds no wire surface.

---

## Task 1: dogs get their own dashboard section

**Files:**
- Modify: `crates/shep-cli/src/lookout/app.rs` (`RowKey` at 611, `visible_rows` at 3213, `select_at` at 3287, `select_by` at 3273, `reseat` at 3252, `selected_index` at 3379)
- Modify: `crates/shep-cli/src/lookout/view/flock.rs` (render loop feeds from `view/mod.rs:275`)
- Test: same files, inline `mod tests`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `RowKey::Section(&'static str)`, a non-selectable row. Task 2 relies on `selected_row()` still returning `None` for it, as it already does for `RowKey::Group`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_flock_with_a_dog_draws_a_section_header_before_each_kind() {
    let mut app = fixtures::app_with_a_dog();
    let rows = app.visible_rows();
    assert_eq!(rows.first(), Some(&RowKey::Section("Flock")), "{rows:?}");
    let dogs = rows
        .iter()
        .position(|row| *row == RowKey::Section("Dogs"))
        .unwrap_or_else(|| panic!("no dogs header: {rows:?}"));
    // Every sheep sorts above the header and every dog below it.
    assert!(rows[..dogs].iter().all(|row| !app.is_dog_row(row)), "{rows:?}");
    assert!(rows[dogs + 1..].iter().all(|row| app.is_dog_row(row)), "{rows:?}");
}

#[test]
fn a_flock_with_no_dog_draws_no_dogs_header() {
    let app = fixtures::started().0;
    let rows = app.visible_rows();
    assert!(!rows.contains(&RowKey::Section("Dogs")), "{rows:?}");
}

#[test]
fn moving_down_steps_over_a_section_header() {
    let mut app = fixtures::app_with_a_dog();
    app.select_at(1);
    let before = app.selected().cloned();
    // Walk the whole list; a header must never become the selection.
    for _ in 0..app.visible_rows().len() + 2 {
        let _ = app.update(Msg::Key(KeyPress::Down));
        assert!(
            !matches!(app.selected(), Some(RowKey::Section(_))),
            "landed on a header from {before:?}"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p shep --lib --all-features a_flock_with_a_dog_draws_a_section`
Expected: FAIL, `no variant named Section found for enum RowKey`.

- [ ] **Step 3: Add the variant**

In `app.rs`, beside `Sheep(u32)` and `Group(String)`:

```rust
    /// A header, never selectable. `&'static str` because the only two are
    /// written here.
    Section(&'static str),
```

- [ ] **Step 4: Splice the headers in `visible_rows`**

The function already sorts by `(name, instance, id)` and splices a `RowKey::Group` before a grouped app's instances. Partition by `row.info.dog.is_some()` first, then emit `Section("Flock")`, the sheep, `Section("Dogs")`, the dogs. Emit a header only when its side is non-empty.

Add the helper the test uses:

```rust
    /// Whether `row` names a dog. A header and a group row are neither.
    #[must_use]
    pub fn is_dog_row(&self, row: &RowKey) -> bool {
        match row {
            RowKey::Sheep(id) => self.flock.get(id).is_some_and(|r| r.info.dog.is_some()),
            RowKey::Group(_) | RowKey::Section(_) => false,
        }
    }
```

- [ ] **Step 5: Make the four movement handlers skip a header**

`select_at` and `select_by` are the only two that index into `visible_rows`. Give `select_at` a direction to search when it lands on a header, and have `select_by` pass its own sign. `reseat` reuses `select_at`, so it is covered. A list of nothing but headers cannot happen, since a header is only emitted when its side is non-empty.

- [ ] **Step 6: Render the header**

In `view/flock.rs`, add a `section_line` beside `group_line` and `row_line`, and dispatch to it from `key_line`. Draw the label followed by a rule to the table width, using `palette.muted()`.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p shep --lib --all-features -- --skip ::slow::`
Expected: PASS. `frames_are_pinned` will fail on any scene with both kinds on screen; that is step 8.

- [ ] **Step 8: Accept the snapshots and regenerate the gallery**

Read every changed snapshot before accepting it, and confirm the header lands where the test says. Then:

```bash
cargo insta accept
```
```bash
cargo test -p shep --lib --all-features -- --ignored write_the_gallery
```

- [ ] **Step 9: Commit**

```bash
git add crates/shep-cli/src/lookout crates/shep-cli/src/lookout/snapshots docs/lookout
git commit -m "feat(lookout): dogs get their own section of the flock table"
```

---

## Task 2: `e` opens a dog's config pane from the dashboard

**Files:**
- Modify: `crates/shep-cli/src/lookout/app.rs` (`ask_for_config` at 2315)
- Test: same file

**Interfaces:**
- Consumes: `RowKey::Section` from Task 1, only in that a header is never the selection.
- Produces: nothing later tasks read.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn e_on_a_dog_row_opens_its_pane_instead_of_refusing() {
    let mut app = fixtures::app_with_a_dog_selected_and_control();
    let effect = app.update(Msg::Key(KeyPress::Edit));
    let Effect::LoadDogPane { name, adopted_path } = effect else {
        panic!("expected a dog pane, got {effect:?}");
    };
    assert_eq!(name, "otel");
    // The path comes off the row, not the settings screen.
    assert_eq!(adopted_path.as_deref(), Some(Path::new("/opt/otel")));
    assert!(app.notice().is_none(), "{:?}", app.notice());
}

#[test]
fn e_on_a_built_in_dog_opens_a_pane_with_no_path() {
    let mut app = fixtures::app_with_a_built_in_dog_selected_and_control();
    let effect = app.update(Msg::Key(KeyPress::Edit));
    let Effect::LoadDogPane { adopted_path, .. } = effect else {
        panic!("expected a dog pane, got {effect:?}");
    };
    // A built-in dog is the shep binary's own argv branch, so there is no
    // adopted path to probe and the pane asks the running binary instead.
    assert_eq!(adopted_path, None);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p shep --lib --all-features e_on_a_dog_row_opens`
Expected: FAIL with the current refusal notice, `otel is a dog; press \`s\` for settings, then \`e\` on its row`.

- [ ] **Step 3: Read the path off the row**

Replace the refusal branch in `ask_for_config` with a `DogSource` match. `DogSource` is `#[non_exhaustive]`, so the match needs a `_` arm; treat an unknown source as having no path rather than refusing, since a newer daemon adding a source is not a reason to close the pane.

```rust
        if let Some(source) = row.info.dog.as_ref() {
            let adopted_path = match source {
                DogSource::Adopted { path } => Some(PathBuf::from(path)),
                DogSource::BuiltIn => None,
                _ => None,
            };
            return Effect::LoadDogPane { name: row.info.name.clone(), adopted_path };
        }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p shep --lib --all-features -- --skip ::slow::`
Expected: PASS. The old test asserting the refusal must be deleted, not weakened; its behaviour is gone.

- [ ] **Step 5: Commit**

```bash
git add crates/shep-cli/src/lookout/app.rs
git commit -m "feat(lookout): e opens a dog's pane from the dashboard"
```

---

## Task 3: the `CFG` column gets a legend

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/status.rs` (`hint_for` at 309)
- Test: same file

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_dashboard_hint_says_what_the_cfg_glyphs_mean() {
    for control in [Control::ReadOnly, Control::Allowed] {
        let hint = hint_for(control, false);
        assert!(hint.contains("* yours"), "{hint}");
        assert!(hint.contains("! parked"), "{hint}");
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p shep --lib --all-features the_dashboard_hint_says_what_the_cfg`
Expected: FAIL, the hint has no `* yours`.

- [ ] **Step 3: Add the legend to both dashboard arms**

Append `   * yours   ! parked` to the two non-settings arms of `hint_for`. Leave the settings arms alone; that screen has no `CFG` column.

The read-only arm's first 40 bytes must stay byte-identical, per the constraint documented above `hint_for`. Appending satisfies that; do not reorder.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p shep --lib --all-features -- --skip ::slow::`
Expected: PASS, with hint snapshots to accept.

- [ ] **Step 5: Accept the snapshots and regenerate the gallery**

```bash
cargo insta accept
```
```bash
cargo test -p shep --lib --all-features -- --ignored write_the_gallery
```

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout docs/lookout
git commit -m "docs(lookout): the dashboard hint names the CFG glyphs"
```

---

## Task 4: the field groups go from four to eight

**Files:**
- Modify: `crates/shep-core/src/config/app.rs` (the `schemars(extend("init" = ...))` attribute on each field)
- Modify: `crates/shep-core/src/config/scaffold.rs` (`GROUP_ORDER` at 84)
- Modify: `crates/shep-cli/src/lookout/field.rs` (the two tests at 543 and 563)
- Regenerate: `crates/shep-core/assets/flockfile.schema.json`

**Interfaces:**
- Produces: `GROUP_ORDER` with eight names, read by `FieldSet::from_properties` and by `scaffold::grouped_order`.

- [ ] **Step 1: Write the failing test**

Replace `the_real_flockfile_schema_yields_thirty_nine_fields_in_four_groups` in `field.rs`:

```rust
    #[test]
    fn the_real_flockfile_schema_yields_thirty_nine_fields_in_eight_groups() {
        let set = real_field_set();
        assert_eq!(set.len(), 39);
        assert_eq!(
            groups_of(&set),
            [
                "process", "logging", "inputs", "restart", "readiness", "shutdown",
                "watch", "cron"
            ]
        );
    }

    #[test]
    fn no_group_holds_more_than_a_third_of_the_fields() {
        let set = real_field_set();
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for field in set.fields() {
            *counts.entry(field.group.as_deref().unwrap_or("")).or_default() += 1;
        }
        let (worst, count) = counts.iter().max_by_key(|(_, n)| **n).expect("fields exist");
        assert!(*count <= 13, "{worst} holds {count} of {}", set.len());
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p shep --lib --all-features yields_thirty_nine_fields`
Expected: FAIL, the group list is the old four.

- [ ] **Step 3: Change `GROUP_ORDER`**

```rust
pub const GROUP_ORDER: &[&str] = &[
    "process", "logging", "inputs", "restart", "readiness", "shutdown", "watch", "cron",
];
```

- [ ] **Step 4: Move each field to its group**

Edit the `"group"` value inside each field's `schemars(extend("init" = ...))` in `app.rs`, per the spec's table. Only the `"group"` string changes; leave `"blurb"` and `"example"` alone.

`logging` takes `out_file`, `err_file`, `merge_logs`. `restart` takes `autostart`, `autorestart`, `max_restarts`, `min_uptime`, `restart_delay`, `exp_backoff_restart_delay`, `stop_exit_codes`, `max_memory`. `readiness` takes `readiness_probe`, `liveness_probe`, `wait_ready`, `listen_timeout`. `shutdown` takes `kill_signal`, `kill_timeout`, `graceful_timeout`, `shutdown_with_message`, `action_timeout`. `watch` takes `watch`, `watch_delay`, `ignore_watch`, `watch_options`. `process` keeps `name`, `script`, `interpreter`, `cwd`, `user`, `group`, `instances`, `fold`, `reuse_port`. `inputs` and `cron` are unchanged.

- [ ] **Step 5: Regenerate the exported schema**

```bash
cargo run --bin shep --features schema -- schema > crates/shep-core/assets/flockfile.schema.json
```

If that hidden verb's spelling differs, find it: `rg 'fn schema' crates/shep-cli/src/commands/`. Do not hand-edit the asset.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --workspace --all-features`
Expected: PASS. `every_field_carries_a_group_and_a_blurb` in `scaffold.rs` refuses a group not in `GROUP_ORDER`, so a typo fails there with the offending name.

- [ ] **Step 7: Commit**

```bash
git add crates/shep-core crates/shep-cli/src/lookout/field.rs
git commit -m "refactor(core): eight field groups, so no group holds half the form"
```

---

## Task 5: schema-declared suggestions

**Files:**
- Modify: `crates/shep-cli/src/lookout/field.rs` (`FieldKind` at 14, `field_from` at 292, `kind_of` at 244)
- Modify: `crates/shep-cli/src/lookout/pane.rs` (`cycle` at 786, `begin_typing` at 839)
- Modify: `crates/shep-core/src/config/app.rs` (two fields gain `"suggest"`)
- Regenerate: `crates/shep-core/assets/flockfile.schema.json`

**Interfaces:**
- Consumes: `GROUP_ORDER` from Task 4, unchanged in shape.
- Produces: `FieldKind::Suggested(Vec<String>)`, which both cycles and types.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn a_field_with_init_suggest_cycles_and_still_types() {
        let schema = json!({
            "type": ["string", "null"],
            "init": { "suggest": ["SIGTERM", "SIGINT"] }
        });
        let field = field_from("kill_signal", &schema, &Map::new());
        assert_eq!(
            field.kind,
            FieldKind::Suggested(vec!["SIGTERM".into(), "SIGINT".into()])
        );
        assert!(field.editable, "a suggestion is not a constraint");
    }

    #[test]
    fn kill_signal_and_cron_restart_both_carry_suggestions() {
        let set = real_field_set();
        for key in ["kill_signal", "cron_restart"] {
            let field = set.by_key(key).unwrap_or_else(|| panic!("no {key}"));
            assert!(
                matches!(field.kind, FieldKind::Suggested(ref names) if !names.is_empty()),
                "{key}: {:?}",
                field.kind
            );
        }
    }
```

And in `pane.rs`:

```rust
    #[test]
    fn space_cycles_a_suggested_field_and_e_still_opens_the_editor() {
        let mut pane = ConfigPane::sheep(web());
        pane.move_to_key("kill_signal");
        pane.cycle(Instant::now());
        let armed = pane.pending_edit().cloned();
        assert!(armed.is_some(), "space arms a suggestion");
        pane.cancel_pending();
        pane.begin_typing();
        assert!(pane.is_typing(), "e still opens a free-text editor");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p shep --lib --all-features a_field_with_init_suggest`
Expected: FAIL, no variant named `Suggested`.

- [ ] **Step 3: Add the variant**

```rust
    /// `init.suggest`. Cycles like a choice and types like text: the values
    /// are offered, not enforced, because the grammar stays open.
    Suggested(Vec<String>),
```

- [ ] **Step 4: Read `init.suggest` in `field_from`**

`kind_of` never sees `init`, so decide this in `field_from` after `kind_of` returns. Override only a `Text` kind: a suggestion on a bool or a map is a schema mistake and silently reinterpreting it would hide that.

```rust
    let kind = match (kind, suggestions(init)) {
        (FieldKind::Text, Some(names)) if !names.is_empty() => FieldKind::Suggested(names),
        (kind, _) => kind,
    };
```

with

```rust
/// The `init.suggest` values, when every entry is a string.
fn suggestions(init: Option<&Value>) -> Option<Vec<String>> {
    let values = init?.get("suggest")?.as_array()?;
    let names: Vec<String> = values.iter().filter_map(Value::as_str).map(str::to_owned).collect();
    (names.len() == values.len()).then_some(names)
}
```

- [ ] **Step 5: Teach the pane to cycle and to type it**

In `cycle`, give `Suggested` the same arm `Choice` has; the wrapping arithmetic is identical. In `begin_typing`, add `Suggested` to the kinds that open an editor, beside `Text` and `Integer`. `Choice` stays excluded.

- [ ] **Step 6: Add the suggestions to the schema**

On `kill_signal` in `app.rs`, add to its existing `init` map:

```rust
    "suggest": ["SIGTERM", "SIGINT", "SIGQUIT", "SIGUSR2"],
```

On `cron_restart`:

```rust
    "suggest": ["*/5 * * * *", "0 * * * *", "0 0 * * *", "0 0 * * 0"],
```

Then regenerate the asset as in Task 4, step 5.

- [ ] **Step 7: Verify the enum prohibition still holds**

Run: `cargo test -p shep-core --lib --all-features kill_signal_stays_an_unconstrained_string`
Expected: PASS. `init.suggest` is neither `enum` nor `pattern`, so this test must not move. If it fails, the change went in the wrong place.

- [ ] **Step 8: Run the full inner loop**

Run: `cargo test -p shep --lib --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/shep-core crates/shep-cli/src/lookout
git commit -m "feat(lookout): a field can offer suggestions without closing its grammar"
```

---

## Task 6: an array field opens a list sub-screen

**Files:**
- Modify: `crates/shep-cli/src/lookout/field.rs` (`kind_of` at 244)
- Modify: `crates/shep-cli/src/lookout/pane.rs` (a `ListPane` beside `EnvPane` at 297)
- Modify: `crates/shep-cli/src/lookout/app.rs` (`on_pane_key` at 2378, a new `on_list_key`)
- Modify: `crates/shep-cli/src/lookout/view/pane.rs` (a `list_lines` beside `env_lines` at 245)
- Test: all four

**Interfaces:**
- Consumes: `FieldKind` from Task 5, which this extends again.
- Produces: `FieldKind::List(ListItem)` where `ListItem` is `Text` or `Integer`; `ConfigPane::list() -> Option<&ListPane>`, the open-state test, mirroring `env()`.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn an_array_of_strings_is_a_list_and_an_array_of_integers_knows_its_item() {
        let set = real_field_set();
        assert_eq!(set.by_key("args").map(|f| f.kind.clone()), Some(FieldKind::List(ListItem::Text)));
        assert_eq!(
            set.by_key("stop_exit_codes").map(|f| f.kind.clone()),
            Some(FieldKind::List(ListItem::Integer))
        );
        assert!(set.by_key("args").is_some_and(|f| f.editable), "an array is editable now");
    }

    #[test]
    fn editing_one_element_sends_the_whole_array() {
        let mut pane = ConfigPane::sheep(web_with_args(&["--port", "8080"]));
        pane.move_to_key("args");
        pane.open_list();
        pane.list_mut().expect("open").move_to(1);
        pane.arm_list_element("9090".into(), Instant::now());
        let Some(PaneEdit::Set { key, value }) = pane.take_armed(1).map(|e| e) else {
            panic!("expected a set");
        };
        assert_eq!(key, "args");
        assert_eq!(value.as_value(), &json!(["--port", "9090"]));
    }

    #[test]
    fn the_list_screen_shows_its_values_unlike_env() {
        let pane = ConfigPane::sheep(web_with_args(&["--port"]));
        let lines = rendered_list(&pane, 120, 20);
        assert!(lines.iter().any(|l| l.contains("--port")), "{lines:?}");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p shep --lib --all-features an_array_of_strings_is_a_list`
Expected: FAIL, no variant named `List`.

- [ ] **Step 3: Classify an array**

In `field.rs`, add `ListItem` and give `kind_of` an `"array"` arm reading `items`:

```rust
/// What an array's elements are, so the editor can parse one back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListItem {
    Text,
    Integer,
}
```

```rust
        Some("array") => match type_of(&strip_nullable(resolve(items(schema), defs), defs)) {
            Some("string") => FieldKind::List(ListItem::Text),
            Some("integer") => FieldKind::List(ListItem::Integer),
            _ => FieldKind::Opaque,
        },
```

An array of anything else stays `Opaque`, which is what keeps a nested-object array read-only rather than half-editable.

- [ ] **Step 4: Add `ListPane`, mirroring `EnvPane`**

Copy `EnvPane`'s shape: a `Vec` of elements, a `Viewport`, a `rows()` returning one `ListRow::Item(usize)` per element plus a trailing `ListRow::New`, a `cursor()`, and a typed buffer. The one difference from env is that a row renders its value.

Its `Debug` is derived rather than redacted. An array field is not a secret, unlike `EnvPane`'s buffer. Say that in the type's doc, and pin it with an exact-string test the way the env types are pinned.

- [ ] **Step 5: Wire the keys**

In `on_pane_key`, dispatch to `on_list_key` when `pane.list().is_some()`, ahead of the env check, mirroring `app.rs:2379`. `on_list_key` handles `Escape` (close one level), movement, `e`/`Enter` (edit the element under the cursor, or add on the `New` row), `d` (remove), and `K`/`J` (move up and down). Arming and sending reuse the existing `PaneEdit::Set` path, since the whole array goes out as one value.

- [ ] **Step 6: Render it**

Add `list_lines` beside `env_lines` in `view/pane.rs`, using the same `scroll::to_cursor` walker and the same counted budget. Do not add a line the budget does not count.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p shep --lib --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 8: Prove one test non-vacuous**

Revert step 3's `"array"` arm so arrays fall back to `Opaque`, re-run `an_array_of_strings_is_a_list`, watch it fail, restore. Note in the commit what the failure said.

- [ ] **Step 9: Commit**

```bash
git add crates/shep-cli/src/lookout
git commit -m "feat(lookout): an array field opens a list sub-screen"
```

---

## Task 7: `--allow-control` inverts to `--read-only`

**Files:**
- Modify: `crates/shep-cli/src/cli.rs` (`LookoutArgs` at 1094)
- Modify: `crates/shep-cli/src/lookout/mod.rs` (`resolve_control` at 203)
- Modify: `web/src/pages/docs/lookout.astro`
- Test: `crates/shep-cli/src/lookout/mod.rs` inline tests at 698

**Interfaces:**
- Produces: `resolve_control(read_only: bool, kv: &Path) -> Control`, the argument's meaning inverted.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn control_is_allowed_when_nothing_says_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let kv = dir.path().join("kv.json");
        assert_eq!(resolve_control(false, &kv), Control::Allowed);
    }

    #[test]
    fn the_flag_and_the_key_can_each_ask_for_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let kv = dir.path().join("kv.json");
        assert_eq!(resolve_control(true, &kv), Control::ReadOnly);

        shep_core::kv::set(&kv, "lookout.allow_control", "false").unwrap();
        assert_eq!(resolve_control(false, &kv), Control::ReadOnly);
    }

    #[test]
    fn an_unreadable_store_leaves_control_allowed() {
        // Fails open now, deliberately: the gate stops an accident, not an
        // attacker, and a broken store is not a reason to refuse every key.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_control(false, &dir.path().join("missing.json")), Control::Allowed);
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p shep --lib --all-features control_is_allowed_when_nothing`
Expected: FAIL, `left: ReadOnly, right: Allowed`.

- [ ] **Step 3: Invert the resolver**

```rust
pub fn resolve_control(read_only: bool, kv: &Path) -> Control {
    if read_only {
        return Control::ReadOnly;
    }
    match shep_core::kv::get(kv, "lookout.allow_control") {
        Ok(Some(value)) if value == "false" => Control::ReadOnly,
        _ => Control::Allowed,
    }
}
```

- [ ] **Step 4: Rename the flag**

```rust
    /// Close the dashboard's action gate. Actions are permitted by default.
    #[arg(long)]
    pub read_only: bool,
```

Update the call site at `mod.rs:119`. Do not keep `--allow-control` as a hidden alias: an operator who passes it should be told it is gone, and clap's unknown-argument error does that.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --workspace --all-features`
Expected: PASS, with hint and gallery snapshots to accept, since scenes rendered under `ReadOnly` now render allowed unless the scene asks otherwise. Read each before accepting.

- [ ] **Step 6: Update the docs and regenerate the CLI reference**

Edit `web/src/pages/docs/lookout.astro` where the flag is named. Then:

```bash
cargo build --release
```
```bash
./web/scripts/generate-cli-reference.sh
```
```bash
cd web && npx astro build
```
```bash
cd web && npx astro check
```

- [ ] **Step 7: Commit**

```bash
git add crates/shep-cli web docs/lookout
git commit -m "feat(lookout)!: control is on by default, and --read-only opts out"
```

The `!` is required. This changes shipped behaviour, and release-plz reads the individual commit, never the pull request title.

---

## Task 8: a menu on close when config is parked

**Files:**
- Modify: `crates/shep-cli/src/lookout/pane.rs` (`pending` at 502, `is_pending` at 1172)
- Modify: `crates/shep-cli/src/lookout/app.rs` (`on_pane_key`'s `Escape` arm at 2404, `close_pane` at 2481)
- Modify: `crates/shep-cli/src/lookout/view/pane.rs`
- Test: all three

**Interfaces:**
- Consumes: nothing from earlier tasks.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn escape_closes_a_pane_with_nothing_parked() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none(), "no menu when nothing is parked");
    }

    #[test]
    fn escape_on_a_parked_pane_offers_the_menu_and_escape_again_leaves() {
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_some(), "the pane stays up behind the menu");
        assert!(app.pane_menu().is_some(), "the menu is open");

        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none(), "escape twice leaves");
    }

    #[test]
    fn the_menu_counts_the_parked_fields_once() {
        let app = fixtures::app_in_sheep_pane_with_two_parked_fields();
        let pane = app.config_pane().expect("a pane");
        assert_eq!(pane.parked_count(), 2);
    }

    #[test]
    fn a_probe_without_reuse_port_is_the_only_serial_reload() {
        // `wait_ready` answers before `readiness_probe` is read, so an app
        // with both still overlaps.
        assert_eq!(reload_mode(false, true, false), ReloadKind::Serial);
        assert_eq!(reload_mode(true, true, false), ReloadKind::Overlap);
        assert_eq!(reload_mode(false, true, true), ReloadKind::Overlap);
        assert_eq!(reload_mode(false, false, false), ReloadKind::Overlap);
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p shep --lib --all-features escape_on_a_parked_pane_offers`
Expected: FAIL, no method named `pane_menu`.

- [ ] **Step 3: Add the parked count**

```rust
    /// How many fields wait for a reload or a restart.
    #[must_use]
    pub fn parked_count(&self) -> usize {
        self.pending.len()
    }
```

- [ ] **Step 4: Add the reload-mode computation**

Mirror the daemon's rule exactly, reading the pane's own values map:

```rust
/// Whether a reload of this app overlaps or runs serially.
///
/// The daemon decides through `ReadinessSource::of`, which answers for
/// `wait_ready` before it reads `readiness_probe`, so an app with both
/// overlaps.
fn reload_mode(wait_ready: bool, has_probe: bool, reuse_port: bool) -> ReloadKind {
    if !wait_ready && has_probe && !reuse_port {
        ReloadKind::Serial
    } else {
        ReloadKind::Overlap
    }
}
```

Read `has_probe` as presence in the values map, not through `display_value`, which stringifies the whole object.

- [ ] **Step 5: Add the menu state and its keys**

A `PaneMenu` on `App`, opened from `on_pane_key`'s `Escape` arm when `pane.parked_count() > 0` and the help overlay is not open. Its keys: `L` reload, `R` restart, `Escape` leave. Reload and restart build the same `Sent::Action` the dashboard's `arm` and `confirm` already build, so the reply path at `on_action_reply` is unchanged.

- [ ] **Step 6: Render it**

Draw into the fixed line under the title, the slot the confirm and help already share, and give the menu precedence over both while it is open. That slot is already counted in the height budget, so nothing in `view/scroll.rs` moves.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p shep --lib --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 8: Prove the silent case non-vacuous**

Make the menu open unconditionally, re-run `escape_closes_a_pane_with_nothing_parked`, watch it fail, restore. This is the assertion that keeps reading a pane free.

- [ ] **Step 9: Add a pinned scene**

Add a `Scene` variant rendering the menu on a parked pane, then:

```bash
cargo insta accept
```
```bash
cargo test -p shep --lib --all-features -- --ignored write_the_gallery
```

- [ ] **Step 10: Commit**

```bash
git add crates/shep-cli/src/lookout docs/lookout
git commit -m "feat(lookout): closing a pane with parked config offers to apply it"
```

---

## Final gate

Run each from its own command with `$?` captured directly, never through a pipe.

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
```bash
cd web && npx astro build
```
```bash
cd web && npx astro check
```

---

## Self-review

**Spec coverage.** Decision 1 is Task 1. Decision 2 is Task 2. Decision 3 is Task 7. Decision 4 is Task 3. Decision 5 is Task 4. Decision 6 is Task 6. Decision 7 is Task 5. Decision 8 is Task 8. The spec's "what this does not do" needs no task: `cron_timezone` gains no suggestions because Task 5 adds `suggest` to two named fields only, and nested objects stay `Opaque` because Task 6's array arm falls through for a non-scalar item.

**Placeholders.** None. Every code step carries the code.

**Type consistency.** `FieldKind` gains `Suggested(Vec<String>)` in Task 5 and `List(ListItem)` in Task 6, and both tasks add an arm to the same `cycle` match, so Task 6 must not delete Task 5's. `RowKey::Section(&'static str)` is used by Task 1 and only read by Task 2. `reload_mode`'s three parameters are in the order `wait_ready, has_probe, reuse_port` in both the test and the function.

**Ordering.** Task 5 must land before Task 6, since both extend `FieldKind` and `kind_of`. Task 4 must land before Task 5, since Task 5's schema regeneration would otherwise carry Task 4's group changes into an unrelated commit. Tasks 1 through 3 and Tasks 7 and 8 are independent of the rest and of each other.
