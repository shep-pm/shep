# Lookout frame 1k, the keymap overlay: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A bordered box over the dimmed dashboard listing every key lookout binds, grouped by what the key does, with its rows derived from `map_key` rather than from a list beside it.

**Architecture:** `keymap.rs` holds the row table and an exhaustive `KeyPress` match, and builds the row list by pushing a probe list of keys through `map_key` itself. `view/keymap.rs` draws it. 1g's box, borderless fallback and mute pass move to `view/overlay.rs` parameterised on interior width, so both overlays share one drawer. The overlay is a `bool` on `App`, not a third `InputMode`.

**Tech Stack:** Rust 2024, MSRV 1.88, ratatui, crossterm. No new dependencies.

**Spec:** [docs/brainstorming/specs/2026-09-13-lookout-1k-keymap-overlay-design.md](../../brainstorming/specs/2026-09-13-lookout-1k-keymap-overlay-design.md)

## Global Constraints

- **One cargo shape for every task in this plan**, so nothing churns the target dir: `cargo test -p shep --lib --all-features -- --skip ::slow::`. To run one test, `cargo test -p shep --lib --all-features <name>`. The package is `shep`, not `shep-cli`: `-p shep-cli` runs zero tests and exits 0.
- **Conventional commit subjects, `type(scope): summary`.** `feat`, `fix`, `perf` and `refactor` produce changelog entries; `docs`, `test`, `ci`, `chore` and `style` are skipped. `revert` and `build` are refused by the parser and vanish. A `!` goes on the commit that actually breaks something, in the crate that breaks. `.github/workflows/commits.yml` gates this and `.githooks/commit-msg` catches it earlier. The scope for every task here is `lookout`, except Task 9's docs commits.
- **`#![forbid(unsafe_code)]` is live in shep-cli.** Nothing in this plan needs unsafe.
- **Invoke the `shep-idiomatic-rust` skill before writing any Rust.** It fronts 47 numbered rules; cite them as `IR-<n>`. The ones this plan touches most: every public item needs docs and a deliberate `Debug` decision (IR-41), `# Errors` sections on fallible public functions, `core::error::Error` not `std::error::Error`, and `#[track_caller]` on anything with a `# Panics` section.
- **Never run the full workspace suite mid-task.** The shape above is the loop. The workspace gate runs once, at the end of Task 10.
- **Do not poll CI.** Report DONE and let the main thread watch the run.
> **Corrected 2026-09-14, after the work shipped.** This plan said in five
> places that `docs/lookout/design-files/rulings.md` states a 132-column floor
> for 1k. It does not, and it gives 1k no width at all. The 132 was in
> `docs/lookout/design-files/README.md:332`, which now reads 130 with the
> arithmetic beside it. Task 10's Step 1 below would have added a correction
> section to `rulings.md` announcing "this file said 132", which would have put
> a false correction into the authority document. It was never run: the real
> citation was found during Task 1 and fixed in `view/overlay.rs` and the spec.
> Flagged by CodeRabbit on the pull request.
>
> Sweeping for the class rather than the instance found a second one it did not
> name: the U+2600 glyph ceiling was also attributed to `rulings.md`, here and
> in shipped code at `view/overlay.rs`. It is `design-files/README.md:67`,
> "no glyph above U+2600". Both now name the README.

- **The design document is not the authority; `rulings.md` is, and the code outranks both on what a key does.** Where this plan quotes existing code, grep it rather than trusting the quote: line numbers move.

---

## Where the files sit

| file | responsibility | task |
|---|---|---|
| `crates/shep-cli/src/lookout/view/overlay.rs` | **new.** The box, the borderless fallback, the mute pass. Shared by 1g and 1k, parameterised on interior width. | 1 |
| `crates/shep-cli/src/lookout/view/pane.rs` | loses the box machinery to `overlay.rs`; `top_line` learns to draw the blurb with no panel | 1, 2 |
| `crates/shep-cli/src/lookout/pane.rs` | loses `help_open` and its four methods | 2 |
| `crates/shep-cli/src/lookout/keymap.rs` | **new.** `Binding`, `Group`, `binding()`, `PROBE`, `rows()`, and the three guard tests | 3 |
| `crates/shep-cli/src/lookout/input.rs` | `?` joins `h` on `KeyPress::Help` | 4 |
| `crates/shep-cli/src/lookout/app.rs` | `keymap_open`, `open_keymap()`, `on_keymap_key()`, the eight `Help` arms | 2, 5 |
| `crates/shep-cli/src/lookout/view/keymap.rs` | **new.** The overlay's own rows and its two forms | 6, 7 |
| `crates/shep-cli/src/lookout/view/mod.rs` | the draw hook at the end of `draw` | 6 |
| `crates/shep-cli/src/lookout/view/status.rs` | `FROZEN_HINT` gains `h keymap` | 8 |
| `crates/shep-cli/src/lookout/frames.rs` | eight new scenes | 9 |
| `docs/`, `web/` | the docs trigger | 10 |

**Dependencies.** Task 1 and Task 2 both edit `view/pane.rs`, so they run in order. Task 3 and Task 4 are independent of both and of each other. Task 5 needs Task 2's `on_pane_key` edits in place. Task 6 needs Tasks 1, 3 and 5. Task 7 needs Task 6. Task 8 needs Task 6's gate line. Task 9 needs Tasks 6, 7 and 8. Task 10 needs everything.

---

## Task 1: Extract 1g's box machinery into `view/overlay.rs`

A pure refactor. 1g's four pinned snapshots must not move, and that is the test.

