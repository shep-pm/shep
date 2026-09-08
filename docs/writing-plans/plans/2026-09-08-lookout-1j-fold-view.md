# Lookout 1j: the fold view Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Gather the same flock under fold headers instead of listing it flat, and let one keypress act on a whole fold.

**Architecture:** A grouping mode on the existing flock table rather than a new pane. The table already builds a two-level row list for multi-instance apps, so a fold header is that shape with a different key, and selection, scrolling, confirms and the detail pane keep working.

**Tech Stack:** Rust 1.88, edition 2024, ratatui 0.30.2, insta snapshots.

**Spec:** [docs/brainstorming/specs/2026-09-08-lookout-1i-1j-design.md](../../brainstorming/specs/2026-09-08-lookout-1i-1j-design.md)

## Global Constraints

- No protocol change. `SelectorSpec::Fold` already ships, so this is a view over a capability that exists. No `PROTOCOL_VERSION`, `MIN_SUPPORTED` or `SCHEMA_VERSION` movement.
- A fold header's uptime is the **minimum** across members, matching `group_totals` (`app.rs:4039`, whose own doc says the minimum). Do not invent a second rollup rule.
- Two levels of grouping, never three. An app inside a fold stays one row carrying its own `×N`.
- Invoke the `shep-idiomatic-rust` skill before writing Rust. Cite `IR-<n>` where a rule applies.
- Every new public item needs a doc comment and a deliberate `Debug` decision.
- Conventional commit subjects. Nothing here breaks, so no `!`.
- ONE cargo shape: `--workspace`. Do not alternate with `-p <crate>`; this repo's cache churns badly when a brief names two shapes.
- Iterate with `cargo test --workspace --all-features --lib --bins`. Run the full `cargo test --workspace --all-features` once per task before committing.

## Two facts that shape the whole plan

**`Column::Fold` already exists** (`flock.rs:91-131`, width 10) and there is already a `NO_FOLD` tier at width 89. That is the per-row column printing one sheep's fold name, and it has nothing to do with grouping. **Do not reuse or rename it.** The fold view's 24-cell `FOLD / NAME` column is a different thing and needs its own name.

**`SelectorSpec::Fold(String)` can say `fold:edge` and cannot say "everything with no fold".** That decides Task 2: a real fold header is selectable and actionable, and the `no fold` header is not, because there is no selector that names its members and an action would have to enumerate ids behind the operator's back.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/shep-cli/src/lookout/app.rs` | the grouping state, `RowKey::Fold`, the fold row list, the fold rollup |
| `crates/shep-cli/src/lookout/view/flock.rs` | the fold column set, its tier ladder, the header row |
| `crates/shep-cli/src/lookout/view/detail.rs` | the fold branch |
| `crates/shep-cli/src/lookout/view/status.rs` | the confirm text for a fold |
| `crates/shep-cli/src/lookout/input.rs` | binding `F` and `z` |
| `crates/shep-cli/src/lookout/frames.rs` | the scene |

---

## Task 1: The grouping state and the fold row list

**Files:**
- Modify: `crates/shep-cli/src/lookout/app.rs` (`RowKey` at :624, `visible_rows` at :3767, `push_grouped_rows` at :3799)
- Modify: `crates/shep-cli/src/lookout/input.rs` (:43-64)

**Interfaces:**
- Consumes: `ProcessInfo::fold: Option<String>` (`crates/shep-core/src/protocol/request.rs:654`).
- Produces: `RowKey::Fold(String)`; `Grouping::{Flat, ByFold}`; `App::grouping(&self) -> Grouping`; `visible_rows` returning fold-grouped rows when `ByFold`.

- [ ] **Step 1: Write the failing tests**

