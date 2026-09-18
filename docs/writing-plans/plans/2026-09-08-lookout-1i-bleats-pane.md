# Lookout 1i: bleats full screen Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the bleats feed the whole screen, with three composable filters over the window it already reads.

**Architecture:** A fourth `Body` variant holding the pane's own state, opened with `b` from the dashboard and closed with `esc`. It reads through `tail.rs`'s existing bounded window rather than the bus, and asks for extra polls through the channel `r` already uses rather than changing any interval.

**Tech Stack:** Rust 1.88, edition 2024, ratatui 0.30.2, insta snapshots.

**Spec:** [docs/brainstorming/specs/2026-09-08-lookout-1i-1j-design.md](../../brainstorming/specs/2026-09-08-lookout-1i-1j-design.md)

## Global Constraints

- No protocol change. No `PROTOCOL_VERSION`, `MIN_SUPPORTED` or `SCHEMA_VERSION` movement. This pane is a view.
- **Do not subscribe to the bus.** Topics are `log.out` and `log.err` and globs match the topic rather than the sheep, so a subscription takes every sheep's output. `source.rs`'s comment explains the cost; leave `TOPICS` alone.
- A log line with no detectable level is **always shown**, whatever the minimum is. This is the decision most likely to be reversed by someone tidying up, so it gets its own test.
- Nothing whole-file: no line count, no absolute line numbers, no density gutter. The pane reads a window and every count it states is scoped to that window.
- Invoke the `shep-idiomatic-rust` skill before writing Rust. Cite `IR-<n>` where a rule applies.
- Every new public item needs a doc comment and a deliberate `Debug` decision.
- Conventional commit subjects. Nothing here breaks, so no `!`.
- ONE cargo shape: `--workspace`. Do not alternate with `-p <crate>`; this repo's cache churns badly when a brief names two shapes.
- Iterate with `cargo test --workspace --all-features --lib --bins`. Run the full `cargo test --workspace --all-features` once per task before committing.
- `map_key` dispatches on `InputMode`, not on which `Body` is showing. There are only two modes, `Normal` and `Text`. A new pane does **not** get a third; its keys are handled by the reducer matching on `Body`.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/shep-cli/src/lookout/pane_bleats.rs` (new) | the pane's own state: filters, cursor, and the pure filter application |
| `crates/shep-cli/src/lookout/level.rs` (new) | parsing a level word out of an arbitrary log line |
| `crates/shep-cli/src/lookout/view/bleats_full.rs` (new) | rendering the full-screen pane |
| `crates/shep-cli/src/lookout/app.rs` | the `Body::Bleats` variant, open and close, key handling |
| `crates/shep-cli/src/lookout/input.rs` | binding `b` |
| `crates/shep-cli/src/lookout/view/mod.rs` | the `draw` arm |
| `crates/shep-cli/src/lookout/frames.rs` | the scene, its label and caption |

New files rather than growing `app.rs`, which is already past 8,000 lines.

---

## Task 1: The pane opens, shows the window, and closes

**Files:**
- Create: `crates/shep-cli/src/lookout/pane_bleats.rs`
- Create: `crates/shep-cli/src/lookout/view/bleats_full.rs`
- Modify: `crates/shep-cli/src/lookout/app.rs` (`Body` at :1251, `close_pane` at :2668, the reducer)
- Modify: `crates/shep-cli/src/lookout/input.rs` (:43-64)
- Modify: `crates/shep-cli/src/lookout/view/mod.rs` (`draw`)

**Interfaces:**
- Consumes: `tail::Tail`, `tail::TailLine`, `tail::Stream` from `crates/shep-cli/src/lookout/tail.rs:34-61`; `App::feed(&self) -> &Tail` at `app.rs:4145`.
- Produces: `Body::Bleats(BleatsPane)`; `BleatsPane::new(sheep: RowKey) -> Self`; `BleatsPane::sheep(&self) -> &RowKey`; `view::bleats_full::draw(app, frame, area)`.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-cli/src/lookout/input.rs`'s `mod tests`, beside the existing test that pins `z` as unbound at `input.rs:159`:

```rust
/// `b` opens the full-screen bleats pane. Pinned because `map_key`
/// dispatches on mode rather than pane, so a key taken here is taken
/// everywhere in `Normal`.
#[test]
fn b_opens_the_bleats_pane() {
    assert_eq!(
        map_key(&key('b'), InputMode::Normal),
        Some(KeyPress::Bleats)
    );
}
```

Use whatever helper the neighbouring tests use to build the event; read two of them first rather than inventing one.

In `crates/shep-cli/src/lookout/app.rs`'s `mod tests`:

```rust
/// The pane opens on whatever was selected and pins it: full screen
/// leaves no table to change a selection with.
#[test]
fn b_opens_the_pane_on_the_selected_sheep() {
    let mut app =
        fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    let _ = app.update(Msg::Key(KeyPress::Bleats));
    let pane = app.bleats_pane().expect("the pane is open");
    assert!(matches!(pane.sheep(), RowKey::Sheep(id) if *id == 9));
}

/// `close_pane` always lands on the dashboard, never on whatever screen
/// preceded the pane. Same rule the config pane follows.
#[test]
fn esc_from_the_bleats_pane_lands_on_the_dashboard() {
    let mut app =
        fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    let _ = app.update(Msg::Key(KeyPress::Bleats));
    let _ = app.update(Msg::Key(KeyPress::Escape));
    assert!(matches!(app.body(), Body::FlockTable));
}

/// `b` with nothing selected asks for nothing, the way `e` does.
#[test]
fn b_with_nothing_selected_opens_no_pane() {
    let mut app = fixtures::app_with(Vec::new(), fixtures::plain());
    let _ = app.update(Msg::Key(KeyPress::Bleats));
    assert!(app.bleats_pane().is_none());
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --workspace --all-features --lib --bins -- b_opens_the esc_from_the_bleats b_with_nothing_selected`
Expected: FAIL, no `KeyPress::Bleats` and no `bleats_pane`.