**Files:**
- Create: `crates/shep-cli/src/lookout/view/overlay.rs`
- Modify: `crates/shep-cli/src/lookout/view/pane.rs` (remove the moved items, call the new ones)
- Modify: `crates/shep-cli/src/lookout/view/mod.rs` (add `mod overlay;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```rust
  pub(super) const fn floor_for(interior: u16) -> u16;
  pub(super) const fn is_boxed(width: u16, interior: u16) -> bool;
  pub(super) fn boxed_height(lines: &[Line<'static>]) -> u16;
  pub(super) fn draw_boxed(
      lines: &[Line<'static>],
      interior: u16,
      palette: Palette,
      area: Rect,
      buffer: &mut Buffer,
  );
  pub(super) fn blank_row(buffer: &mut Buffer, x: u16, y: u16, width: u16);
  pub(super) fn mute(buffer: &mut Buffer, area: Rect, palette: Palette);
  ```

- [ ] **Step 1: Read what is moving**

Read `crates/shep-cli/src/lookout/view/pane.rs` and locate, by grep rather than by line number:

- `BOX_WIDTH`, `BOX_FLOOR`
- `BOX_TOP_LEFT`, `BOX_TOP_RIGHT`, `BOX_BOTTOM_LEFT`, `BOX_BOTTOM_RIGHT`, `BOX_LEFT`, `BOX_TOP`, `BOX_BOTTOM`, `BOX_RIGHT`
- `dialog_is_boxed`
- `boxed_dialog_height`
- `draw_boxed_close_dialog`
- `blank_row`
- the two `buffer.set_style` calls in `draw_pane` that dim the pane behind the dialog, and the long comment above them

`draw_borderless_close_dialog` and `shed_dialog_rows` stay in `pane.rs`: they read a `CloseDialog` and build its rows, which is 1g's content rather than shared machinery.

- [ ] **Step 2: Write the failing test**

In `crates/shep-cli/src/lookout/view/overlay.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The floor is the interior plus a border cell and a margin cell each
    /// side. 1g's own floor is 90 over an 86-cell interior and 1k's is 130
    /// over a 126-cell one, and both come out of this one expression: the
    /// design README gave 132 for 1k, which would be a two-cell margin
    /// neither frame asks for.
    #[test]
    fn both_frames_floors_come_out_of_one_expression() {
        assert_eq!(floor_for(86), 90, "1g");
        assert_eq!(floor_for(126), 130, "1k");
    }

    /// The box draws at its floor and gives way one column under it.
    #[test]
    fn the_floor_is_the_narrowest_boxed_width() {
        assert!(is_boxed(90, 86));
        assert!(!is_boxed(89, 86));
        assert!(is_boxed(130, 126));
        assert!(!is_boxed(129, 126));
    }
}
```

- [ ] **Step 3: Run it and watch it fail**

Run: `cargo test -p shep --lib --all-features overlay::`
Expected: FAIL — `overlay.rs` does not exist, so the module does not resolve.

- [ ] **Step 4: Write `overlay.rs`**

Move the items from Step 1 verbatim, with these changes and nothing else:

```rust
//! The box, the borderless fallback's frame, and the mute pass behind
//! them. Shared by the close dialog (1g) and the keymap overlay (1k).
//!
//! Both frames draw a bordered box over a dimmed body, differing in one
//! number: 1g's interior is 86 cells and 1k's is 126. Written once here
//! rather than twice, because two box drawers differing in a constant is
//! the shape four `create_*_file` helpers took in shep-core before one
//! helper replaced them.

/// The narrowest terminal that draws a box with an `interior`-cell inside.
///
/// The interior, a border cell each side, and a margin cell each side. 1g's
/// 86 gives 90 and 1k's 126 gives 130, and
/// `draw_boxed`'s own `margin` arithmetic comes out at 1 at either floor.
///
/// `docs/lookout/design-files/README.md` gave 132 for 1k, which is a two-cell
/// margin 1g does not ask for. Corrected there rather than special-cased
/// here, so this stays one expression for both frames. Not `rulings.md`,
/// which gives 1k no width.
pub(super) const fn floor_for(interior: u16) -> u16 {
    interior + 4
}

/// Whether a `width`-column terminal draws the box, or gives way to the
/// borderless form.
pub(super) const fn is_boxed(width: u16, interior: u16) -> bool {
    width >= floor_for(interior)
}
```

`boxed_dialog_height` becomes `boxed_height`, unchanged in body, keeping its whole doc comment about why one function does the addition.

`draw_boxed_close_dialog` becomes `draw_boxed` and takes `interior: u16` where it read `BOX_WIDTH`. Every `BOX_WIDTH` inside it becomes `interior`. Nothing else changes.

`blank_row` moves unchanged, comment intact.

The mute pass becomes:

```rust
/// Dims everything already drawn in `area`, so an overlay reads as a
/// question about what is behind it rather than as a new screen.
///
/// Two calls, not one: `Buffer::set_style` (ratatui-core 0.1.2,
/// `buffer/buffer.rs:405`) patches a cell rather than replacing it, so a
/// single `palette.muted()` would leave a title band's reverse video and a
/// selected row's own ground sitting under the new ink. `Style::reset()`
/// clears both back to the terminal's default first; `palette.muted()` then
/// repaints the one ink the overlay leaves the body in. Under `NO_COLOR` the
/// second call is a no-op (`Palette::muted` has no colour to give), so only
/// the reset runs and the body goes completely flat, which is the right
/// outcome there: the border and the reverse-video heading carry the
/// separation on their own.
pub(super) fn mute(buffer: &mut Buffer, area: Rect, palette: Palette) {
    buffer.set_style(area, Style::reset());
    buffer.set_style(area, palette.muted());
}
```

Add `mod overlay;` to `view/mod.rs` beside the other `mod` lines.

- [ ] **Step 5: Rewire `pane.rs`**

In `pane.rs`, keep `BOX_WIDTH: u16 = 86` as the close dialog's own interior and delete `BOX_FLOOR`, replacing its uses with `overlay::floor_for(BOX_WIDTH)`. Replace `dialog_is_boxed(area.width)` with `overlay::is_boxed(area.width, BOX_WIDTH)`, `boxed_dialog_height(&lines)` with `overlay::boxed_height(&lines)`, `draw_boxed_close_dialog(&lines, palette, area, buffer)` with `overlay::draw_boxed(&lines, BOX_WIDTH, palette, area, buffer)`, and the two `set_style` calls in `draw_pane` with `overlay::mute(buffer, area, app.palette())`. `blank_row` calls become `overlay::blank_row`.

`the_border_vocabulary_is_the_one_that_was_checked` moves to `overlay.rs` with the glyphs, comment intact.

- [ ] **Step 6: Run the tests, including 1g's snapshots**

Run: `cargo test -p shep --lib --all-features -- --skip ::slow::`
Expected: PASS, with **no snapshot changed**. Then:

Run: `git status --short`
Expected: no `.snap.new` file. A changed 1g snapshot means the refactor moved a cell, and the refactor is wrong — do not accept the new snapshot.

- [ ] **Step 7: Commit**

```bash
git add crates/shep-cli/src/lookout/view/overlay.rs crates/shep-cli/src/lookout/view/pane.rs crates/shep-cli/src/lookout/view/mod.rs
git commit -m "refactor(lookout): share the overlay box between 1g and 1k"
```

---

## Task 2: The config pane's blurb at every width, and `help_open` goes

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/pane.rs` (`top_line`)
- Modify: `crates/shep-cli/src/lookout/pane.rs` (delete four methods and a field)
- Modify: `crates/shep-cli/src/lookout/app.rs` (`on_pane_key`'s `Help` and `Escape` arms, and the `set_help_open` call site)

**Interfaces:**
- Consumes: nothing.
- Produces: `ConfigPane::help_open`, `toggle_help`, `close_help` and `set_help_open` no longer exist. `on_pane_key`'s `Help` arm is inert, for Task 5 to claim.

- [ ] **Step 1: Find every caller first**

```bash
grep -rn "help_open\|toggle_help\|close_help\|set_help_open" crates/shep-cli/src/
```

Expect the field and four methods in `pane.rs`, `top_line` in `view/pane.rs`, the `Help` and `Escape` arms in `app.rs`, one rebuild site calling `set_help_open`, and tests in all three. Every one goes.

- [ ] **Step 2: Write the failing tests**

In `crates/shep-cli/src/lookout/view/pane.rs`'s test module:

```rust
    /// At a width with no explanation panel, the field under the cursor
    /// still has its help text on screen, with no key pressed.
    ///
    /// 89 columns is the widest terminal `panel_width` refuses: the panel
    /// clamps to `PANEL_MIN` 50 and 89 - 50 is 39, one short of `LEFT_MIN`.
    /// `h` used to be the only route to this text, which is why it survived
    /// the panel that made it redundant everywhere else.
    #[test]
    fn the_blurb_draws_at_a_width_with_no_panel() {
        let pane = web_pane();
        assert!(panel_width(89).is_none(), "89 must have no panel");
        let lines = pane_lines(&pane, Palette::detect(false, false), 89, 40);
        let help = first_field_help(&pane);
        assert!(
            text_of(&lines).iter().any(|row| row.contains(&help)),
            "no blurb at 89 columns: {:?}",
            text_of(&lines)
        );
    }

    /// And it describes the row the cursor is on, not the first field.
    #[test]
    fn the_blurb_follows_the_cursor_with_no_panel() {
        let mut pane = web_pane();
        let first = first_field_help(&pane);
        pane.move_by(1);
        let second = field_help_under_cursor(&pane);
        assert_ne!(first, second, "the fixture needs two differing blurbs");
        let lines = pane_lines(&pane, Palette::detect(false, false), 89, 40);
        let rows = text_of(&lines);
        assert!(
            rows.iter().any(|row| row.contains(&second)),
            "the cursor moved and the blurb did not: {rows:?}"
        );
        assert!(
            !rows.iter().any(|row| row.contains(&first)),
            "the previous field's blurb is still on screen: {rows:?}"
        );
    }

    /// At the design target the panel draws the blurb, and the fix must not
    /// have added a second copy above the field list.
    #[test]
    fn the_panel_is_the_only_blurb_where_it_draws() {
        let pane = web_pane();
        assert!(panel_width(160).is_some(), "160 must have a panel");
        let lines = pane_lines(&pane, Palette::detect(false, false), 160, 48);
        let help = first_field_help(&pane);
        let hits = text_of(&lines)
            .iter()
            .filter(|row| row.contains(&help))
            .count();
        assert_eq!(hits, 1, "the blurb is on screen {hits} times, not once");
    }
```

Add the two helpers next to `text_of`:

```rust
    /// The `help` string of the field the pane's cursor starts on.
    fn first_field_help(pane: &ConfigPane) -> String {
        field_help_under_cursor(pane)
    }

    /// The `help` string of the field under the cursor, whichever it is.
    fn field_help_under_cursor(pane: &ConfigPane) -> String {
        let Some(PaneRow::Field(index)) = pane.cursor() else {
            panic!("the cursor is not on a field");
        };
        pane.fields().fields()[index].help.clone()
    }
```

The `assert_ne!` in the second test is load-bearing: it fails loudly if the fixture's first two fields happen to share a help string, rather than letting the test pass on text that was there for an unrelated reason.

- [ ] **Step 3: Run them and watch them fail**

Run: `cargo test -p shep --lib --all-features blurb`
Expected: `the_blurb_draws_at_a_width_with_no_panel` and `the_blurb_follows_the_cursor_with_no_panel` FAIL — no key was pressed, so `help_open` is false and `top_line` returns `None`. `the_panel_is_the_only_blurb_where_it_draws` PASSES already; that is correct, and it is there to fail if Step 4 doubles the text.

- [ ] **Step 4: Make `top_line` read the width instead of a key**

Replace `top_line` with:

```rust
/// The rows the field list reserves under its title: the selected field's
/// own help text, wrapped, at widths where the explanation panel cannot
/// draw it. Empty where the panel does.
///
/// One route to a field's help, and it follows the cursor. `h` used to be
/// the other, drawing the same `field.help` string on one line under the
/// title, which duplicated `panel_for_field`'s own second region everywhere
/// the panel drew and was the only route below `panel_width`'s floor of 90
/// columns. Making this unconditional retired the key rather than leaving
/// its meaning to depend on the terminal's width, and freed `h` for the
/// keymap overlay the design always wanted on it.
fn top_lines(pane: &ConfigPane, palette: Palette, width: u16) -> Vec<(String, Style)> {
    if panel_width(width).is_some() {
        return Vec::new();
    }
    let Some(PaneRow::Field(index)) = pane.cursor() else {
        return Vec::new();
    };
    let Some(field) = pane.fields().fields().get(index) else {
        return Vec::new();
    };
    // Two columns for the indent this row draws with, the same budget
    // `panel_for_field`'s own blurb wraps to.
    wrap(&field.help, usize::from(BLURB_WRAP.min(width.saturating_sub(2))))
        .into_iter()
        .map(|row| (format!("  {row}"), palette.muted()))
        .collect()
}
```

Update `pane_lines`'s call site: it takes a `Vec` of rows now rather than an `Option` of one, so the rows it reserves under the title is `top_lines(...).len()` rather than `usize::from(top_line(...).is_some())`. Grep for the existing call and follow the arithmetic it feeds — the field list's own row budget subtracts it.

Then delete `help_open`, `toggle_help`, `close_help` and `set_help_open` from `pane.rs` along with the field, delete `toggling_help_flips_it_and_closing_it_is_idempotent`, and delete the `set_help_open` call at its rebuild site.

- [ ] **Step 5: Fix `on_pane_key`'s two arms**

`Help` becomes inert, with a comment saying who claims it next:

```rust
            // Inert here until Task 5 routes it to the keymap overlay. The
            // field help this used to toggle is unconditional now: see
            // `view::pane::top_lines`.
            KeyPress::Help => {}
```

`Escape` loses its first rung. The cascade was "help first, if it is open, else the close dialog's own question, else the pane", and there is no help to close:

```rust
            // Backs out one level at a time: the close dialog's own
            // question first, if there is one, else the pane. `Escape`
            // closes rather than cascading to a filter clear or a quit,
            // exactly as it does on the settings screen.
            //
            // The dialog is asked before anything is taken: `esc` used to
            // write first and ask second, which missed the very edit that
            // made this pane's `Escape` worth asking about. Now nothing
            // leaves the pane until the question is answered, one way or
            // another.
            //
            // There used to be a rung above the dialog, closing the field
            // help `h` had opened. The help is unconditional now, so an
            // operator who could see a blurb no longer presses `esc` twice
            // to leave the pane.
            KeyPress::Escape => {
                if let Some(dialog) = self.close_offer() {
```

- [ ] **Step 6: Pin the `Escape` change**

Add to `app.rs`'s tests:

```rust
    /// `esc` leaves the config pane on the first press, with a blurb on
    /// screen. It used to take two: `h`'s field help was a rung above the
    /// close dialog's question in `Escape`'s cascade, and the blurb is
    /// unconditional now, so there is no rung to spend.
    #[test]
    fn esc_leaves_the_pane_on_one_press_with_a_blurb_showing() {
        let mut app = app_with_config_pane();
        assert!(app.config_pane().is_some());
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            app.config_pane().is_none(),
            "one esc did not leave the pane"
        );
    }
```

Use whichever existing helper in `app.rs`'s test module opens a config pane on a sheep with no pending edits; grep for one rather than writing a new fixture. If the only helpers file edits, use one and assert on `close_dialog().is_some()` after the first `esc` instead, with the comment saying why an edited pane asks a question first.

- [ ] **Step 7: Run the suite**

Run: `cargo test -p shep --lib --all-features -- --skip ::slow::`
Expected: PASS. Snapshots for `EditPaneNarrow` (88 columns) **will** change, because that scene now carries a blurb it did not before. That is the point of the task; review the diff and confirm the new rows are the focused field's own help text before accepting it.

- [ ] **Step 8: Mutate each new test and watch it fail**

For each of the three blurb tests and the `esc` test: break the thing it claims to pin, run it, confirm it fails, restore.

- `the_blurb_draws_at_a_width_with_no_panel` — make `top_lines` return `Vec::new()` unconditionally.
- `the_blurb_follows_the_cursor_with_no_panel` — make `top_lines` read `fields()[0]` instead of the cursor's index.
- `the_panel_is_the_only_blurb_where_it_draws` — delete the `panel_width(width).is_some()` early return.
- `esc_leaves_the_pane_on_one_press_with_a_blurb_showing` — put an unconditional `return Effect::None` at the top of the `Escape` arm.

Record the four failures in the commit body. A test that still passes with its subject broken pins nothing, which is how six tests shipped across the panes already built.

- [ ] **Step 9: Commit**

```bash
git add crates/shep-cli/src/lookout/view/pane.rs crates/shep-cli/src/lookout/pane.rs crates/shep-cli/src/lookout/app.rs crates/shep-cli/src/lookout/snapshots
git commit -m "refactor(lookout)!: draw a field's help at every width, and retire h"
```

The `!` is deliberate: `h` stops showing field help in the config pane, which is operator-visible behaviour, and `shep-cli` is the crate that breaks.

---

## Task 3: `keymap.rs`, the rows derived from `map_key`

**Files:**
- Create: `crates/shep-cli/src/lookout/keymap.rs`
- Modify: `crates/shep-cli/src/lookout/mod.rs` (add `mod keymap;`)

**Interfaces:**
- Consumes: `super::app::{ActionVerb, InputMode, KeyPress}`, `super::input::map_key`.
- Produces:
  ```rust
  pub(super) enum Group { Moving, Looking, Changing, Doing, Closing }
  impl Group {
      pub(super) const fn heading(self) -> &'static str;   // None-ish for Closing
      pub(super) const fn role(self) -> Role;
      pub(super) const DRAWN: [Self; 4];   // the four columns; Closing is not one
  }
  pub(super) struct Binding { pub keys: &'static str, pub group: Group, pub does: &'static str }
  pub(super) fn rows() -> Vec<Binding>;   // 36: 35 in drawn columns, plus q
  pub(super) const ENTRY_ROWS: usize = 12;
  pub(super) const KEY_CELL: u16 = 12;
  pub(super) const TEXT_CELL: u16 = 17;
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    /// Every `KeyPress` `map_key` can produce is produced by some `PROBE`
    /// entry too.
    ///
    /// The sweep is the whole surface: `map_key` matches on `Char`, `Esc`,
    /// `Enter`, `Backspace`, `Tab`, `BackTab`, the four arrows, `Home`,
    /// `End` and `F(n)`, and nothing else. A binding added to `map_key` and
    /// not to `PROBE` fails here rather than going missing from the
    /// overlay, which is the whole reason the rows are built by running
    /// `map_key` instead of listed beside it.
    #[test]
    fn every_key_map_key_binds_is_in_the_probe() {
        let probed: Vec<KeyPress> = PROBE
            .iter()
            .filter_map(|(code, modifiers)| {
                map_key(
                    &Event::Key(KeyEvent::new(*code, *modifiers)),
                    InputMode::Normal,
                )
            })
            .chain(PROBE.iter().filter_map(|(code, modifiers)| {
                map_key(
                    &Event::Key(KeyEvent::new(*code, *modifiers)),
                    InputMode::Text,
                )
            }))
            .collect();

        for (code, modifiers) in every_key() {
            for mode in [InputMode::Normal, InputMode::Text] {
                let Some(press) = map_key(&Event::Key(KeyEvent::new(code, modifiers)), mode) else {
                    continue;
                };
                assert!(
                    probed.iter().any(|seen| same_row(seen, &press)),
                    "{code:?} with {modifiers:?} in {mode:?} binds {press:?}, \
                     which no PROBE entry produces"
                );
            }
        }
    }

    /// And the reverse: `PROBE` cannot accumulate keys that stopped being
    /// bound.
    #[test]
    fn every_probe_entry_binds_something() {
        for (code, modifiers) in PROBE {
            let normal = map_key(
                &Event::Key(KeyEvent::new(*code, *modifiers)),
                InputMode::Normal,
            );
            let text = map_key(
                &Event::Key(KeyEvent::new(*code, *modifiers)),
                InputMode::Text,
            );
            assert!(
                normal.is_some() || text.is_some(),
                "{code:?} with {modifiers:?} is in PROBE and binds nothing"
            );
        }
    }

    /// No group has more entries than its column has rows.
    ///
    /// Zero spare at the time of writing: `Looking` has twelve against
    /// twelve. A forty-third binding compiles, because `binding` gives it a
    /// row, and then overflows the box. This is the test that names which
    /// group grew.
    #[test]
    fn every_group_fits_its_column() {
        for group in Group::DRAWN {
            let count = rows().iter().filter(|row| row.group == group).count();
            assert!(
                count <= ENTRY_ROWS,
                "{group:?} has {count} entries against {ENTRY_ROWS} rows"
            );
        }
    }

    /// Thirty-six rows, thirty-five of them in a drawn column, and no two
    /// share a key caption.
    ///
    /// The thirty-sixth is `q  ctrl-c`, in `Group::Closing`: it has no row
    /// of its own in the box, which is what bought `Looking` its twelfth
    /// entry. Both counts are asserted, because one alone would pass if a
    /// row migrated between a column and the closing line.
    #[test]
    fn the_rows_are_deduplicated() {
        let rows = rows();
        let captions: Vec<&str> = rows.iter().map(|row| row.keys).collect();
        assert_eq!(rows.len(), 36, "{captions:?}");
        assert_eq!(
            rows.iter()
                .filter(|row| Group::DRAWN.contains(&row.group))
                .count(),
            35,
            "{captions:?}"
        );
        for (index, row) in rows.iter().enumerate() {
            assert!(
                !rows[index + 1..].iter().any(|other| other.keys == row.keys),
                "{} appears twice",
                row.keys
            );
        }
    }

    /// The two keys the design named wrongly, asserted here as well as in
    /// `input.rs`, because this is the file the overlay prints from.
    #[test]
    fn the_overlay_names_the_keys_that_shipped_not_the_ones_drawn() {
        let rows = rows();
        assert!(
            rows.iter().any(|row| row.keys == "b" && row.does.contains("feed")),
            "the feed is on `b`, not the design's `l`"
        );
        assert!(
            rows.iter().any(|row| row.keys == "S" && row.does.contains("secrets")),
            "secrets is on `S`, not the design's `g`"
        );
        assert!(
            !rows.iter().any(|row| row.keys == "l"),
            "`l` is unbound and must not appear"
        );
    }

    /// Every key the overlay prints in a caption is a key `map_key` binds.
    ///
    /// Catches the drift the derivation cannot: a caption edited to name a
    /// key that was never bound. Single-character captions only, since a
    /// caption like `g/G home/end` names four keys in one cell.
    #[test]
    fn a_single_character_caption_names_a_bound_key() {
        for row in rows() {
            let mut chars = row.keys.chars();
            let Some(only) = chars.next() else { continue };
            if chars.next().is_some() {
                continue;
            }
            assert!(
                map_key(
                    &Event::Key(KeyEvent::new(KeyCode::Char(only), KeyModifiers::NONE)),
                    InputMode::Normal
                )
                .is_some(),
                "the overlay prints `{only}` and `map_key` does not bind it"
            );
        }
    }

    /// Every `KeyCode` variant `map_key` can match, bare and with CONTROL.
    fn every_key() -> Vec<(KeyCode, KeyModifiers)> {
        let mut keys: Vec<KeyCode> = (0x20u8..=0x7E).map(|b| KeyCode::Char(b as char)).collect();
        keys.extend([
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Backspace,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
        ]);
        keys.extend((1..=12).map(KeyCode::F));
        keys.into_iter()
            .flat_map(|code| {
                [
                    (code, KeyModifiers::NONE),
                    (code, KeyModifiers::CONTROL),
                    (code, KeyModifiers::SHIFT),
                ]
            })
            .collect()
    }

    /// Whether two presses land on the same overlay row. `TextChar('a')`
    /// and `TextChar('z')` are one row, and `Group(1)` and `Group(8)` are
    /// one row, so equality is the wrong comparison.
    fn same_row(left: &KeyPress, right: &KeyPress) -> bool {
        binding(left).keys == binding(right).keys
    }
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p shep --lib --all-features keymap::`
Expected: FAIL — the module does not resolve.

- [ ] **Step 3: Write `keymap.rs`**

```rust
//! What the keymap overlay prints, derived from `map_key` rather than
//! listed beside it.
//!
//! [`binding`] is an exhaustive match over [`KeyPress`] with no wildcard
//! arm, so a new variant does not compile until it has a row here, and
//! [`rows`] builds the list by pushing [`PROBE`] through
//! [`super::input::map_key`] itself. A binding cannot reach the reducer
//! without also reaching the overlay, and a row cannot name a key nothing
//! is bound to.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use super::app::{ActionVerb, InputMode, KeyPress};
use super::input::map_key;
use crate::vocabulary::Role;

/// How many entry rows one column of the overlay has.
///
/// The box is nineteen rows: a border pair, a heading row, twelve entries,
/// a blank, the gate line, and two closing lines. Zero spare, which
/// `every_group_fits_its_column` is what guards.
pub(super) const ENTRY_ROWS: usize = 12;

/// One of the overlay's four columns, grouped by what the keys in it do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Group {
    Moving,
    Looking,
    Changing,
    Doing,
    /// Not a column. The one row that draws on the overlay's closing line
    /// instead: `q  ctrl-c`, which has to come out of [`binding`] like
    /// every other variant but must not take one of `Looking`'s twelve
    /// rows, since `Looking` is already at twelve.
    Closing,
}

impl Group {
    /// The four that draw as columns, left to right. [`Self::Closing`] is
    /// not among them.
    pub(super) const DRAWN: [Self; 4] = [Self::Moving, Self::Looking, Self::Changing, Self::Doing];

    /// The column's heading, or `""` for [`Self::Closing`], which has no
    /// column to head.
    pub(super) const fn heading(self) -> &'static str {
        match self {
            Self::Moving => "MOVING",
            Self::Looking => "LOOKING",
            Self::Changing => "CHANGING",
            Self::Doing => "DOING",
            Self::Closing => "",
        }
    }

    /// The heading's colour role. `Doing` is bark because its three keys
    /// are the destructive ones; the rest are meadow.
    pub(super) const fn role(self) -> Role {
        match self {
            Self::Doing => Role::Bark,
            Self::Moving | Self::Looking | Self::Changing | Self::Closing => Role::Meadow,
        }
    }
}

/// One row: the key column, its group, and the sentence beside it.
///
/// `keys` is at most [`KEY_CELL`] characters and `does` at most
/// [`TEXT_CELL`]; `the_cells_fit_their_widths` asserts both rather than
/// leaving a long one to be cut on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Binding {
    pub keys: &'static str,
    pub group: Group,
    pub does: &'static str,
}

/// The overlay's row for `press`.
///
/// Exhaustive, with no wildcard arm and no `Action(_)` catch-all, so both
/// [`KeyPress`] and [`ActionVerb`] have to grow a row here before they
/// compile. That is the whole guard against a binding that dispatches and
/// does not print.
const fn binding(press: &KeyPress) -> Binding {
    // `row` keeps each arm to one line, so a reader checks thirty-five
    // captions rather than thirty-five struct literals.
    const fn row(keys: &'static str, group: Group, does: &'static str) -> Binding {
        Binding { keys, group, does }
    }
    match press {
        KeyPress::SelectDown | KeyPress::SelectUp => {
            row("j/k  \u{2191}/\u{2193}", Group::Moving, "select a row")
        }
        KeyPress::SelectFirst | KeyPress::SelectLast => {
            row("g/G home/end", Group::Moving, "first / last")
        }
        KeyPress::StepDown | KeyPress::StepUp => {
            row("J/K", Group::Moving, "next / prev sheep")
        }
        KeyPress::PageDown | KeyPress::PageUp => row("ctrl-d/u", Group::Moving, "page the feed"),
        KeyPress::TabPrev | KeyPress::TabNext => {
            row("\u{2190}/\u{2192}", Group::Moving, "environment tab")
        }
        KeyPress::NextGroup => row("tab", Group::Moving, "next config group"),
        KeyPress::Group(_) => row("1-8", Group::Moving, "jump to a group"),
        KeyPress::MatchNext | KeyPress::MatchPrev => {
            row("n/N", Group::Moving, "next / prev match")
        }
        KeyPress::Confirm => row("\u{21b5}", Group::Moving, "open selection"),
        KeyPress::Escape => row("esc", Group::Moving, "back one level"),

        KeyPress::Help => row("h  ?", Group::Looking, "this keymap"),
        KeyPress::Bleats => row("b", Group::Looking, "feed, full screen"),
        KeyPress::FoldView => row("F", Group::Looking, "gather by fold"),
        KeyPress::Collapse => row("z", Group::Looking, "collapse a fold"),
        KeyPress::StreamCycle => row("o", Group::Looking, "out / err / both"),
        KeyPress::LevelCycle => row("m", Group::Looking, "minimum level"),
        KeyPress::FollowToggle => row("f", Group::Looking, "follow the tail"),
        KeyPress::WrapToggle => row("w", Group::Looking, "wrap long lines"),
        KeyPress::FilterStart => row("/", Group::Looking, "filter, or match"),
        KeyPress::Refresh => row("r", Group::Looking, "refresh now"),
        KeyPress::Reveal => row("v", Group::Looking, "reveal for 10s"),
        KeyPress::Copy => row("y", Group::Looking, "copy the value"),

        KeyPress::Edit => row("e", Group::Changing, "the config pane"),
        KeyPress::Cycle => row("space", Group::Changing, "cycle a value"),
        KeyPress::Remove => row("d", Group::Changing, "restore default"),
        KeyPress::Undo => row("u", Group::Changing, "undo the edit"),
        KeyPress::Settings => row("s", Group::Changing, "shepherd settings"),
        KeyPress::Secrets => row("S", Group::Changing, "secrets"),
        KeyPress::SecretDelete => row("D", Group::Changing, "delete a secret"),
        KeyPress::Continue => row("c", Group::Changing, "leave it running"),
        KeyPress::TextChar(_) => row("a-z 0-9", Group::Changing, "types into a box"),
        KeyPress::TextBackspace | KeyPress::TextApply | KeyPress::TextAbandon => {
            row("\u{232b} \u{21b5} esc", Group::Changing, "erase, file, drop")
        }

        KeyPress::Action(ActionVerb::Stop) => row("x", Group::Doing, "stop"),
        KeyPress::Action(ActionVerb::Restart) => row("R", Group::Doing, "restart"),
        KeyPress::Action(ActionVerb::Reload) => row("L", Group::Doing, "reload"),

        // `q` draws on the closing line beside `h or ? closes this`, not in
        // a column: `Looking` is at twelve of twelve and this would be its
        // thirteenth. `Group::Closing` is how it comes out of this match
        // without taking a column row.
        KeyPress::Quit => row("q  ctrl-c", Group::Closing, "quit"),
    }
}

/// Every key the overlay claims, as `map_key` would see it.
///
/// One entry per row rather than one per key: `Char('a')` stands for every
/// letter the text-mode row covers and `Char('1')` for the eight group
/// digits. `every_key_map_key_binds_is_in_the_probe` sweeps the whole
/// keyboard against this, so a key missing here is a test failure rather
/// than a row missing from the box.
const PROBE: &[(KeyCode, KeyModifiers)] = &[
    // ... one entry per distinct row, in no particular order; `rows`
    // sorts by group.
];

/// Every row the overlay draws, grouped, in the order each column lists
/// them.
///
/// Built by running [`PROBE`] through [`map_key`], so nothing here names a
/// key the reducer does not dispatch on.
pub(super) fn rows() -> Vec<Binding> {
    let mut out: Vec<Binding> = Vec::new();
    for group in Group::DRAWN.into_iter().chain([Group::Closing]) {
        for (code, modifiers) in PROBE {
            let event = Event::Key(KeyEvent::new(*code, *modifiers));
            let Some(press) =
                map_key(&event, InputMode::Normal).or_else(|| map_key(&event, InputMode::Text))
            else {
                continue;
            };
            let binding = binding(&press);
            if binding.group == group && !out.iter().any(|seen| seen.keys == binding.keys) {
                out.push(binding);
            }
        }
    }
    out
}
```

Fill `PROBE` with one `(KeyCode, KeyModifiers)` per row: the keys named in the captions above. `ctrl-d` needs `KeyModifiers::CONTROL`; the shifted letters (`G`, `J`, `K`, `S`, `D`, `R`, `L`, `N`) are `KeyCode::Char('G')` with `KeyModifiers::SHIFT`, matching how crossterm delivers a capital and how `input.rs`'s own `a_shifted_letter_is_still_a_letter_in_the_box` describes it. Run `every_probe_entry_binds_something` after each addition rather than at the end.

Add `mod keymap;` to `lookout/mod.rs`.

- [ ] **Step 4: Add the cell-width test**

```rust
    /// Every caption fits the cell it draws into, so nothing is cut on
    /// screen. The widths are `view::keymap`'s, asserted here because this
    /// is where the strings are written.
    #[test]
    fn the_cells_fit_their_widths() {
        for row in rows() {
            assert!(
                visible_width(row.keys) <= usize::from(KEY_CELL),
                "the `{}` caption is {} cells against {KEY_CELL}",
                row.keys,
                visible_width(row.keys)
            );
            assert!(
                visible_width(row.does) <= usize::from(TEXT_CELL),
                "`{}` is {} cells against {TEXT_CELL}",
                row.does,
                visible_width(row.does)
            );
        }
    }
```

`visible_width` is `crate::output::width::visible_width`, which `view/pane.rs`'s own tests already use. `KEY_CELL` (12) and `TEXT_CELL` (17) are declared here, in `keymap.rs`, since the captions live here and Task 6's drawer imports them.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p shep --lib --all-features keymap::`
Expected: PASS, all seven.

- [ ] **Step 6: Prove the compiler guard works**

Add a scratch variant to `KeyPress` in `app.rs`:

```rust
    ScratchDoNotCommit,
```

Run: `cargo check -p shep --all-features`
Expected: FAIL in `keymap.rs` — `` non-exhaustive patterns: `&KeyPress::ScratchDoNotCommit` not covered ``. Remove the variant. This is the guard the whole derivation rests on, and it is worth thirty seconds to see it fire.

- [ ] **Step 7: Mutate each test and watch it fail**

- `every_key_map_key_binds_is_in_the_probe` — delete `ctrl-d`'s `PROBE` entry.
- `every_probe_entry_binds_something` — add `(KeyCode::Char('l'), KeyModifiers::NONE)`, which is unbound.
- `every_group_fits_its_column` — set `ENTRY_ROWS` to 11.
- `the_rows_are_deduplicated` — drop the `!out.iter().any(...)` guard in `rows`.
- `the_overlay_names_the_keys_that_shipped_not_the_ones_drawn` — change `Bleats`' caption to `l`.
- `a_single_character_caption_names_a_bound_key` — change `Collapse`'s caption to `Z`.
- `the_cells_fit_their_widths` — lengthen one `does` string by two characters.

- [ ] **Step 8: Commit**

```bash
git add crates/shep-cli/src/lookout/keymap.rs crates/shep-cli/src/lookout/mod.rs
git commit -m "feat(lookout): derive the keymap's rows from map_key"
```

---

## Task 4: `?` joins `h`

**Files:**
- Modify: `crates/shep-cli/src/lookout/input.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `KeyCode::Char('?')` maps to `KeyPress::Help` in `InputMode::Normal`.

- [ ] **Step 1: Write the failing test**

```rust
    /// `?` is the keymap's second key, and `/` is untouched. They are the
    /// same physical key on most layouts, so a wrong arm here would take
    /// the name filter with it.
    #[test]
    fn question_mark_opens_the_keymap_and_slash_still_filters() {
        assert_eq!(
            map_key(&key(KeyCode::Char('?')), InputMode::Normal),
            Some(KeyPress::Help)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('/')), InputMode::Normal),
            Some(KeyPress::FilterStart)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('?')), InputMode::Text),
            Some(KeyPress::TextChar('?')),
            "a question mark typed into a box is a character"
        );
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p shep --lib --all-features question_mark`
Expected: FAIL — `` left: None, right: Some(Help) ``.

- [ ] **Step 3: Bind it**

In `map_key`'s `Normal` match, replace the `h` arm:

```rust
        // Two keys, one meaning, which is what the design asked for. `h` was
        // the config pane's field help until that became unconditional
        // (`view::pane::top_lines`); `?` is new here.
        KeyCode::Char('h' | '?') => Some(KeyPress::Help),
```

Update the doc comment on `KeyPress::Help` in `app.rs` in the same commit, since it still describes the field help. It describes behaviour Task 5 delivers, so for one commit it runs ahead of the reducer; that is the right trade against editing the same comment twice, and no mid-branch commit ships:

```rust
    /// `h` or `?`: opens the keymap overlay, from any body. Pressing either
    /// again, or `Escape`, closes it. Refused only while a close dialog is
    /// up, which owns the keyboard until it is answered.
    Help,
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p shep --lib --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 5: Mutate and watch it fail**

Change the arm to `Char('h' | '/')` and confirm the test fails on the `/` assertion, not only the `?` one. Restore.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout/input.rs crates/shep-cli/src/lookout/app.rs
git commit -m "feat(lookout): bind ? to the keymap alongside h"
```

---

## Task 5: The overlay's state and its keyboard

**Files:**
- Modify: `crates/shep-cli/src/lookout/app.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks; Task 2 must have left `on_pane_key`'s `Help` arm inert.
- Produces:
  ```rust
  impl App {
      pub fn keymap_open(&self) -> bool;
      fn open_keymap(&mut self) -> Effect;   // sets the flag, returns Effect::None
      fn on_keymap_key(&mut self, key: KeyPress) -> Effect;
  }
  ```

- [ ] **Step 1: Write the failing tests**

```rust
    /// The overlay swallows a movement key rather than letting it reach the
    /// table underneath, which the box is covering.
    ///
    /// Asserts on the selection index, not on the effect: a `j` that
    /// returned `Effect::None` and still moved the cursor is exactly the
    /// bug, and an effect-only assertion would pass through it.
    #[test]
    fn the_overlay_swallows_a_movement_key() {
        let mut app = healthy_app();
        let before = app.selected_index();
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.keymap_open());
        assert_eq!(app.update(Msg::Key(KeyPress::SelectDown)), Effect::None);
        assert_eq!(
            app.selected_index(),
            before,
            "j moved the selection behind the overlay"
        );
        assert!(app.keymap_open(), "j closed the overlay as well");
    }

    /// `h`, `?` and `esc` all close it. `?` reaches here as `Help` too, so
    /// this is one variant tested by the route the operator takes.
    #[test]
    fn the_overlay_closes_on_help_and_on_esc() {
        for closer in [KeyPress::Help, KeyPress::Escape] {
            let mut app = healthy_app();
            let _ = app.update(Msg::Key(KeyPress::Help));
            assert!(app.keymap_open());
            let _ = app.update(Msg::Key(closer));
            assert!(!app.keymap_open(), "{closer:?} did not close it");
        }
    }

    /// `q` still quits, the way it does with 1g's dialog up.
    #[test]
    fn quit_still_quits_with_the_overlay_up() {
        let mut app = healthy_app();
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert_eq!(app.update(Msg::Key(KeyPress::Quit)), Effect::Quit);
    }

    /// A close dialog owns the keyboard, overlay included: `h` with one up
    /// does not open a box over the question.
    #[test]
    fn the_close_dialog_keeps_the_keyboard_from_the_overlay() {
        let (mut app, _) = app_with_close_dialog();
        assert!(app.close_dialog().is_some());
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(
            !app.keymap_open(),
            "the overlay opened over an unanswered dialog"
        );
    }

    /// An armed confirm is cancelled by `h` and the overlay does not open,
    /// the same rule `any_other_key_cancels_an_action_armed_inside_the_pane`
    /// already states for every other key: a cancelling press is consumed.
    #[test]
    fn h_cancels_an_armed_confirm_instead_of_opening_the_overlay() {
        let mut app = healthy_app();
        let _ = app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(app.action().is_some());
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.action().is_none(), "h did not cancel the confirm");
        assert!(!app.keymap_open(), "h cancelled and also opened the overlay");
    }

    /// `h` typed into the filter box is a letter. Text mode is checked
    /// ahead of the overlay branch, so the overlay can never open from
    /// inside an open box.
    #[test]
    fn h_in_a_filter_box_types_a_letter() {
        let mut app = healthy_app();
        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        let _ = app.update(Msg::Key(KeyPress::TextChar('h')));
        assert!(!app.keymap_open());
    }

    /// It opens from every body, not only the dashboard, because the design
    /// draws one reference rather than a per-pane hint.
    #[test]
    fn the_overlay_opens_from_every_body() {
        for opener in [
            KeyPress::Edit,
            KeyPress::Settings,
            KeyPress::Secrets,
            KeyPress::Bleats,
            KeyPress::Confirm,
        ] {
            let mut app = healthy_app();
            let _ = app.update(Msg::Key(opener));
            let _ = app.update(Msg::Key(KeyPress::Help));
            assert!(
                app.keymap_open(),
                "the overlay did not open after {opener:?}"
            );
        }
    }

    /// And on a frozen dashboard, where a key list is most wanted.
    #[test]
    fn the_overlay_opens_when_the_link_is_gone() {
        let mut app = healthy_app();
        app.update(Msg::LinkLost);
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.keymap_open());
    }
```

Use whichever existing helpers `app.rs`'s test module has for a healthy app, a config pane, a close dialog and a lost link; grep for them rather than writing new ones. `healthy_app`, `app_with_close_dialog`, `selected_index` and the `Msg` for a lost link may all be named differently — find the real names and use those. `the_overlay_opens_from_every_body` needs a selected sheep before `Edit`/`Confirm` do anything, so the helper must supply one.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p shep --lib --all-features overlay`
Expected: FAIL — `keymap_open` does not exist.

- [ ] **Step 3: Add the state**

On `App`:

```rust
    /// Whether the keymap overlay is up.
    ///
    /// Not an [`InputMode`]: `map_key` has exactly two modes and an overlay
    /// that swallows keys is a reducer concern rather than a keyboard-edge
    /// one. A third mode would also have to answer what a letter means in
    /// it, and the answer is nothing.
    keymap_open: bool,
```

Initialise it `false` in every constructor. Then:

```rust
    /// Whether the keymap overlay is up, for `view` to draw.
    #[must_use]
    pub const fn keymap_open(&self) -> bool {
        self.keymap_open
    }

    /// Raises the overlay. Reached from every body's own `Help` arm, so
    /// each body's cancel and dialog guards have already run by the time
    /// this is called: `h` cancels an armed confirm and is consumed, and a
    /// close dialog never reaches its body's match at all.
    fn open_keymap(&mut self) -> Effect {
        self.keymap_open = true;
        Effect::None
    }

    /// The overlay's own keymap while it is up.
    ///
    /// Four keys do something and everything else is swallowed. Swallowing
    /// is the point: the box covers the flock table, so a `j` that reached
    /// the reducer would move a selection the operator cannot see.
    fn on_keymap_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::Help | KeyPress::Escape => {
                self.keymap_open = false;
                Effect::None
            }
            _ => Effect::None,
        }
    }
```

`on_keymap_key`'s `_` arm is the one wildcard this plan allows, and it is right: the overlay's answer to a new binding is to swallow it.

- [ ] **Step 4: Wire the branch and the eight arms**

In `on_key`, immediately after the text-mode branch and before the config pane's:

```rust
        // The overlay owns the keyboard while it is up, ahead of every pane
        // below. Behind text mode, not in front of it: `h` typed into an
        // open filter box is a letter, so the overlay can never be raised
        // from inside one.
        if self.keymap_open {
            return self.on_keymap_key(key);
        }
```

Then route `Help` to `open_keymap` in seven handlers, each getting its own arm out of whatever inert group it sits in today:

| handler | today | becomes |
|---|---|---|
| `on_key` (dashboard) | `KeyPress::Help => Effect::None` | `KeyPress::Help => self.open_keymap()` |
| `on_secrets_key` | in an inert group | its own arm calling `self.open_keymap()` |
| `on_sheep_pane_key` | in an inert group | the same |
| `on_bleats_key` | in an inert group | the same |
| `on_settings_key` | in an inert group | the same |
| `on_pane_key` | `KeyPress::Help => {}` from Task 2 | the same |
| `on_list_key` | in an inert group | the same |
| `on_close_dialog_key` | in an inert group | **unchanged** |

`on_close_dialog_key` keeps `Help` inert, and its group's comment gains a clause saying so:

```rust
            // `Help` among them: the dialog owns the keyboard until it is
            // answered, and the keymap overlay is not an exception to that.
```

- [ ] **Step 5: Run the suite**

Run: `cargo test -p shep --lib --all-features -- --skip ::slow::`
Expected: PASS. The three existing tests that pressed `Help` to toggle field help were deleted in Task 2; if any survive, they fail here and should be replaced by the tests above rather than repaired.

- [ ] **Step 6: Mutate each test and watch it fail**

- `the_overlay_swallows_a_movement_key` — change `on_keymap_key`'s `_` arm to fall through to the dashboard match.
- `the_overlay_closes_on_help_and_on_esc` — drop `Escape` from the closing arm.
- `quit_still_quits_with_the_overlay_up` — move `Quit` into the `_` arm.
- `the_close_dialog_keeps_the_keyboard_from_the_overlay` — route `Help` to `open_keymap` in `on_close_dialog_key`.
- `h_cancels_an_armed_confirm_instead_of_opening_the_overlay` — move the `keymap_open` branch above the armed-confirm check in `on_key`.
- `h_in_a_filter_box_types_a_letter` — move the `keymap_open` branch above the text-mode branch.
- `the_overlay_opens_from_every_body` — revert one handler's arm to inert, and check the failure message names that body.
- `the_overlay_opens_when_the_link_is_gone` — guard `open_keymap` on `Link::Live`.

- [ ] **Step 7: Commit**

```bash
git add crates/shep-cli/src/lookout/app.rs
git commit -m "feat(lookout): raise and dismiss the keymap overlay"
```

---

## Task 6: Draw the boxed overlay

**Files:**
- Create: `crates/shep-cli/src/lookout/view/keymap.rs`
- Modify: `crates/shep-cli/src/lookout/view/mod.rs` (`mod keymap;` and the draw hook)
- Modify: `crates/shep-cli/src/lookout/view/overlay.rs` (the two new glyphs join the width test)

**Interfaces:**
- Consumes: `overlay::{draw_boxed, boxed_height, is_boxed, mute, blank_row}`, `keymap::{rows, Group, Binding, ENTRY_ROWS, KEY_CELL, TEXT_CELL}`, `App::{keymap_open, control, link, palette}`.
- Produces:
  ```rust
  pub(super) const INTERIOR: u16 = 126;
  pub(super) fn draw(app: &App, area: Rect, buffer: &mut Buffer);
  pub(super) fn lines(app: &App, interior: u16) -> Vec<Line<'static>>;
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::lookout::app::{KeyPress, Msg};
    use crate::lookout::frames::render_text;
    use crate::output::width::visible_width;

    /// The arithmetic, asserted rather than commented.
    ///
    ///   interior 126 = 4 columns x 30 + 3 gutters x 2
    ///   column    30 = 12 key + 1 gap + 17 text
    ///   box      128 = 126 interior + 1 border each side
    ///   floor    130 = 128 + 1 margin each side
    #[test]
    fn the_columns_sum_to_the_interior() {
        assert_eq!(COLUMN, KEY_CELL + GAP + TEXT_CELL);
        assert_eq!(COLUMN * COLUMN_COUNT + GUTTER * (COLUMN_COUNT - 1), INTERIOR);
        assert_eq!(COLUMN, 30);
        assert_eq!(INTERIOR, 126);
    }

    /// Every row of the box is exactly the interior wide, so no column
    /// drifts and the right border lands in one place on every row.
    #[test]
    fn every_row_is_the_interior_wide() {
        let app = app_with_overlay();
        for line in lines(&app, INTERIOR) {
            let text: String = line.spans.iter().map(|span| span.content.as_ref()).collect();
            assert!(
                visible_width(&text) <= usize::from(INTERIOR),
                "a row is {} cells against {INTERIOR}: {text:?}",
                visible_width(&text)
            );
        }
    }

    /// Nineteen rows: a border pair, a heading, twelve entries, a blank,
    /// the gate line and two closing lines.
    #[test]
    fn the_box_is_nineteen_rows() {
        let app = app_with_overlay();
        assert_eq!(overlay::boxed_height(&lines(&app, INTERIOR)), 19);
    }

    /// All four headings draw, each at its own column start. Counting the
    /// starts rather than looking for the words is what catches a column
    /// that drew at the wrong offset.
    #[test]
    fn the_four_headings_sit_at_their_column_starts() {
        let app = app_with_overlay();
        let rendered = render_overlay(&app, 160, 48);
        let heading_row = rendered
            .lines()
            .find(|row| row.contains("MOVING"))
            .expect("no heading row");
        for (index, group) in Group::DRAWN.iter().enumerate() {
            let offset = usize::from(COLUMN + GUTTER) * index;
            // +1 for the box's own left border cell, and the box starts at
            // (160 - 128) / 2 = 16.
            let start = 16 + 1 + offset;
            assert!(
                heading_row[start..].starts_with(group.heading()),
                "{} is not at column {start}: {heading_row:?}",
                group.heading()
            );
        }
    }

    /// Every row `keymap::rows` produces reaches the screen. The point of
    /// deriving them is lost if the drawer silently drops the tail of a
    /// column.
    ///
    /// `Group::Closing` is checked by its caption rather than its `does`,
    /// which is the four letters `quit` and would be satisfied by the
    /// closing line's own `quits lookout` whatever the drawer did with the
    /// row.
    #[test]
    fn every_derived_row_is_drawn() {
        let app = app_with_overlay();
        let rendered = render_overlay(&app, 160, 48);
        for row in crate::lookout::keymap::rows() {
            let wanted = if row.group == Group::Closing {
                row.keys
            } else {
                row.does
            };
            assert!(
                rendered.contains(wanted),
                "`{wanted}` ({}) never reached the screen",
                row.keys
            );
        }
    }

    /// The gate line's three states. Each asserts the other two strings are
    /// absent, so a line carrying both answers fails.
    #[test]
    fn the_gate_line_names_the_gate_and_then_the_link() {
        let allowed = render_overlay(&app_with_overlay(), 160, 48);
        assert!(allowed.contains("control enabled"), "{allowed}");
        assert!(!allowed.contains("read-only"));
        assert!(!allowed.contains("the link is down"));

        let read_only = render_overlay(&read_only_app_with_overlay(), 160, 48);
        assert!(read_only.contains("read-only"), "{read_only}");
        assert!(!read_only.contains("control enabled"));

        let frozen = render_overlay(&frozen_app_with_overlay(), 160, 48);
        assert!(frozen.contains("the link is down"), "{frozen}");
        assert!(
            !frozen.contains("control enabled") && !frozen.contains("read-only"),
            "a frozen overlay still names the control gate: {frozen}"
        );
    }

    /// The sheep, and only here. The design allows it exactly once.
    #[test]
    fn the_sheep_draws_in_the_doing_column() {
        let app = app_with_overlay();
        let rendered = render_overlay(&app, 160, 48);
        assert!(rendered.contains(SHEEP[0]), "no sheep: {rendered}");
        // The rightmost column starts at 16 + 1 + 3 * (30 + 2) = 113.
        let row = rendered
            .lines()
            .find(|row| row.contains(SHEEP[0]))
            .expect("no sheep row");
        assert!(
            row.find(SHEEP[0]).is_some_and(|at| at >= 113),
            "the sheep is left of the DOING column: {row:?}"
        );
    }

    /// The body behind is dimmed, not painted over: the frame draws the
    /// overlay as a question about what is underneath.
    #[test]
    fn the_body_behind_is_dimmed() {
        // Assert on a cell outside the box's own columns, so what is read
        // is the muted table rather than the overlay's own ground.
        // ... the assertion follows `view/pane.rs`'s own
        // `the_pane_behind_the_box_is_muted`, whichever form that takes.
    }

    /// Renders one overlay and returns the screen as text.
    fn render_overlay(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| super::super::draw(app, frame))
            .expect("draw");
        render_text(terminal.backend().buffer())
    }

    /// A healthy dashboard with the overlay up.
    fn app_with_overlay() -> App {
        let mut app = /* the module's own healthy fixture */;
        let _ = app.update(Msg::Key(KeyPress::Help));
        app
    }
}
```

Build `read_only_app_with_overlay` and `frozen_app_with_overlay` on the same fixture, setting `Control::ReadOnly` and a lost link the way `frames.rs` and `view/status.rs`'s tests already do. Grep for how those two states are constructed in tests rather than inventing a route.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p shep --lib --all-features view::keymap`
Expected: FAIL — the module does not resolve.

- [ ] **Step 3: Write the drawer**

```rust
//! The keymap overlay, frame 1k: every key lookout binds, grouped by what
//! it does, boxed over the dimmed body.
//!
//! The rows come from [`crate::lookout::keymap::rows`], which derives them
//! by running `map_key`. Nothing here decides which keys exist.

/// Cells inside the border.
///
/// ```text
/// interior 126 = 4 columns x 30 + 3 gutters x 2
/// column    30 = 12 key (KEY_CELL) + 1 gap (GAP) + 17 text (TEXT_CELL)
/// box      128 = 126 interior + 1 border cell each side
/// floor    130 = 128 + 1 margin cell each side  (overlay::floor_for)
/// ```
///
/// Written out because a frame pinned one column short of its own column
/// set drops the thing it exists to show, which happened once already in
/// this bundle. `the_columns_sum_to_the_interior` asserts every line of
/// the block above.
pub(super) const INTERIOR: u16 = 126;

const COLUMN_COUNT: u16 = 4;
const GAP: u16 = 1;
const GUTTER: u16 = 2;
const COLUMN: u16 = KEY_CELL + GAP + TEXT_CELL;

/// The one decoration in the whole TUI that carries no information, drawn
/// in the DOING column's own thirty cells at entry rows five through eight.
///
/// The design centres it in a 38-cell cell. The rightmost 38 cells of a
/// 126-cell interior start at 88, inside CHANGING's column (64..93), which
/// runs to entry row ten and leaves three free rows there rather than four.
/// The art is ten columns wide, so DOING's column at 96..125 holds it with
/// room. Allowed exactly once, here.
const SHEEP: [&str; 4] = [
    "  \u{259f}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2599}",
    " \u{259f}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2599}",
    " \u{259c}\u{2588}\u{2588}\u{2580} \u{2580}\u{2588}\u{2588}\u{259b}",
    "   \u{2580}\u{2598} \u{2580}\u{2598}",
];
```

`lines(app, interior)` builds, in order: the heading row, `ENTRY_ROWS` entry rows, a blank, the gate line, and the two closing lines. Each entry row is four cells of `KEY_CELL` + `GAP` + `TEXT_CELL` joined by `GUTTER` spaces, with a group's exhausted rows drawn as `COLUMN` spaces and the sheep overwriting DOING's cell at entry rows 5 through 8.

Styles: a heading in `palette.band(group.role())`; a key caption in `palette.attention()`; a `does` sentence in `palette.ground()`; the two closing lines in `palette.muted()`. The gate line's own three forms:

```rust
/// What the three destructive keys cost, and whether they can act at all.
///
/// One full-width line rather than printed beside the three keys as the
/// frame draws it: `each one arms, ↵ confirms, 10s to answer` plus the
/// control label is 57 characters against a 17-cell text field, and split
/// over three stacked cells inside DOING it reads worse than stated once.
///
/// `Link::Lost` outranks the control gate, because `x`/`R`/`L` then refuse
/// for a reason `Control` does not carry, and a line saying `control
/// enabled` beside a dead link is false.
fn gate_line(app: &App, palette: Palette) -> Line<'static> {
    if matches!(app.link(), Link::Lost { .. }) {
        return /* "the three above are refused  ·  █ the link is down · nothing acts" */;
    }
    /* "the three above each arm a confirm  ·  ↵ confirms  ·  10s to answer  ·  █ <label>" */
}
```

`<label>` is the existing `"control enabled"` / `"read-only"` pair from `view/status.rs`. Make them `pub(super)` consts there and import them rather than retyping either string.

`draw(app, area, buffer)` calls `overlay::draw_boxed(&lines(app, INTERIOR), INTERIOR, palette, area, buffer)` when `overlay::is_boxed(area.width, INTERIOR)` and the height holds `overlay::boxed_height`. Task 7 adds the other branch; until then, return without drawing and leave a `// Task 7:` line.