```rust
/// Sheep gather under their fold, unfoldered ones under a header that
/// names the situation, and dogs keep their own band because a dog
/// cannot carry a fold at all.
#[test]
fn by_fold_groups_sheep_under_their_fold() {
    let mut app = fixtures::app_with(
        vec![
            fixtures::sheep_in_fold(1, "api", Some("edge")),
            fixtures::sheep_in_fold(2, "cdn", Some("edge")),
            fixtures::sheep_in_fold(3, "batch", None),
        ],
        fixtures::plain(),
    );
    let _ = app.update(Msg::Key(KeyPress::FoldView));
    let rows = app.visible_rows();
    assert!(rows.iter().any(|r| matches!(r, RowKey::Fold(name) if name == "edge")));
    assert!(rows.iter().any(|r| matches!(r, RowKey::Section("no fold"))));
}

/// F toggles rather than opening, so pressing it twice is where it began.
#[test]
fn f_toggles_back_to_the_flat_list() {
    let mut app = fixtures::app_with(
        vec![fixtures::sheep_in_fold(1, "api", Some("edge"))],
        fixtures::plain(),
    );
    let flat = app.visible_rows();
    let _ = app.update(Msg::Key(KeyPress::FoldView));
    assert_ne!(app.visible_rows(), flat);
    let _ = app.update(Msg::Key(KeyPress::FoldView));
    assert_eq!(app.visible_rows(), flat);
}

/// Two levels, never three. A three-instance app inside a fold is one
/// member row keeping its own rollup, or `edge ×4` stops meaning
/// anything fixed.
#[test]
fn an_app_inside_a_fold_stays_one_row() {
    let mut app = fixtures::app_with(
        vec![
            fixtures::instance_in_fold(1, "web", 0, Some("edge")),
            fixtures::instance_in_fold(2, "web", 1, Some("edge")),
            fixtures::instance_in_fold(3, "web", 2, Some("edge")),
        ],
        fixtures::plain(),
    );
    let _ = app.update(Msg::Key(KeyPress::FoldView));
    let rows = app.visible_rows();
    let sheep = rows.iter().filter(|r| matches!(r, RowKey::Sheep(_))).count();
    assert_eq!(sheep, 0, "instances stay behind their app's row: {rows:?}");
    assert_eq!(rows.iter().filter(|r| matches!(r, RowKey::Group(_))).count(), 1);
}
```

Add `sheep_in_fold` and `instance_in_fold` to `view/fixtures.rs` beside `app_with` (`fixtures.rs:35`), building a `ProcessInfo` with the fold set.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --workspace --all-features --lib --bins -- by_fold_groups f_toggles_back an_app_inside_a_fold`
Expected: FAIL, no `KeyPress::FoldView` and no `RowKey::Fold`.

- [ ] **Step 3: Add the row key and the grouping state**

Extend `RowKey` at `app.rs:624`:

```rust
    /// One fold's header, carrying its name.
    ///
    /// Selectable and actionable, unlike [`Self::Section`], because
    /// `SelectorSpec::Fold` can name its members on the wire.
    Fold(String),