- [ ] **Step 3: Add the key**

`b` is currently unbound; confirm with `rg "'b' =>" crates/shep-cli/src/lookout/input.rs` before adding. Add `KeyPress::Bleats` and bind `b` in the `Normal` arm at `input.rs:43-64`, following the shape of the neighbouring bindings.

- [ ] **Step 4: Add the pane's state**

`crates/shep-cli/src/lookout/pane_bleats.rs`:

```rust
/// The full-screen bleats pane's own state.
///
/// Holds the sheep it opened on rather than reading the dashboard's
/// selection: full screen leaves no table on which to change one, so the
/// pane describes a single sheep for as long as it is open.
///
/// `Debug` is derived. A row key and a cursor position carry no env, no
/// path and no argument vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BleatsPane {
    sheep: RowKey,
}

impl BleatsPane {
    /// Opens the pane on one sheep.
    #[must_use]
    pub fn new(sheep: RowKey) -> Self {
        Self { sheep }
    }

    /// The sheep this pane describes, fixed for its lifetime.
    #[must_use]
    pub fn sheep(&self) -> &RowKey {
        &self.sheep
    }
}
```

- [ ] **Step 5: Add the `Body` variant and its accessor**

In `app.rs`, extend `Body` at `:1251`:

```rust
    /// The bleats feed given the whole screen, opened by
    /// [`KeyPress::Bleats`].
    Bleats(BleatsPane),
```

Add the accessor beside `config_pane` at `app.rs:4247`, matching its shape exactly:

```rust
    /// The open bleats pane, or `None` on any other screen.
    #[must_use]
    pub fn bleats_pane(&self) -> Option<&BleatsPane> {
        match &self.body {
            Body::Bleats(pane) => Some(pane),
            Body::FlockTable | Body::Settings(_) | Body::ConfigPane(_) => None,
        }
    }
```

The compiler will name every other `match` on `Body` that needs the new arm. Fix each; `settings()` and `config_pane()` list their non-matching variants explicitly rather than using a wildcard, so follow that.

`close_pane` at `app.rs:2668` already sets `Body::FlockTable` and clears the pane fields. `BleatsPane` carries no state outside itself, so nothing new needs clearing there.

- [ ] **Step 6: Handle the keys in the reducer**

`KeyPress::Bleats` opens the pane when a sheep is selected and does nothing otherwise, mirroring how `KeyPress::Edit` behaves with nothing selected. `KeyPress::Escape` while `Body::Bleats` calls `close_pane()`.

Remember the constraint: there is no new `InputMode`. The reducer matches on `Body` to decide what a key means.

- [ ] **Step 7: Render it**

`view/bleats_full.rs` draws the whole area: a title band naming the sheep and both log paths, then the window's lines, newest at the bottom, each tagged `out` or `err`. Reuse `cell::band` and `Palette::band(Role::Meadow)` for the title, and follow `view/bleats.rs:88-98` for how a line is tagged.

Add the `Body::Bleats` arm to `view::draw` in `view/mod.rs`, following the arms already there.

- [ ] **Step 8: Run to verify they pass**

Run: `cargo test --workspace --all-features --lib --bins -- b_opens_the esc_from_the_bleats b_with_nothing_selected`
Expected: PASS.

- [ ] **Step 9: Full gate and commit**

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
git commit -m "feat(lookout): open the bleats feed full screen on b"
```

---

## Task 2: Parsing a level out of an arbitrary line

**Files:**
- Create: `crates/shep-cli/src/lookout/level.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `pub enum Level { Trace, Debug, Info, Warn, Error }` with `Ord`; `pub fn level_of(line: &str) -> Option<Level>`.

A pure function with no dependencies, so it lands on its own and is tested exhaustively before anything filters with it.

**Correction, 2026-09-08, after this task's review.** The trimming rule below
was written as `trim_matches(|c: char| !c.is_ascii_alphabetic())`, which
strips digits as well as punctuation from both ends of a word. That turns
`/error404` into `error` and `info2` into `info`, so `level_of` announces a
level on an ordinary access-log token that announced none. Spec decision 3
forbids exactly that. The trim must not swallow digits: a level word is the
whole word once surrounding punctuation is gone, and a digit next to it means
it was never a level word. The tests below cover only the alphabetic
continuation case (`information`, `warnings`), which is why this got through.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_leading_level_word_is_found_whatever_its_case() {
    assert_eq!(level_of("WARN pool exhausted"), Some(Level::Warn));
    assert_eq!(level_of("warn pool exhausted"), Some(Level::Warn));
    assert_eq!(level_of("Error: connection refused"), Some(Level::Error));
}

/// A timestamp before the level is the common shape, so the search looks
/// past a prefix rather than only at the first word.
#[test]
fn a_level_after_a_timestamp_is_still_found() {
    assert_eq!(
        level_of("2026-09-08T11:02:03Z INFO listening on 8080"),
        Some(Level::Info)
    );
}

/// The whole reason the filter shows unclassifiable lines: most app
/// output looks like this.
#[test]
fn a_line_with_no_level_word_has_no_level() {
    assert_eq!(level_of("listening on 8080"), None);
    assert_eq!(level_of(""), None);
}

/// A word that merely contains a level name is not a level. Without
/// this, "information" and "errors:" both read as levels.
#[test]
fn a_level_name_inside_a_longer_word_is_not_a_level() {
    assert_eq!(level_of("information about the pool"), None);
    assert_eq!(level_of("warnings are disabled"), None);
}