- [ ] **Step 4: Hook it into `view::draw`**

At the end of `view::draw`, after every pane and the chrome:

```rust
    // Last, over everything: the overlay covers whatever body is showing,
    // unlike 1g's dialog, which only ever covers the config pane and so
    // draws from inside `view::pane::draw_pane`.
    if app.keymap_open() {
        overlay::mute(buffer, area, palette);
        keymap::draw(app, area, buffer);
    }
```

- [ ] **Step 5: Add the two glyphs to the width check**

In `overlay.rs`, extend `the_border_vocabulary_is_the_one_that_was_checked`'s array with `'▘'` and `'⌫'`, and extend its doc comment:

```rust
    /// ... `▘` (U+2598) joined for the keymap overlay's sheep and `⌫`
    /// (U+232B) for its text-mode row: both are below the design README's
    /// U+2600 ceiling but absent from the design's own vocabulary table, so
    /// neither inherits this test's answer without being in it.
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p shep --lib --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 7: Look at it**

Invoke the `tui-screen-capture` skill and capture the real binary at 160×48 with `h` pressed. Reading source and diffs is what fails to catch a TUI defect. Note that `--keys` land near the end of `--seconds`, so an early capture looks like nothing happened; give the capture enough seconds for the keypress to land.

Compare against the spec's own sketch: four headings, twelve entries in LOOKING, the sheep bottom-right in DOING, the three closing lines.

- [ ] **Step 8: Mutate each test and watch it fail**

- `the_columns_sum_to_the_interior` — set `TEXT_CELL` to 16.
- `every_row_is_the_interior_wide` — drop one gutter from the join.
- `the_box_is_nineteen_rows` — drop the blank row.
- `the_four_headings_sit_at_their_column_starts` — swap LOOKING and CHANGING.
- `every_derived_row_is_drawn` — take `ENTRY_ROWS - 1` entries per column.
- `the_gate_line_names_the_gate_and_then_the_link` — check `Control` before `Link` in `gate_line`.
- `the_sheep_draws_in_the_doing_column` — draw it in CHANGING's column.

- [ ] **Step 9: Commit**

```bash
git add crates/shep-cli/src/lookout/view/keymap.rs crates/shep-cli/src/lookout/view/mod.rs crates/shep-cli/src/lookout/view/overlay.rs
git commit -m "feat(lookout): draw the keymap overlay over the dimmed body"
```

---

## Task 7: The borderless ladder, and what it sheds

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/keymap.rs`