```

```rust
/// How the flock table gathers its rows.
///
/// `Debug` is derived; it is two words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Grouping {
    /// Every sheep in one list, apps rolled up by instance.
    #[default]
    Flat,
    /// Sheep gathered under their fold.
    ByFold,
}
```

The compiler will name every `match` on `RowKey` that needs the new arm. `Section` is guarded by `unreachable!` in several places (`app.rs:3506` and friends) because it is never selectable; `Fold` **is** selectable, so it needs a real arm at each of those rather than another `unreachable!`.

- [ ] **Step 4: Bind the key**

`F` is unbound today; confirm before adding. Add `KeyPress::FoldView` and bind `F` in the `Normal` arm at `input.rs:43-64`. Add a test pinning the binding, in the shape of the existing one that pins `z` as unbound (`input.rs:159`).

- [ ] **Step 5: Build the fold row list**

In `visible_rows` (`app.rs:3767`), branch on the grouping. The `ByFold` path:

- partition dogs out first, exactly as the flat path does, and keep `RowKey::Section("Dogs")` for them. Dogs cannot carry a fold: `dog_app` builds its config through `AppConfig::minimal`, which defaults `fold: None`, and `DogSpec` has no fold field at all.
- gather the remaining sheep by `info.fold`, each real fold under `RowKey::Fold(name)`, in name order.
- put sheep with no fold under `RowKey::Section("no fold")`, which is a header rather than a fold because it is not selectable and not actionable.
- inside a fold, reuse `push_grouped_rows` (`app.rs:3799`) unchanged so a multi-instance app keeps its own `Group` row and its instances stay behind it.

- [ ] **Step 6: Run to verify they pass**

Run: `cargo test --workspace --all-features --lib --bins -- by_fold_groups f_toggles_back an_app_inside_a_fold`
Expected: PASS.

- [ ] **Step 7: Full gate and commit**

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
git add crates/shep-cli/src/lookout/
git commit -m "feat(lookout): gather the flock table by fold on F"
```

---

## Task 2: The fold rollup, and acting on a fold

**Files:**
- Modify: `crates/shep-cli/src/lookout/app.rs` (`group_totals` at :4039, `arm` at :3473)
- Modify: `crates/shep-cli/src/lookout/view/status.rs` (`confirm_prompt` at :191)

**Interfaces:**
- Consumes: `RowKey::Fold` from Task 1; `GroupTotals` (`app.rs:373`).
- Produces: `App::fold_totals(&self, fold: &str) -> GroupTotals`; a `RowKey::Fold` arm in `arm` and in `confirm_prompt`.

- [ ] **Step 1: Write the failing tests**

```rust
/// The same rule group_totals uses, whose own doc calls uptime the
/// minimum. A second rollup rule would make two headers disagree about
/// the same numbers.
#[test]
fn a_fold_rolls_up_like_a_group_does() {
    let app = fixtures::app_with(
        vec![
            fixtures::sheep_with(1, "api", Some("edge"), 120_000, Some(100 << 20), 2),
            fixtures::sheep_with(2, "cdn", Some("edge"), 30_000, Some(150 << 20), 5),
        ],
        fixtures::plain(),
    );
    let totals = app.fold_totals("edge");
    assert_eq!(totals.count, 2);
    assert_eq!(totals.restarts, 7);
    assert_eq!(totals.memory, Some(250 << 20));
    assert_eq!(totals.uptime_ms, Some(30_000), "the minimum, not the first or the longest");
}

/// The confirm names the count so nobody stops four things believing
/// they stopped one.
#[test]
fn a_fold_confirm_states_how_many_it_reaches() {
    let mut app = fixtures::app_with(
        vec![
            fixtures::sheep_in_fold(1, "api", Some("edge")),
            fixtures::sheep_in_fold(2, "cdn", Some("edge")),
        ],
        fixtures::plain(),
    );
    app.set_control_for_tests(Control::Allowed);
    let _ = app.update(Msg::Key(KeyPress::FoldView));
    app.select_fold_for_tests("edge");
    let _ = app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
    let text = fixtures::status_text(&app);
    assert!(text.contains('2'), "the confirm must name the count: {text}");
    assert!(text.contains("edge"), "and the fold: {text}");
}

/// The no-fold header is a header, not a fold. There is no
/// SelectorSpec that names "everything with no fold", so an action there
/// would have to enumerate ids behind the operator's back.
#[test]
fn the_no_fold_header_is_not_selectable() {
    let mut app = fixtures::app_with(
        vec![fixtures::sheep_in_fold(1, "batch", None)],
        fixtures::plain(),
    );
    let _ = app.update(Msg::Key(KeyPress::FoldView));
    let _ = app.update(Msg::Key(KeyPress::SelectFirst));
    assert!(
        !matches!(app.selected(), Some(RowKey::Section(_))),
        "selection steps past a header, got {:?}",
        app.selected()
    );
}
```