#[test]
fn levels_order_from_trace_up_to_error() {
    assert!(Level::Trace < Level::Debug);
    assert!(Level::Debug < Level::Info);
    assert!(Level::Info < Level::Warn);
    assert!(Level::Warn < Level::Error);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --workspace --all-features --lib --bins -- level_of levels_order`
Expected: FAIL, module does not exist.

- [ ] **Step 3: Implement**

```rust
/// A log level, ordered so a minimum can be compared against.
///
/// `Ord` is derived and the declaration order is the ordering: `Trace` is
/// the lowest and `Error` the highest, so `level >= minimum` reads the way
/// an operator setting `level >= warn` expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// The level a line announces, or `None` when it announces none.
///
/// Scans the first few whitespace-separated words rather than only the
/// first, because a timestamp before the level is the common shape. A
/// candidate matches only as a whole word, after trailing punctuation is
/// stripped, so `information` is not `info`.
///
/// `None` is the ordinary answer for app output. Callers must not treat it
/// as "below the minimum": see the spec's decision 3.
#[must_use]
pub fn level_of(line: &str) -> Option<Level> {
    line.split_whitespace().take(4).find_map(|word| {
        // The predicate keeps digits: see the correction note above for why
        // stripping them made `/error404` read as `Error`.
        match word
            .trim_matches(|c: char| !c.is_ascii_alphanumeric())
            .to_ascii_lowercase()
            .as_str()
        {
            "trace" => Some(Level::Trace),
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "error" | "fatal" => Some(Level::Error),
            _ => None,
        }
    })
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --workspace --all-features --lib --bins -- level_of levels_order`
Expected: PASS, five tests.

- [ ] **Step 5: Full gate and commit**

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
git add crates/shep-cli/src/lookout/level.rs crates/shep-cli/src/lookout/mod.rs
git commit -m "feat(lookout): read a log level out of an arbitrary line"
```

---

## Task 3: The three filters, and what esc does

**Files:**
- Modify: `crates/shep-cli/src/lookout/pane_bleats.rs`
- Modify: `crates/shep-cli/src/lookout/app.rs` (reducer)

**Interfaces:**
- Consumes: `Level` and `level_of` from Task 2; `TailLine` and `Stream` from `tail.rs`.
- Produces: `Filters { stream, min_level, matcher }`; `BleatsPane::filters(&self) -> &Filters`; `BleatsPane::visible<'a>(&self, lines: &'a [TailLine]) -> Vec<&'a TailLine>`; `BleatsPane::drop_newest_chip(&mut self) -> bool`.

- [ ] **Step 1: Write the failing tests**

```rust
/// The decision most likely to be quietly reversed. An app printing bare
/// text must not vanish because somebody asked for warnings.
#[test]
fn a_line_with_no_level_survives_a_minimum() {
    let mut pane = BleatsPane::new(RowKey::Sheep(9));
    pane.set_min_level(Some(Level::Warn));
    let lines = vec![
        line(Stream::Out, "listening on 8080"),
        line(Stream::Out, "INFO routine chatter"),
        line(Stream::Out, "ERROR pool exhausted"),
    ];
    let kept: Vec<&str> = pane.visible(&lines).iter().map(|l| l.text.as_str()).collect();
    assert_eq!(kept, vec!["listening on 8080", "ERROR pool exhausted"]);
}

#[test]
fn the_three_axes_compose_with_and() {
    let mut pane = BleatsPane::new(RowKey::Sheep(9));
    pane.set_stream(Some(Stream::Err));
    pane.set_min_level(Some(Level::Warn));
    pane.set_match("pool".to_string());
    let lines = vec![
        line(Stream::Err, "ERROR pool exhausted"),   // all three hold
        line(Stream::Out, "ERROR pool exhausted"),   // wrong stream
        line(Stream::Err, "INFO pool warming"),      // below the minimum
        line(Stream::Err, "ERROR disk full"),        // no match
    ];
    assert_eq!(pane.visible(&lines).len(), 1);
}

/// esc removes the newest chip rather than clearing every filter, so
/// backing out of a filter is one key at a time.
#[test]
fn esc_drops_the_newest_chip_then_reports_there_are_none_left() {
    let mut pane = BleatsPane::new(RowKey::Sheep(9));
    pane.set_stream(Some(Stream::Err));
    pane.set_min_level(Some(Level::Warn));

    assert!(pane.drop_newest_chip(), "the level chip was newest");
    assert!(pane.filters().min_level.is_none());
    assert!(pane.filters().stream.is_some(), "the older chip stays");

    assert!(pane.drop_newest_chip(), "the stream chip goes next");
    assert!(!pane.drop_newest_chip(), "nothing left to drop");
}

/// With no chips left, esc closes the pane instead of dropping one.
#[test]
fn esc_with_no_chips_closes_the_pane() {
    let mut app =
        fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    let _ = app.update(Msg::Key(KeyPress::Bleats));
    let _ = app.update(Msg::Key(KeyPress::Escape));
    assert!(matches!(app.body(), Body::FlockTable));
}
```

Write the `line` helper beside the tests; it builds a `TailLine` from a stream and a `&str`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --workspace --all-features --lib --bins -- survives_a_minimum compose_with_and drops_the_newest_chip no_chips_closes`
Expected: FAIL.

- [ ] **Step 3: Implement the filter state**

`Filters` holds `stream: Option<Stream>`, `min_level: Option<Level>` and `matcher: Option<String>`, plus the order the chips were added so `drop_newest_chip` knows which is newest. A small `Vec<Axis>` of the axes currently set, pushed on set and removed on drop, is enough; do not reach for anything cleverer.

`visible` keeps a line when every set axis holds. The level axis holds when the line has no detectable level **or** its level meets the minimum, and that arm carries a comment saying why.

- [ ] **Step 4: Wire esc in the reducer**

While `Body::Bleats`, `KeyPress::Escape` calls `drop_newest_chip()` first and only calls `close_pane()` when that returns `false`.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test --workspace --all-features --lib --bins -- survives_a_minimum compose_with_and drops_the_newest_chip no_chips_closes`
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
git commit -m "feat(lookout): filter the bleats pane by stream, level and match"
```

---

## Task 4: The filter row

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/bleats_full.rs`

**Interfaces:**
- Consumes: `Filters` and `visible` from Task 3.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Write the failing tests**

```rust
/// The row states the composition rule, because three chips with no
/// stated relationship read as alternatives.
#[test]
fn the_filter_row_says_the_axes_compose_with_and() {
    let app = fixtures::bleats_pane_with_filters();
    let text = render_all(&draw_lines(&app, 160, 40));
    assert!(text.contains("all three must hold"), "got {text}");
}

/// Scoped to the window, because nothing counted the whole file.
#[test]
fn the_survivor_count_is_scoped_to_the_window() {
    let app = fixtures::bleats_pane_with_filters();
    let text = render_all(&draw_lines(&app, 160, 40));
    assert!(
        text.contains("lines in the window"),
        "the count must not claim a whole-file total: {text}"
    );
}
```

Add `bleats_pane_with_filters()` to `view/fixtures.rs` beside the existing helpers, building an app with the pane open and all three axes set.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --workspace --all-features --lib --bins -- filter_row_says survivor_count_is_scoped`
Expected: FAIL.

- [ ] **Step 3: Render the row**

One row under the title: a chip per set axis on `Palette::ground()`, then the sentence naming how many survived, of how many were read, in the window. Only set axes get a chip.

The frame's 8-cell line-number column is not rendered: the ruling dropped absolute numbers and a window-relative one would move as the window slides. Those 8 cells go to the line text.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --workspace --all-features --lib --bins -- filter_row_says survivor_count_is_scoped`
Expected: PASS.

- [ ] **Step 5: Full gate and commit**

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
git add crates/shep-cli/src/lookout/view/
git commit -m "feat(lookout): state the filter composition and the window's survivor count"
```

---

## Task 5: Poll faster while the pane is open

**Files:**
- Modify: `crates/shep-cli/src/lookout/app.rs`
- Nothing in `crates/shep-cli/src/lookout/mod.rs`. `Effect::RefreshFeed` is already handled there (`mod.rs:392`); the pre-flight scan confirmed it, so this task is `app.rs` only.

**Interfaces:**
- Consumes: the pane from Task 1.
- Produces: nothing later tasks depend on.

**Read this before starting.** `link::FLOCK_POLL` is a `const` passed once into `run_link` (`link.rs:29`, `mod.rs:172`) and the ticker is built once per connection at `link.rs:165`. **There is no way to change the interval at runtime, and you must not add one.** Nor is the answer the out-of-band `channels.polls` at `link.rs:173`: that is the reconcile route, and `Effect::PollNow` and `r` own it.

The feed has a separate route already. `Effect::RefreshFeed` sets `feed_dirty` at `mod.rs:392` and the read is coalesced onto `MIN_REDRAW`, for the reason the comment above it gives: a held `j` reaches the terminal as twenty to thirty events a second, and an uncoalesced synchronous 128 KiB read would sit behind every repeat on the task that also owns the redraw. Raising `RefreshFeed` more often is therefore cheap by construction. Nothing in `mod.rs` changes.

**Corrected during the pre-flight scan.** This task first said to raise `Effect::PollNow`. That is the wrong effect: it asks the link task for a `ListFlock`, the whole flock listing, which a log viewer does not want and which would re-fetch every sheep every tick while the pane is open.

`Effect::RefreshFeed` (`app.rs:273`) is the right one, and its own doc says why: "Re-read the selected sheep's log files and hand the result back as `Msg::Bleats`. The feed has no timer of its own; it rides this." The read is coalesced at `mod.rs:257`, so repeated effects do not stack up reads.

Raise `RefreshFeed` on each tick while the pane is open. The dashboard's own cadence is untouched.

- [ ] **Step 1: Write the failing test**

```rust
/// The pane asks for its own refreshes rather than changing the link's
/// interval, which is fixed for a connection's lifetime.
#[test]
fn a_tick_while_the_pane_is_open_asks_for_a_poll() {
    let mut app =
        fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    let _ = app.update(Msg::Key(KeyPress::Bleats));
    assert_eq!(app.update(Msg::Tick(now)), Effect::RefreshFeed);
}

/// And does not on the dashboard, or every lookout would poll twice as
/// often for nothing.
#[test]
fn a_tick_on_the_dashboard_asks_for_nothing() {
    let mut app =
        fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    assert_eq!(app.update(Msg::Tick(now)), Effect::None);
}
```

Read the existing `Msg::Tick` tests first and build `now` the way they do; `fixtures::later()` does not exist. `Effect::RefreshFeed` does, at `app.rs:273`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --workspace --all-features --lib --bins -- tick_while_the_pane tick_on_the_dashboard`
Expected: FAIL.

- [ ] **Step 3: Implement**

In the `Msg::Tick` arm, return the poll effect when `body` is `Body::Bleats` and the existing effect otherwise.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --workspace --all-features --lib --bins -- tick_while_the_pane tick_on_the_dashboard`
Expected: PASS.

- [ ] **Step 5: Full gate and commit**

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
git commit -m "feat(lookout): refresh the bleats pane on its own ticks"
```

---

## Task 6: The filter keys

**Added 2026-09-08.** The plan shipped three filter axes that no key could
set. `KeyPress::FilterStart`, which is `/`, sat in `on_bleats_key`'s inert arm,
while `docs/lookout/design-files/README.md:321` says plainly "In 1i, `/` adds a
match chip". `rulings.md` calls the three axes "the point of it". They were
built, tested, rendered and unreachable. The maintainer chose to build the
pane's full status bar rather than the filter keys alone, so this task and the
two after it exist.

**Files:**
- Modify: `crates/shep-cli/src/lookout/input.rs`
- Modify: `crates/shep-cli/src/lookout/app.rs` (`on_bleats_key`)
- Modify: `crates/shep-cli/src/lookout/pane_bleats.rs`

**Interfaces:**
- Consumes: `Filters`, `BleatsPane::{set_stream, set_min_level, set_match}`,
  `Level`, `Stream` — all built in Tasks 2 and 3.
- Produces: `KeyPress::StreamCycle`, `KeyPress::LevelCycle`; `/` reaching the
  pane's match input.

**Three keys, and only two of them come from the design.**

| key | axis | source |
|---|---|---|
| `/` | match | `README.md:321` and the status bar's `/ search` |
| `o` | stream, out/err/both | the status bar's `o out/err/both` |
| `m` | minimum level | **invented here.** The design's status bar names no key for the level axis, though the filter row draws its chip. `m` for minimum, chosen because every better mnemonic is taken: `l` reads as line, `L` is Reload, `v` means nothing here. Flag it to the maintainer rather than burying it. |

**`/` reuses `InputMode::Text`, and does not add a mode.** The dashboard's name
filter already does exactly this: `/` starts text entry, typing narrows, `↵`
applies, `esc` abandons the edit. Read how `KeyPress::FilterStart` and the
`TextChar`/`TextApply`/`TextAbandon` family are handled for the flock table
before writing the pane's version, and follow it. The global constraint stands:
there are two modes, `Normal` and `Text`, and a new pane does not get a third.

**`esc` while typing abandons the edit; `esc` with the input closed drops the
newest chip.** Those are different actions on the same key and the existing
flock-table filter draws the same distinction. Task 3's chip-dropping behaviour
must keep working, and it has a test.

- [ ] **Step 1: Write the failing tests**

```rust
/// `o` cycles the stream axis through its three states and back. Three
/// presses return to where it began, which is what makes it a cycle
/// rather than a toggle that strands the operator on `err`.
#[test]
fn o_cycles_the_stream_axis_and_returns_to_both() {
    let mut app =
        fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    let _ = app.update(Msg::Key(KeyPress::Bleats));
    assert!(app.bleats_pane().expect("open").filters().stream.is_none());
    let _ = app.update(Msg::Key(KeyPress::StreamCycle));
    let first = app.bleats_pane().expect("open").filters().stream;
    assert!(first.is_some(), "one press sets an axis");
    let _ = app.update(Msg::Key(KeyPress::StreamCycle));
    let _ = app.update(Msg::Key(KeyPress::StreamCycle));
    assert!(
        app.bleats_pane().expect("open").filters().stream.is_none(),
        "three presses land back on both"
    );
}

/// `m` raises the minimum level and eventually clears it. The unset state
/// has to be reachable by key, or an operator who sets a minimum can never
/// see unlevelled output again without closing the pane.
#[test]
fn m_cycles_the_level_axis_back_to_unset() {
    let mut app =
        fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    let _ = app.update(Msg::Key(KeyPress::Bleats));
    let mut seen_some = false;
    for _ in 0..8 {
        let _ = app.update(Msg::Key(KeyPress::LevelCycle));
        if app.bleats_pane().expect("open").filters().min_level.is_some() {
            seen_some = true;
        }
    }
    assert!(seen_some, "the cycle passes through a set minimum");
    // Whatever the cycle length, it must return to unset within one lap.
    let mut cleared = false;
    for _ in 0..8 {
        let _ = app.update(Msg::Key(KeyPress::LevelCycle));
        if app.bleats_pane().expect("open").filters().min_level.is_none() {
            cleared = true;
            break;
        }
    }
    assert!(cleared, "the cycle returns to unset");
}

/// `/` opens the match input rather than doing nothing, which is what it
/// did when this pane first shipped.
#[test]
fn slash_opens_the_match_input_in_the_bleats_pane() {
    let mut app =
        fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
    let _ = app.update(Msg::Key(KeyPress::Bleats));
    let _ = app.update(Msg::Key(KeyPress::FilterStart));
    assert_eq!(app.input_mode(), InputMode::Text, "typing goes to the pane");
}
```

Read the existing flock-table filter tests before writing these: the accessor
for the current mode is named from a survey rather than read in place, so
confirm `input_mode` exists and is reachable from the test module, and use
whatever the neighbouring tests use if it is not.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --workspace --all-features --lib --bins -- o_cycles_the_stream m_cycles_the_level slash_opens_the_match`
Expected: FAIL, no `KeyPress::StreamCycle` and no `KeyPress::LevelCycle`.

- [ ] **Step 3: Add the two keys**

`o` and `m` are both unbound; confirm with
`rg "'o' =>|'m' =>" crates/shep-cli/src/lookout/input.rs` before adding. Add
`KeyPress::StreamCycle` and `KeyPress::LevelCycle` to the enum and bind them in
the `Normal` arm, following the shape of the neighbouring bindings.

They are global bindings, so they reach the dashboard too. The dashboard's
reducer must ignore them explicitly rather than by falling through, the same
way it ignores `KeyPress::Bleats` on screens that have no bleats pane.

- [ ] **Step 4: Cycle the axes**

In `on_bleats_key`, move `KeyPress::FilterStart` out of the inert arm and add
the two new keys. The stream cycle runs `None` → `Out` → `Err` → `None`. The
level cycle walks `Level`'s variants in order and then returns to `None`, which
is the state the "unclassifiable lines always show" rule depends on staying
reachable.

Both go through `set_stream` and `set_min_level` rather than writing
`filters.stream` directly, because those setters maintain the chip order that
`drop_newest_chip` reads. Setting an axis to `None` through them must remove it
from that order; `note_axis` already has the branch for it and Task 3 left it
untested, so this is where it earns a test.

- [ ] **Step 5: Wire the match input**

`/` puts the pane in `InputMode::Text` and routes `TextChar`, `TextBackspace`,
`TextApply` and `TextAbandon` to the match axis. `TextApply` calls `set_match`;
`TextAbandon` leaves the axis as it was. Follow the flock table's handling
rather than inventing one.

- [ ] **Step 6: Remove the dead-code allows this task retires**

`set_stream`, `set_min_level`, `set_match`, `note_axis` and `Axis` carry
`#[allow(dead_code)]` with a reason naming the task that would give them a
caller. This is that task for the first three, and `note_axis` and `Axis` are
reached through them. Remove every attribute this task retires, and its
sentence with it.

- [ ] **Step 7: Gate and commit**

`cargo fmt --all --check`, then
`cargo clippy --workspace --all-targets --all-features -- -D warnings`, then
`cargo test --workspace --all-features`. One command at a time.

---

## Task 7: Scrolling, and following the tail

**Added 2026-09-08.** The pane auto-tails and cannot be scrolled: `j`, `k`,
`G` and the page keys all sit in `on_bleats_key`'s inert arm. The design's
status bar names `j/k line`, `ctrl-d/u page`, `G end` and `f follow`.

**Files:**
- Modify: `crates/shep-cli/src/lookout/input.rs`
- Modify: `crates/shep-cli/src/lookout/app.rs` (`on_bleats_key`)
- Modify: `crates/shep-cli/src/lookout/pane_bleats.rs`
- Modify: `crates/shep-cli/src/lookout/view/bleats_full.rs`

**Interfaces:**
- Consumes: `BleatsPane`, `BleatsPane::visible`.
- Produces: a scroll offset and a follow flag on `BleatsPane`;
  `KeyPress::PageDown`, `KeyPress::PageUp`, `KeyPress::FollowToggle`.

**Follow is the state that makes scrolling coherent, so build it in the same
task.** The pane is a live feed: new lines arrive every refresh. While
following, the view stays pinned to the newest line and an arriving line
scrolls the old ones up. The moment the operator scrolls back, following stops,
or every keypress is undone two seconds later. `G` jumps to the end **and**
re-enables following, because that is what an operator means by it. `f` toggles
following explicitly, and the status bar's right-aligned `█ following`
indicator says which state the pane is in.

The scroll offset counts **filtered** lines, not raw ones. `visible` is what
the body renders, so a changed filter changes what a given offset means;
clamp the offset against the survivor count on every draw rather than trusting
it to still be valid.

`j`, `k` and `G` already map to `KeyPress::SelectDown`, `SelectUp` and
`SelectLast`. Reuse them rather than adding pane-specific twins — the reducer
already dispatches on `Body`, which is what makes one `KeyPress` mean two
things on two screens. `ctrl-d` and `ctrl-u` are new and go in `map_key`'s
CONTROL branch at `input.rs:24`, which currently handles only `ctrl-c`.

**That CONTROL branch returns before the `InputMode::Text` check**, so
`ctrl-d` will fire while the operator is typing a match. Decide deliberately
whether that is acceptable and say which you chose: either move the check, or
accept it and note why. Do not leave it unconsidered.

- [ ] **Step 1: Write the failing tests**

```rust
/// Scrolling back stops the follow, or the next refresh undoes the
/// operator's keypress.
#[test]
fn scrolling_back_stops_following() {
    let mut app = fixtures::bleats_pane_with_lines(120);
    assert!(app.bleats_pane().expect("open").following());
    let _ = app.update(Msg::Key(KeyPress::SelectUp));
    assert!(
        !app.bleats_pane().expect("open").following(),
        "one line back is enough to mean the operator took over"
    );
}

/// `G` is the way back to the live tail, so it restores following as well
/// as jumping.
#[test]
fn g_returns_to_the_end_and_resumes_following() {
    let mut app = fixtures::bleats_pane_with_lines(120);
    let _ = app.update(Msg::Key(KeyPress::SelectUp));
    let _ = app.update(Msg::Key(KeyPress::SelectLast));
    let pane = app.bleats_pane().expect("open");
    assert!(pane.following(), "G resumes the follow");
    assert_eq!(pane.scroll_offset(), 0, "and lands on the newest line");
}

/// A filter that hides most of the window must not leave the offset
/// pointing past the end of what survives.
#[test]
fn a_narrowing_filter_clamps_the_scroll_offset() {
    let mut app = fixtures::bleats_pane_with_lines(120);
    let _ = app.update(Msg::Key(KeyPress::PageUp));
    let _ = app.update(Msg::Key(KeyPress::PageUp));
    app.bleats_pane_mut_for_tests()
        .expect("open")
        .set_match("a-string-no-line-contains".to_string());
    let text = fixtures::render_all(&fixtures::draw_lines(&app, 160, 40));
    assert!(!text.is_empty(), "the pane still draws rather than panicking");
}
```

`fixtures::bleats_pane_with_lines` does not exist. Build it beside the
existing bleats fixtures: an open pane over a feed of `n` lines, enough to
exceed any test's body height.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --workspace --all-features --lib --bins -- scrolling_back_stops g_returns_to_the_end a_narrowing_filter_clamps`
Expected: FAIL, no `following` and no `scroll_offset`.

- [ ] **Step 3: Add the state**

`BleatsPane` gains a scroll offset counted back from the newest surviving line,
and a follow flag defaulting to true. Both need doc comments saying what the
offset counts, because "0 is the newest" and "0 is the oldest" are equally
plausible to the next reader and only one is right.

- [ ] **Step 4: Handle the keys**

`j`/`k` move by a line, `ctrl-d`/`ctrl-u` by a body height, `G` to the end.
Any backward movement clears the follow flag; `G` and `f` restore it. Clamp
against the survivor count.

- [ ] **Step 5: Draw from the offset, and show the indicator**

`view/bleats_full.rs` currently takes the last `body_rows` survivors. It now
takes the window the offset names, still oldest-first with the newest at the
bottom. The right-aligned `█ following` indicator appears only while following.

- [ ] **Step 6: Gate and commit**

The four commands, one at a time.

---

## Task 8: Wrap, and stepping between matches

**Added 2026-09-08.** The last two keys in the design's status bar: `w wrap`
and `n/N match`.

**Files:**
- Modify: `crates/shep-cli/src/lookout/input.rs`
- Modify: `crates/shep-cli/src/lookout/app.rs` (`on_bleats_key`)
- Modify: `crates/shep-cli/src/lookout/pane_bleats.rs`
- Modify: `crates/shep-cli/src/lookout/view/bleats_full.rs`

**Interfaces:**
- Consumes: the scroll offset and follow flag from Task 7, the match axis and
  its highlighting from Tasks 3 and 4.
- Produces: `KeyPress::WrapToggle`, `KeyPress::MatchNext`,
  `KeyPress::MatchPrev`.

**Wrap changes how many rows a line occupies, so it changes what the scroll
offset means.** A wrapped line takes as many rows as it needs; the offset
counts lines, and the draw has to turn lines into rows. Get this wrong and
scrolling near the end of a wrapped feed walks off the bottom. Task 7's clamp
is the thing to extend, not to duplicate.

**`n` and `N` step between lines that match the match axis**, which is only
meaningful while that axis is set. With no matcher they do nothing rather than
moving by one line, since a silent fallback to line movement is worse than an
inert key. Stepping sets the offset and clears the follow flag, the same as any
other backward movement.

The highlighting from Task 4 already finds the match positions. Use what it
computes rather than running the matcher a second time with a different
implementation, which is how the two drift apart.

- [ ] **Step 1: Write the failing tests**

```rust
/// `n` with no match axis set does nothing, rather than quietly becoming
/// a line-movement key.
#[test]
fn n_without_a_matcher_does_nothing() {
    let mut app = fixtures::bleats_pane_with_lines(120);
    let before = app.bleats_pane().expect("open").scroll_offset();
    let _ = app.update(Msg::Key(KeyPress::MatchNext));
    assert_eq!(app.bleats_pane().expect("open").scroll_offset(), before);
    assert!(app.bleats_pane().expect("open").following());
}

/// Wrapping a long line makes it occupy more rows than one, which is the
/// whole point, and the pane must still draw inside its area.
#[test]
fn a_wrapped_line_occupies_more_rows_and_stays_in_the_area() {
    let mut app = fixtures::bleats_pane_with_long_line();
    let unwrapped = fixtures::draw_lines(&app, 80, 20).len();
    let _ = app.update(Msg::Key(KeyPress::WrapToggle));
    let wrapped = fixtures::draw_lines(&app, 80, 20);
    assert!(wrapped.len() <= 20, "never draws past its own height");
    assert!(
        wrapped.iter().filter(|line| !line.spans.is_empty()).count() >= unwrapped,
        "wrapping uses at least as many rows as not wrapping"
    );
}
```

`fixtures::bleats_pane_with_long_line` does not exist; build it, with a line
comfortably wider than 80 columns.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --workspace --all-features --lib --bins -- n_without_a_matcher a_wrapped_line_occupies`
Expected: FAIL, no `KeyPress::MatchNext` and no `KeyPress::WrapToggle`.

- [ ] **Step 3: Add the three keys**

`w`, `n` and `N` are unbound; confirm before adding.

- [ ] **Step 4: Wrap**

A flag on `BleatsPane`, and a draw that turns one line into as many rows as it
needs. Use `columns` from the width module rather than `chars().count()`: this
repo has fixed that same bug in two other branches and a third would reintroduce
it.

- [ ] **Step 5: Step between matches**

`n` forward, `N` back, both clamped, both clearing the follow flag, both inert
with no matcher set.

- [ ] **Step 6: Update the status bar**

The pane's status bar now carries the design's full line: `esc back`,
`j/k line`, `ctrl-d/u page`, `G end`, `/ search`, `n/N match`, `f follow`,
`w wrap`, `o out/err/both`, and right-aligned `█ following`. Add `m` for the
level axis, which the design's own status bar omits, and say in the report that
it was added.

- [ ] **Step 7: Gate and commit**

The four commands, one at a time.

---

## Task 9: The scene, the gallery and the docs

**Files:**
- Modify: `crates/shep-cli/src/lookout/frames.rs`
- Create: a snapshot under `crates/shep-cli/src/lookout/snapshots/`
- Modify: `docs/lookout/frames.txt`, `docs/lookout/frames.ansi` (generated)
- Modify: `web/src/pages/docs/lookout.astro`, `docs/lookout/README.md`

**Interfaces:**
- Consumes: everything above.
- Produces: nothing.

**Adding a scene touches more places than it looks.** All of these, and the first four fail to compile if missed:

- the variant inside the `scenes!` block, with a doc comment (`frames.rs:164-250`)
- `label()` (`frames.rs:256`), exhaustive, no wildcard
- `caption()` (`frames.rs:302`), exhaustive; a test asserts the caption is a real sentence over 30 characters
- `scene_after()` (`frames.rs:2359`), test-only and exhaustive, and `scene_all_lists_every_variant_the_compiler_can_see` walks the chain and compares it to `Scene::ALL` in order
- `scene_with()`'s construction matches (`frames.rs:568` and the others listed at 916, 989, 1025, 1077)
- `control()` (`frames.rs:418`) and `size()` (`frames.rs:427`) both have wildcard arms, so they compile without a new arm. Add one anyway if the pane needs a non-default size.

- [ ] **Step 1: Add the scene**

Add the variant, its label, its caption, its `scene_after` arm and its construction. Build a scene showing the pane with all three filters set, since an unfiltered pane looks like the dashboard feed and pins nothing interesting.

- [ ] **Step 2: Run the scene tests**

Run: `cargo test --workspace --all-features --lib --bins -- scene_all_lists every_scene_shows_the_thing`
Expected: PASS.

- [ ] **Step 3: Accept the snapshot**

Run: `cargo test --workspace --all-features -- frames_are_pinned`
Read the generated `.snap` before accepting it. A snapshot accepted without being read pins whatever bug was rendered.

- [ ] **Step 4: Regenerate the gallery**

```bash
cargo test --workspace --all-features -- write_the_gallery --ignored
```

`docs/lookout/frames.txt` and `docs/lookout/frames.ansi` are generated. Confirm the diff shows the new scene and nothing else moved.

- [ ] **Step 5: Update the prose**

`docs/lookout/README.md` and `web/src/pages/docs/lookout.astro` describe what lookout's screens are. Add the pane: the key that opens it, the three axes, that they compose with AND, that `esc` drops the newest chip before closing, and that a line with no level is always shown.

**This is prose a person reads. Run the `humanizer` skill, then `rin-voice`, before committing.** No em dashes.

- [ ] **Step 6: Build the site**

```bash
cd web && npx astro build
```
```bash
cd web && npx astro check
```

`check` is the one that catches a wrong component prop; `build` passes with those.

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

Two commits, since the scene and the published docs are separate concerns:

```bash
git add crates/shep-cli/src/lookout/ docs/lookout/frames.txt docs/lookout/frames.ansi
git commit -m "test(lookout): pin the bleats pane in the frame gallery"
git add docs/lookout/README.md web/src/pages/docs/lookout.astro
git commit -m "docs(lookout): describe the full-screen bleats pane"
```

---

## Self-review

**Correction, 2026-09-08, found during Task 3's review.** The coverage claim
below said "The three axes: Tasks 2 and 3" and that was false for half of one
axis. The spec's match row reads "text **or regex**, with matches
**highlighted**". Neither `regex` nor `highlight` appeared anywhere in this
plan, so a spec requirement had no task at all and the self-review passed it
anyway.

Ruled into Task 4, which renders the filter row and is where highlighting
belongs. The cost is small and was checked rather than assumed: `regex` is
already a workspace dependency (`Cargo.toml:89`), already used by shep-core,
and already in shep-cli's dependency graph, so honouring the spec row adds one
`regex.workspace = true` line and no new crate to the tree.

**Spec coverage.** Pane shape and pinned sheep: Task 1. Source and the no-bus rule: Task 1, with the constraint stated globally. The three axes: Tasks 2 and 3, with the match axis's regex and highlighting in Task 4 (see the correction above). The unclassifiable-line rule: Task 2 defines it, Task 3 tests it. `esc` semantics: Task 3. Dropped line-number column: Task 4. Window-scoped survivor count: Task 4. Faster polling: Task 5. The filter keys: Task 6. Scrolling and follow: Task 7. Wrap and match stepping: Task 8. Scene, gallery and docs: Task 9.

**Second correction, 2026-09-08.** The coverage claim above accounted for the filter axes and never asked whether an operator could reach them. They could not: `KeyPress::FilterStart` sat in `on_bleats_key`'s inert arm, and no key touched stream or level. `docs/lookout/design-files/README.md:274` gives this pane a nine-key status bar and the plan implemented one key of it, `esc`. The maintainer chose the full status bar over the filter keys alone, which is Tasks 6 through 8.

**One spec item deliberately has no task.** The spec's "no per-sheep log topics" is a decision not to build something, recorded so a later reader knows it was considered. Nothing to implement.

**Ordering.** Task 2 is independent and could run first. Task 3 consumes it. Task 4 consumes Task 3. Tasks 5 and 6 consume Task 1. Task 6 last, so the scene pins the finished pane rather than being re-accepted after every task.

**Names checked against the tree, not carried from the spec.** `Body` (`app.rs:1251`), `close_pane` (`:2668`), `config_pane` (`:4247`), `App::feed` (`:4145`), `tail::Tail`/`TailLine`/`Stream` (`tail.rs:34-61`), `tail::read` (`tail.rs:104`), `link::FLOCK_POLL` (`link.rs:29`), the `polls` channel (`link.rs:173`), `cell::*` (`cell.rs:24-101`), `Palette::band`/`ground` (`theme.rs:194`, `:208`), the width sweep (`view/mod.rs:957`).

**Helpers this plan invents, marked so the implementer builds rather than hunts.** `fixtures::draw_lines` in Task 4 and the clock value in Task 5's tick tests. `fixtures::with_selection`, `app_with`, `plain`, `render_all` and `rendered` were all confirmed to exist. `Effect::RefreshFeed` and `Effect::PollNow` both exist; the pre-flight scan established that this pane wants the first.