**Interfaces:**
- Consumes: Task 6's `lines`, `COLUMN`, `GUTTER`, `COLUMN_COUNT`.
- Produces: `const fn columns_for(width: u16) -> u16`, and `draw`'s second branch.

- [ ] **Step 1: Write the failing tests**

```rust
    /// `n` columns take `32n - 2` cells, so `n = (width + 2) / 32`, clamped
    /// to one through four.
    ///
    /// Each boundary is asserted from both sides. The 126..129 band is the
    /// only place the full four-column layout draws unboxed, and it is four
    /// widths wide: the box needs two cells the columns themselves do not.
    #[test]
    fn the_column_ladder_has_a_boundary_on_each_side() {
        assert_eq!(columns_for(126), 4);
        assert_eq!(columns_for(125), 3);
        assert_eq!(columns_for(94), 3);
        assert_eq!(columns_for(93), 2);
        assert_eq!(columns_for(62), 2);
        assert_eq!(columns_for(61), 1);
        assert_eq!(columns_for(MIN_TERM_WIDTH), 1);
    }

    /// One column under the floor: no border, and still four columns.
    #[test]
    fn one_column_under_the_floor_keeps_four_columns_and_loses_the_border() {
        let app = app_with_overlay();
        let boxed = render_overlay(&app, 130, 48);
        let bare = render_overlay(&app, 128, 48);
        assert!(boxed.contains('\u{259b}'), "130 must be boxed: {boxed}");
        assert!(!bare.contains('\u{259b}'), "128 must not be: {bare}");
        for group in Group::DRAWN {
            assert!(
                bare.contains(group.heading()),
                "{} is missing at 128 columns",
                group.heading()
            );
        }
    }

    /// Three columns at 100, with DOING on a bank of its own below.
    #[test]
    fn three_columns_put_doing_on_its_own_bank() {
        let rendered = render_overlay(&app_with_overlay(), 100, 48);
        let heading_rows: Vec<&str> = rendered
            .lines()
            .filter(|row| Group::DRAWN.iter().any(|g| row.contains(g.heading())))
            .collect();
        assert_eq!(heading_rows.len(), 2, "{heading_rows:?}");
        assert!(heading_rows[0].contains("MOVING") && heading_rows[0].contains("CHANGING"));
        assert!(heading_rows[1].contains("DOING"));
    }

    /// The heights, from the top of the ladder to the refusal.
    ///
    ///   19  boxed, everything
    ///   18  borderless (a box cannot shed its border pair)
    ///   17  borderless, everything
    ///   16  the NO_COLOR line and the sheep go
    ///   15  the blank separator goes
    ///   14  the gate line folds onto the closing line
    ///   13  the floor: the heading and twelve entry rows
    ///   12  refuse
    #[test]
    fn the_height_ladder_shows_its_boundaries() {
        assert_eq!(rows_for_height(19), Shed::Boxed);
        assert_eq!(rows_for_height(18), Shed::Nothing);
        assert_eq!(rows_for_height(17), Shed::Nothing);
        assert_eq!(rows_for_height(16), Shed::Decoration);
        assert_eq!(rows_for_height(15), Shed::Blank);
        assert_eq!(rows_for_height(14), Shed::Gate);
        assert_eq!(rows_for_height(13), Shed::Gate);
        assert_eq!(rows_for_height(12), Shed::Refuse);
    }

    /// At 16 rows the sheep and the colour sentence are gone and every key
    /// row is still there.
    ///
    /// 16, not 22: the box needs 19 rows and 22 holds it whole, so nothing
    /// sheds at 22 and this test would have passed on an unshed form. The
    /// sheep also frees no rows by itself, since it sits inside entry rows
    /// LOOKING needs anyway — it goes with the NO_COLOR line because a
    /// decoration beside a trimmed list is wrong, not because it buys room.
    #[test]
    fn a_short_terminal_sheds_the_decoration_and_keeps_the_keys() {
        let app = app_with_overlay();
        let rendered = render_overlay(&app, 160, 16);
        assert!(!rendered.contains(SHEEP[0]), "the sheep survived: {rendered}");
        assert!(
            !rendered.contains("decoration only"),
            "the NO_COLOR line survived: {rendered}"
        );
        for row in crate::lookout::keymap::rows() {
            if row.group == Group::Closing {
                continue;
            }
            assert!(
                rendered.contains(row.does),
                "`{}` was shed with the decoration",
                row.does
            );
        }
    }

    /// Below the key rows themselves, the overlay says so rather than
    /// drawing a partial list. A key list missing rows silently is worse
    /// than one that refuses.
    ///
    /// 12 rows: one under the floor of 13, which is the heading plus the
    /// twelve entries. `MIN_HEIGHT` is 6, so the dashboard behind still
    /// draws at this height and the refusal is the overlay's own.
    #[test]
    fn too_short_refuses_instead_of_clipping() {
        let rendered = render_overlay(&app_with_overlay(), 160, 12);
        assert!(
            rendered.contains("the keymap needs"),
            "no refusal at 12 rows: {rendered}"
        );
        assert!(
            !rendered.contains("MOVING"),
            "a partial list drew anyway: {rendered}"
        );
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p shep --lib --all-features view::keymap`
Expected: FAIL — `columns_for` does not exist and `draw` has no second branch.