Read `a_group_confirm_states_how_many_processes_it_reaches` (`app.rs:4618`) before writing the confirm test, and follow how it reads the status text rather than inventing a helper.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --workspace --all-features --lib --bins -- a_fold_rolls_up a_fold_confirm no_fold_header_is_not_selectable`
Expected: FAIL.

- [ ] **Step 3: Add the rollup**

`fold_totals` gathers the fold's members and computes the same five fields `group_totals` does, with uptime as `.min()` over `self.uptime_ms(id)`. If the two functions end up identical but for how they select members, factor the shared body into one helper taking an iterator of members rather than leaving two copies.

- [ ] **Step 4: Arm and confirm on a fold**

In `arm` (`app.rs:3473`), add the `RowKey::Fold(name)` arm beside the `Group` arm at `:3498`, counting the fold's members the same way. The refusal ladder above it is untouched.

In `confirm_prompt` (`status.rs:191`), add a `RowKey::Fold` arm naming the verb, the count and the fold, following the `Group` arm's sentence shape. `in_flight_text` (`status.rs:211`) mirrors it, so it needs the arm too.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test --workspace --all-features --lib --bins -- a_fold_rolls_up a_fold_confirm no_fold_header_is_not_selectable`
Expected: PASS.

- [ ] **Step 6: Full gate and commit**

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
git add crates/shep-cli/src/lookout/
git commit -m "feat(lookout): act on a whole fold behind a confirm that names the count"
```

---

## Task 3: The fold view's columns

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/flock.rs`

**Interfaces:**
- Consumes: `Grouping` and `RowKey::Fold` from Task 1; `fold_totals` from Task 2.
- Produces: `FoldColumn` enum; `fold_columns_for(width: u16) -> &'static [FoldColumn]`; `fold_key_line(...) -> Line<'static>`.

**A separate column set, not a reworking of `Column`.** Flat view is 14 columns and fold view is 8, they share only `STATUS`, and `Column::Fold` already exists meaning something else entirely. Two sets that each stay simple beat one that carries a mode flag into every arm.

- [ ] **Step 1: Write the failing tests**

```rust
/// The same invariant the flat ladder carries: a tier chosen for a width
/// must actually fit in it.
#[test]
fn every_fold_tier_fits_the_width_it_claims() {
    for width in MIN_WIDTH..=200 {
        let cols = fold_columns_for(width);
        let fixed: u16 = cols.iter().map(|c| c.width()).sum();
        let gaps = u16::try_from(cols.len() - 1).unwrap() * 2;
        assert!(
            fixed + gaps + NAME_MIN <= width,
            "width {width} chose {} columns needing {}",
            cols.len(),
            fixed + gaps + NAME_MIN
        );
    }
}

/// The share bar goes first because it is the widest thing that is not
/// the name, and it does not exist in flat view to be missed.
#[test]
fn the_share_bar_is_the_first_column_to_go() {
    let wide = fold_columns_for(200);
    assert!(wide.contains(&FoldColumn::Share));
    let narrow = fold_columns_for(100);
    assert!(!narrow.contains(&FoldColumn::Share), "got {narrow:?}");
}

/// A header sums its members and shows the share of total flock memory.
#[test]
fn a_fold_header_shows_its_rollup_and_its_share() {
    let app = fixtures::app_with(
        vec![
            fixtures::sheep_with(1, "api", Some("edge"), 120_000, Some(100 << 20), 2),
            fixtures::sheep_with(2, "cdn", Some("edge"), 30_000, Some(150 << 20), 5),
            fixtures::sheep_with(3, "batch", None, 60_000, Some(50 << 20), 0),
        ],
        fixtures::plain(),
    );
    let line = fold_key_line(&app, &RowKey::Fold("edge".into()), fold_columns_for(160), 160, false);
    let text = rendered(&line);
    assert!(text.contains("edge ×2"), "got {text}");
    assert!(text.contains("250.0M"), "memory sums: {text}");
    assert!(text.contains("30s"), "uptime is the minimum: {text}");
    assert!(text.contains("83%"), "250 of 300 MiB of flock memory: {text}");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --workspace --all-features --lib --bins -- every_fold_tier share_bar_is_the_first a_fold_header_shows`