- [ ] **Step 3: Implement the ladder**

```rust
/// How many columns fit `width` cells with no border.
///
/// `n` columns take `n * COLUMN + (n - 1) * GUTTER`, which is `32n - 2`, so
/// the widest `n` that fits is `(width + 2) / 32`. Clamped to one at the
/// bottom, since `MIN_TERM_WIDTH` is 33 and one column is 30, and to
/// [`COLUMN_COUNT`] at the top, since only four groups draw as columns.
const fn columns_for(width: u16) -> u16 {
    let fits = (width + GUTTER) / (COLUMN + GUTTER);
    if fits < 1 {
        1
    } else if fits > COLUMN_COUNT {
        COLUMN_COUNT
    } else {
        fits
    }
}
```

Groups wrap into banks of `columns_for(width)`, banks separated by a blank row.

Then the height ladder, as an enum rather than a chain of `if`s, so the test above can assert each boundary without rendering:

```rust
/// What a form of `height` rows has to give up.
///
/// The sheep is not a step of its own: it sits at entry rows five through
/// eight of the DOING column, and those rows exist because LOOKING has
/// twelve entries, so removing it frees nothing. It goes with
/// [`Shed::Decoration`]'s `NO_COLOR` line because a decoration beside a
/// list that has already lost text is worse than no decoration.
///
/// ```text
/// 19  Boxed       everything, border pair included
/// 18  Nothing     borderless: a box cannot shed its border pair
/// 17  Nothing     borderless, everything
/// 16  Decoration  the NO_COLOR line and the sheep
/// 15  Blank       and the blank separator
/// 14  Gate        and the gate line, folded onto the closing line
/// 13  Gate        the floor: the heading and twelve entry rows
/// 12  Refuse
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shed {
    Boxed,
    Nothing,
    Decoration,
    Blank,
    Gate,
    Refuse,
}

const fn rows_for_height(height: u16) -> Shed { /* the ladder above */ }
```

`Shed::Refuse` draws one line naming the height the key rows need and stops.

- [ ] **Step 4: Run the tests and look at it**

Run: `cargo test -p shep --lib --all-features -- --skip ::slow::`
Expected: PASS.

Then capture the real binary at 100×48 and at 70×48 with `tui-screen-capture`, and confirm the banks stack rather than overlapping.

- [ ] **Step 5: Mutate each test and watch it fail**

- `the_column_ladder_has_a_boundary_on_each_side` — change the divisor to `COLUMN`.
- `one_column_under_the_floor_keeps_four_columns_and_loses_the_border` — set the floor to `INTERIOR + 6`, which is the 132 the design README first gave, and watch 130 stop being boxed.
- `three_columns_put_doing_on_its_own_bank` — put all four groups on one bank regardless of width.
- `the_height_ladder_shows_its_boundaries` — move the `Decoration` boundary to 17.
- `a_short_terminal_sheds_the_decoration_and_keeps_the_keys` — shed an entry row instead of the closing line.
- `too_short_refuses_instead_of_clipping` — draw what fits instead of refusing.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout/view/keymap.rs
git commit -m "feat(lookout): drop the keymap's columns and border as the terminal narrows"
```