Expected: FAIL.

- [ ] **Step 3: Add the column set**

```rust
/// A column in the fold view.
///
/// Separate from [`Column`], which is the flat table's set: the two share
/// only `STATUS`, and `Column::Fold` already means one sheep's fold name
/// rather than anything about grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldColumn {
    /// The fold or member name, taking the remainder.
    Name,
    Status,
    /// A 20-cell gauge of this fold's part of total flock memory.
    Share,
    Mem,
    Cpu,
    Uptime,
    Restarts,
    Notes,
}
```

Widths from the spec: `Name` flexes, `Status` 12, `Share` 22, `Mem` 10, `Cpu` 9, `Uptime` 10, `Restarts` 8, `Notes` 63.

Build `FOLD_TIERS` widest-first in the shape of `TIERS` (`flock.rs:345`), dropping `Share` first, then `Notes`, then `Restarts`, then `Cpu`, then `Uptime`, then `Mem`. Reuse `name_width` (`flock.rs:371`) and the existing `NAME_MIN` / `NAME_MAX` / `GUTTER` constants rather than adding new ones.

- [ ] **Step 4: Render the rows**

`fold_key_line` dispatches like `key_line` (`flock.rs:457`) does: a `RowKey::Fold` renders the header, a `Group` or `Sheep` renders a member, a `Section` renders the band. Header rows in ink, member rows in `Palette::muted()` for the ink-2 effect.

The share bar is `cell::gauge(fold_memory, Some(total_flock_memory), 20)`, with the percentage stated in `Notes`. A fold whose memory is unknown gets an empty bar rather than a wrong one.

Selection paint and the gutter reuse `pad_ground` (`flock.rs:644`) and `gutter` (`flock.rs:81`) unchanged.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test --workspace --all-features --lib --bins -- every_fold_tier share_bar_is_the_first a_fold_header_shows`
Expected: PASS.

- [ ] **Step 6: Full gate and commit**

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
git add crates/shep-cli/src/lookout/view/flock.rs
git commit -m "feat(lookout): give the fold view its own columns and drop ladder"
```

---

## Task 4: Collapsing a fold, and the detail pane

**Files:**
- Modify: `crates/shep-cli/src/lookout/app.rs`
- Modify: `crates/shep-cli/src/lookout/view/detail.rs` (`detail_lines` at :27, `group_lines` at :57)
- Modify: `crates/shep-cli/src/lookout/input.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: `z` collapsing a fold; a `RowKey::Fold` arm in `detail_lines`.

- [ ] **Step 1: Write the failing tests**

```rust
/// z hides a fold's members and leaves its header, so a big flock can be
/// read a fold at a time.
#[test]
fn z_collapses_a_fold_and_keeps_its_header() {
    let mut app = fixtures::app_with(
        vec![
            fixtures::sheep_in_fold(1, "api", Some("edge")),
            fixtures::sheep_in_fold(2, "cdn", Some("edge")),
        ],
        fixtures::plain(),
    );
    let _ = app.update(Msg::Key(KeyPress::FoldView));
    app.select_fold_for_tests("edge");
    let _ = app.update(Msg::Key(KeyPress::Collapse));
    let rows = app.visible_rows();
    assert!(rows.iter().any(|r| matches!(r, RowKey::Fold(n) if n == "edge")));
    assert_eq!(rows.iter().filter(|r| matches!(r, RowKey::Sheep(_))).count(), 0);
}

/// The detail pane already refuses to invent a single process for a
/// group. A fold is the same situation one level up.
#[test]
fn a_selected_fold_shows_the_rollup_and_no_log_paths() {
    let mut app = fixtures::app_with(
        vec![
            fixtures::sheep_in_fold(1, "api", Some("edge")),
            fixtures::sheep_in_fold(2, "cdn", Some("edge")),
        ],
        fixtures::plain(),
    );
    let _ = app.update(Msg::Key(KeyPress::FoldView));
    app.select_fold_for_tests("edge");
    let text = render_all(&detail_lines(&app, 200));
    assert!(text.contains("fold edge ×2"), "got {text}");
    assert!(!text.contains("out  "), "a fold has no single log path: {text}");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --workspace --all-features --lib --bins -- z_collapses a_selected_fold_shows`
Expected: FAIL.

- [ ] **Step 3: Implement collapse**

`z` is unbound today and a test at `input.rs:159` pins that; update it rather than leaving a test asserting the opposite of the code. Add `KeyPress::Collapse`, bind `z`, and hold the collapsed set on `App`. `visible_rows` skips members of a collapsed fold. `z` on anything other than a fold header does nothing.

- [ ] **Step 4: Add the detail branch**

In `detail_lines` (`detail.rs:27`), add `Some(RowKey::Fold(name)) => fold_lines(app, &name, width, palette)`. `fold_lines` mirrors `group_lines` (`detail.rs:57`): the head names the fold and its count, then the rollup from `fold_totals`, then a sentence in place of the lamb line saying a fold has no single process, then blanks where a sheep's log paths would be.

Use `columns` from that module for the width budget, not `chars().count()`. Both existing branches were fixed for that and a third would reintroduce it.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test --workspace --all-features --lib --bins -- z_collapses a_selected_fold_shows`
Expected: PASS.

- [ ] **Step 6: Full gate and commit**

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
git add crates/shep-cli/src/lookout/
git commit -m "feat(lookout): collapse a fold with z and describe one in the detail pane"
```

---

## Task 5: The scene, the gallery and the docs

**Files:**
- Modify: `crates/shep-cli/src/lookout/frames.rs`
- Create: a snapshot under `crates/shep-cli/src/lookout/snapshots/`
- Modify: `docs/lookout/frames.txt`, `docs/lookout/frames.ansi` (generated)
- Modify: `docs/lookout/README.md`, `web/src/pages/docs/lookout.astro`, `web/src/pages/docs/folds.astro`

**Adding a scene touches more places than it looks**, and the first four fail to compile if missed:

- the variant inside the `scenes!` block, with a doc comment (`frames.rs:164-250`)
- `label()` (`frames.rs:256`), exhaustive, no wildcard
- `caption()` (`frames.rs:302`), exhaustive; a test asserts the caption is a real sentence over 30 characters
- `scene_after()` (`frames.rs:2359`), test-only and exhaustive, walked by `scene_all_lists_every_variant_the_compiler_can_see` (`frames.rs:2407`)
- `scene_with()`'s construction matches (`frames.rs:568`, and those at 916, 989, 1025, 1077)
- `control()` (`frames.rs:418`) and `size()` (`frames.rs:427`) both have wildcard arms and compile without a new one

- [ ] **Step 1: Add the scene**

Build it with at least two real folds, one unfoldered sheep and a dog, so the scene pins the `no fold` and `Dogs` headers as well as a fold. Include one collapsed fold if the scene can carry it without becoming unreadable.

- [ ] **Step 2: Run the scene tests**

Run: `cargo test --workspace --all-features --lib --bins -- scene_all_lists every_scene_shows_the_thing`
Expected: PASS.

- [ ] **Step 3: Accept the snapshot**

Run: `cargo test --workspace --all-features -- frames_are_pinned`

Read the generated `.snap` before accepting. Check the share bars against the memory figures by hand once: a gauge that renders plausibly and computes wrongly is exactly what a snapshot will pin forever.

- [ ] **Step 4: Regenerate the gallery**

```bash
cargo test --workspace --all-features -- write_the_gallery --ignored
```

- [ ] **Step 5: Update the prose**

`docs/lookout/README.md` and `web/src/pages/docs/lookout.astro`: `F` toggles the fold view, `z` collapses a fold, a fold header's numbers are its members summed with the shortest uptime, an action on a fold reaches every member behind a confirm naming the count, and the `no fold` header is not selectable because no selector names its members.

**This is prose a person reads. Run the `humanizer` skill, then `rin-voice`, before committing.** No em dashes.

- [ ] **Step 6: Build the site**

```bash
cd web && npx astro build
```
```bash
cd web && npx astro check
```

- [ ] **Step 7: Full gate and commit**

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
git add crates/shep-cli/src/lookout/ docs/lookout/frames.txt docs/lookout/frames.ansi
git commit -m "test(lookout): pin the fold view in the frame gallery"
git add docs/lookout/README.md web/src/pages/docs/lookout.astro
git commit -m "docs(lookout): describe the fold view and its fold-wide actions"
```

---

## Self-review

**Correction, 2026-09-08, found during Task 3's review.** The claim below says
"Own column ladder: Task 3", and Task 3 does build one. Nothing connects it.
`view/mod.rs` calls `flock::columns_for(table_width)` unconditionally and
never branches on `App::grouping()`, so the column header row and every member
row still lay out in flat view's fourteen columns while a fold header lays
itself out in `FoldColumn`'s eight. The table renders misaligned the moment
`F` is pressed.

This is why `App::grouping()` still had no non-test caller after the task that
was supposed to consume it. Ruled into Task 4, which is the last functional
task before the scene, and the scene in Task 5 has to pin a correct render
rather than a misaligned one.

**Spec coverage.** Grouping mode and `F`: Task 1. Fold, `no fold` and dogs headers: Task 1. Two levels not three: Task 1. Rollup matching the group rule: Task 2. Fold-wide actions and the confirm: Task 2. Share bar: Task 3. Own column ladder: Task 3 builds it, Task 4 wires it into `view/mod.rs` (see the correction above). `z` collapse: Task 4. Detail branch: Task 4. Scene and docs: Task 5.

**One spec sentence needed sharpening, and the plan carries the sharper version.** The spec says the `no fold` header exists; it does not say whether it is selectable. `SelectorSpec::Fold(String)` can express `fold:edge` and cannot express "everything with no fold", so an action there would have to enumerate ids the operator never named. The plan makes it a `Section`, which is already the non-selectable kind, and Task 2 tests it.

**Ordering.** Task 1 produces `RowKey::Fold` and the grouping, which everything else consumes. Task 2 needs Task 1's row key. Task 3 needs Task 2's rollup. Task 4 needs all three. Task 5 last, so the scene pins finished work.

**Names checked against the tree.** `RowKey` (`app.rs:624`), `visible_rows` (`:3767`), `push_grouped_rows` (`:3799`), `group_totals` (`:4039`), `GroupTotals` (`:373`), `arm` (`:3473`), `Column`/`columns_for`/`TIERS`/`name_width` (`flock.rs:91`, `:362`, `:345`, `:371`), `key_line` (`:457`), `group_line` (`:499`), `pad_ground` (`:644`), `gutter` (`:81`), `confirm_prompt` (`status.rs:191`), `in_flight_text` (`:211`), `detail_lines` (`detail.rs:27`), `group_lines` (`:57`), `ProcessInfo::fold` (`request.rs:654`).

**Three names the implementer must confirm rather than trust:** `select_fold_for_tests`, `fixtures::status_text` and `fixtures::sheep_with` are all test helpers this plan invents. Follow the standing rule: read the neighbouring tests, use what is really there, and add a helper only when nothing fits. The assertions are the requirement.