---

## Task 8: `FROZEN_HINT` names the key

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/status.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `CONTROL_ENABLED` and `READ_ONLY` as `pub(super)` consts, if Task 6 has not already made them so.

- [ ] **Step 1: Write the failing test**

```rust
    /// The frozen hint names `h`, because the overlay opens with the link
    /// down and the sentence's last clause claims every other key refuses.
    ///
    /// Asserts the clause is still there as well as the new key: the point
    /// is that the sentence stays true, not that a key got appended to it.
    #[test]
    fn the_frozen_hint_names_the_keymap_it_no_longer_refuses() {
        assert!(FROZEN_HINT.contains("h keymap"), "{FROZEN_HINT}");
        assert!(
            FROZEN_HINT.contains("nothing you press can reach the shepherd"),
            "{FROZEN_HINT}"
        );
        assert!(FROZEN_HINT.contains("q quit") && FROZEN_HINT.contains("j/k g/G move"));
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p shep --lib --all-features frozen_hint`
Expected: FAIL — `h keymap` is absent.

- [ ] **Step 3: Amend the hint**

```rust
/// The key hint once the link is [`Link::Lost`].
///
/// Three keys, because three still do something: `q` leaves, `j`/`k` move a
/// cursor over values that are already history, and `h` opens the keymap,
/// which is read-only and wanted most on the screen where nothing else
/// works. `r` is not among them, whatever the design's own copy says: it is
/// refused like the rest (`App::on_key`), and `super::super::link::run_link`
/// has already returned by the time a freeze lands, so no task survives to
/// answer a redial. The last clause is the whole rest of the keymap, said
/// once rather than discovered a keypress at a time — which is why `h` had
/// to be named here the moment it stopped being refused.
const FROZEN_HINT: &str = "q quit   h keymap   j/k g/G move   \
     nothing you press can reach the shepherd";
```

Check the width tests in this module: the hint got longer, and `status.rs` has tests pinning where it truncates. Re-run them and update whichever numeric threshold they assert, with the new number in the comment rather than the old one edited around.

- [ ] **Step 4: Run the suite**

Run: `cargo test -p shep --lib --all-features -- --skip ::slow::`
Expected: PASS. The `Frozen` scene's snapshot changes; confirm the new hint is what changed before accepting it.

- [ ] **Step 5: Mutate and watch it fail**

Drop `h keymap` and confirm the failure. Then drop only the refusal clause and confirm the test still fails, which proves the second assertion is doing work rather than riding on the first.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout/view/status.rs crates/shep-cli/src/lookout/snapshots
git commit -m "fix(lookout): name the keymap in the frozen hint that claims to refuse it"
```

---

## Task 9: Gallery scenes

**Files:**
- Modify: `crates/shep-cli/src/lookout/frames.rs`

**Interfaces:**
- Consumes: `App::keymap_open`.
- Produces: eight `Scene` variants.

- [ ] **Step 1: Add the variants, labels, captions, sizes and control**

| variant | label | size | control | what it exists to show |
|---|---|---|---|---|
| `Keymap` | `keymap` | 160 × 48 | `Allowed` | the design target: boxed, four columns, the sheep, all 35 rows |
| `KeymapFloor` | `keymap_floor` | 130 × 48 | `Allowed` | the narrowest box: border intact, one margin cell each side |
| `KeymapBorderlessWide` | `keymap_borderless_wide` | 128 × 48 | `Allowed` | two columns under that: four columns, no border |
| `KeymapNarrow` | `keymap_narrow` | 100 × 48 | `Allowed` | three columns, DOING on its own bank |
| `KeymapTwoColumn` | `keymap_two_column` | 70 × 48 | `Allowed` | two columns, two banks |
| `KeymapFrozen` | `keymap_frozen` | 160 × 48 | `Allowed` | the gate line naming the dead link |
| `KeymapReadOnly` | `keymap_read_only` | 160 × 48 | `ReadOnly` | `█ read-only` in the gate line |
| `KeymapShort` | `keymap_short` | 160 × 16 | `Allowed` | the sheep and the colour sentence shed, every key row intact |

`Scene::control`'s match is `Self::Refused => ReadOnly, _ => Allowed` today, so `KeymapReadOnly` joins the first arm.

Each `caption` says what the scene shows, in the voice the existing captions use. `KeymapFloor` and `KeymapBorderlessWide` each name their own number and why it is that number: 130 is `126 + 4`, and 128 is the widest four-column form without a border.

- [ ] **Step 2: Raise the overlay in the builder**

Every `Keymap*` scene uses the healthy flock fixture and then presses `Help`. Add them to whichever existing arm builds a plain healthy dashboard, then after the poll:

```rust
    if matches!(
        scene,
        Scene::Keymap
            | Scene::KeymapFloor
            | Scene::KeymapBorderlessWide
            | Scene::KeymapNarrow
            | Scene::KeymapTwoColumn
            | Scene::KeymapFrozen
            | Scene::KeymapReadOnly
            | Scene::KeymapShort
    ) {
        let _ = app.update(Msg::Key(KeyPress::Help));
    }
```

`KeymapFrozen` also needs the link lost, the way `Scene::Frozen` does it.

- [ ] **Step 3: Write the assertions, not just the snapshots**

Each scene gets a test that checks the thing it exists to show. A scene test that only looks for the word `MOVING` passes at every width in the table, which is the failure this plan is trying not to repeat:

```rust
    /// Each width scene draws the column count it exists to show, counted
    /// off the heading row's own occupied starts rather than inferred from
    /// a word being present.
    #[test]
    fn each_keymap_scene_draws_its_own_column_count() {
        for (scene, wanted) in [
            (Scene::Keymap, 4),
            (Scene::KeymapFloor, 4),
            (Scene::KeymapBorderlessWide, 4),
            (Scene::KeymapNarrow, 3),
            (Scene::KeymapTwoColumn, 2),
        ] {
            let rendered = render(scene);
            let first_bank = rendered
                .lines()
                .find(|row| row.contains("MOVING"))
                .expect("no heading row");
            let drawn = Group::DRAWN
                .iter()
                .filter(|group| first_bank.contains(group.heading()))
                .count();
            assert_eq!(drawn, wanted, "{}: {first_bank:?}", scene.label());
        }
    }
```

- [ ] **Step 4: Generate and review the snapshots**

Run: `cargo test -p shep --lib --all-features -- --skip ::slow::`
Then review every new `.snap.new` by eye before accepting. For each: count the columns, find the sheep, read the gate line. A snapshot accepted without being read pins whatever the bug produced.

- [ ] **Step 5: Commit**

```bash
git add crates/shep-cli/src/lookout/frames.rs crates/shep-cli/src/lookout/snapshots
git commit -m "test(lookout): eight gallery scenes for the keymap overlay"
```

---

## Task 10: Docs, and the gate

**Files:**
- Modify: `docs/lookout/design-files/README.md` (lines near 317 and 332)
- Modify: `docs/lookout/README.md`
- Modify: `web/src/pages/docs/lookout.astro`
- Regenerate: `web/src/data/cli-reference.generated.txt`

- [x] **Step 1: Do NOT correct the rulings**

Struck, not done. `rulings.md` never said 132 and gives 1k no width, so a
section there announcing "this file said 132" would be a false correction in
the authority document. The 132 was in `design-files/README.md:332`, which
Step 2 corrects, and that line now carries the arithmetic:

```text
(130, not the 132 this line first gave: the floor is the interior plus a
border cell and a margin cell each side, which is 86 + 4 for 1g and
126 + 4 for 1k.)
```

- [ ] **Step 2: Correct the design README**

Line 317's navigation sentence: `g` → `S`, `l` → `b`. Line 332's `132 columns` → `130 columns`. Leave every other claim alone; this plan is not a general audit of that file.

- [ ] **Step 3: Write the operator docs**

`docs/lookout/README.md` gets the overlay: `h` or `?`, the four groups, that it swallows every other key, that `q` still quits, and that it opens with the link down.

`web/src/pages/docs/lookout.astro` gets a keymap section. Its existing description of `h` as the config pane's field-help key (two places, near lines 1034 and 1089) is now wrong twice over — the key moved and the help is automatic — and both need rewriting rather than deleting: the field help still exists, it just has no key.

**Both prose changes go through the `humanizer` skill and then `rin-voice` before they are final.** `web/` is published and is part of the public surface. The pages' existing prose is the voice sample to match.

- [ ] **Step 4: Regenerate the CLI reference**

```bash
cargo build --release
```
```bash
env -u SHEP_HOME -u SHEP_STYLE -u NO_COLOR ./web/scripts/generate-cli-reference.sh
```
Then `git diff`. No flag changed, so the expectation is no diff; the run is what confirms it rather than the assumption.

- [ ] **Step 5: Build and check the site**

```bash
cd web && npx astro build
```
```bash
cd web && npx astro check
```

Both. `check` is what catches a component given a prop it does not have; `build` is green on exactly that bug.

- [ ] **Step 6: Run the full gate, one command at a time**

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

One cargo command per invocation: the workspace shares one target-dir build lock. Capture `$?` directly rather than through a pipe.

- [ ] **Step 7: Cross-compile checks**

```bash
cargo check -p shep-daemon --all-targets --all-features --target x86_64-unknown-linux-gnu
```
```bash
cargo check --workspace --all-targets --all-features --target x86_64-pc-windows-gnu
```

Nothing in this plan is platform-conditional, so both are expected clean. They run because the local gate does not otherwise cover either.

- [ ] **Step 8: Commit**

```bash
git add docs web
git commit -m "docs(lookout): document the keymap overlay, and correct the floor"
```

---

## Self-review against the spec

**Spec coverage.** Every section maps to a task: the bundle check and the drift table to Task 10's doc corrections; `h`'s retirement to Task 2; the two modes to Task 5; the derivation to Task 3; layout and its arithmetic to Task 6; the rows to Task 3 and Task 6; the global-reference decision to Task 6's `every_derived_row_is_drawn`; the borderless ladder and shedding to Task 7; the frozen dashboard to Tasks 5, 6 and 8; the extracted box machinery to Task 1; the two new glyphs to Task 6's Step 5; testing to every task's mutation step; docs to Task 10.

**Type consistency.** `floor_for`, `is_boxed`, `boxed_height`, `draw_boxed`, `blank_row` and `mute` are named identically in Task 1's Produces block and at every call site in Tasks 6 and 7. `KEY_CELL` and `TEXT_CELL` are declared in `keymap.rs` (Task 3) and imported by `view/keymap.rs` (Task 6). `ENTRY_ROWS` likewise. `top_line` becomes `top_lines` returning a `Vec` in Task 2, and nothing later calls the old name.

**Known gaps, deliberately left to the implementer.** Three places name a fixture or helper by a guessed name and tell the implementer to grep for the real one: Task 2's Step 6, Task 5's Step 1, and Task 6's Step 1. A plan that invents fixture names produces an implementer who writes new fixtures beside existing ones. Task 6's `the_body_behind_is_dimmed` is left as a sketch for the same reason: `view/pane.rs` already has the equivalent assertion for 1g and it should be followed rather than re-derived.
