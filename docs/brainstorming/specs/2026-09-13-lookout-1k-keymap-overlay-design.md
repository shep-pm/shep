# Lookout pane 1k: the keymap overlay

**Status:** approved 2026-09-13.

The last frame of the bundle. A bordered box over the dimmed dashboard listing
every key lookout binds, grouped by what the key does.

`docs/lookout/design-files/rulings.md` is the authority on which of the bundle's
claims survive contact with shipped shep. Where a frame and a ruling disagree,
the ruling wins. The rulings list 1k among the frames that go ahead as drawn,
and set its width floor at 132 columns, which is the one number in them this spec
corrects rather than follows (see Layout).

## The bundle is complete, and this frame is last on purpose

1a, 1d, 1e, 1g, 1h, 1i, 1j and 1l are all on `main` as of `a6fef8bf`. Verified
against the code rather than the plans, whose checkboxes read 0 of N for all
seven shipped plans:

| frame | on `main` as |
|---|---|
| 1a | `view/flock.rs`'s `CPU 20s` / `MEM/CEIL` / `CFG` / `SMIT` columns, `view/host.rs`'s gauges |
| 1d | `pane_sheep.rs`, `view/sheep.rs` (#202) |
| 1e | `view/settings.rs`, `pane.rs`'s `ConfigPane` (#226) |
| 1g | `Scene::CloseDialog{,Floor,Narrow,Parked}`, `view/pane.rs`'s box machinery |
| 1h | `secrets.rs`, `view/secrets.rs` (#201) |
| 1i | `view/bleats_full.rs` |
| 1j | `KeyPress::FoldView`, `Scene::Folds` |
| 1l | `Scene::Frozen` (#197) |

This frame draws the keys the others claimed, so building it earlier would have
drawn a keymap that was stale on merge. Nine of the bindings it lists were added
by 1i and 1j, and several of those were picked rather than taken from the design,
because the design named no key for them.

## The document had drifted, and this is where it gets settled

`docs/lookout/design-files/README.md:317` makes three claims about keys. Two are
stale and one describes something that does not exist.

| the line says | `input.rs` does | which moved |
|---|---|---|
| `g` opens secrets | `S` is `KeyPress::Secrets`; `g` is `KeyPress::SelectFirst` | the code, deliberately. `docs/brainstorming/specs/2026-09-08-lookout-1h-secrets-design.md:224` records the pick, and `input.rs`'s `capital_s_opens_the_secrets_pane_and_lower_s_still_opens_settings` asserts `g` stays `SelectFirst` with the reason in the message. |
| `l` opens the full-screen feed | `b` is `KeyPress::Bleats`; `l` is unbound | the code. `KeyPress::Bleats`' own doc names `b`. |
| `h` or `?` toggles the keymap | `h` is `KeyPress::Help`; `?` is unbound | neither. This frame's work, not drift. |

The design also names no key at all for `m` (`LevelCycle` — 1i's own status-bar
list omits it, and the variant's doc says it was picked because the design named
none), for `1`–`8` (`Group`), or for the `Home`/`End`/`Left`/`Right` aliases.
`c` is named in the 1g section but missing from the navigation line.

**This spec corrects the README line rather than drawing what it says.** The two
stale keys are corrected to `S` and `b`; the third becomes true when this frame
lands.

## `h` was already spent, and the panel had made it redundant

`KeyPress::Help` exists and `h` is bound to it. On the dashboard `on_key`
(`app.rs:3605`) answers `Effect::None`; inside the config pane `on_pane_key`
(`app.rs:4946`) calls `ConfigPane::toggle_help`, and `view/pane.rs:399`'s
`top_line` then draws the selected field's `help` string on one line under the
title.

The explanation panel 1e shipped draws the same string. `panel_for_field`'s
region 2 (`view/pane.rs:2020`) wraps `field.help` unconditionally, and
`panel_lines` follows the cursor. So at any width where the panel draws, `h`
duplicates it.

The panel does not draw at every width. `panel_width` (`view/pane.rs:135`):

```
panel = clamp(width * 45 / 100, PANEL_MIN 50, PANEL_MAX 72)
draws iff  width - panel >= LEFT_MIN 40

  width 90 : panel 50,  90 - 50 = 40  >= 40   panel draws
  width 89 : panel 50,  89 - 50 = 39  <  40   panel absent
```

So `h`'s whole surviving job is field help between 33 (`MIN_TERM_WIDTH`) and 89
columns, where the panel is gone.

**The resolution is to fix that band rather than keep the key for it.** When
`panel_width` returns `None`, `top_line` draws the wrapped blurb for the field
under the cursor unconditionally — same source, same cursor-following, no key
required. `ConfigPane::help_open`, `toggle_help`, `close_help` and
`set_help_open` then have no callers and go, and `h` means one thing everywhere.

A key whose meaning depends on the terminal's width was the alternative, and it
is worse than either end of it.

## Two modes, and the overlay is not a third

`map_key` dispatches on `InputMode` and there are exactly two variants. The
overlay is a `bool` on `App`, not a mode:

```rust
/// Whether the keymap overlay is up. Not an `InputMode`: `map_key` has
/// exactly two modes and an overlay that swallows keys is a reducer
/// concern, not a keyboard-edge one.
keymap_open: bool,
```

`on_key` gains one branch, placed after the text-mode check and ahead of every
pane's own routing:

```
on_key(key)
  ├─ InputMode::Text        → on_text_key          (unchanged, first)
  ├─ self.keymap_open       → on_keymap_key         (new)
  ├─ config_pane().is_some() → on_pane_key          (unchanged)
  └─ ... the four other panes, then the dashboard
```

Text mode stays first, so the overlay can never open from inside an open box: `h`
typed into the filter is a `TextChar`. And because the config pane owns the
keyboard below this branch, a close dialog that is up keeps it — `h` cannot
reach the overlay while a dialog is waiting for an answer.

`on_keymap_key` swallows everything except four keys:

| key | effect |
|---|---|
| `h`, `?` (`Help`) | closes the overlay |
| `esc` (`Escape`) | closes the overlay |
| `q`, `ctrl-c` (`Quit`) | quits, the way 1g's dialog also allows |
| anything else | consumed, `Effect::None` |

Swallowing is the point. The overlay covers the flock table, so a `j` that
reached the reducer would move a selection the operator cannot see.

## The rows come from `map_key`, not from a list beside it

A hand-maintained list of keys next to `map_key` is the one thing that would make
this frame rot on the next binding. The overlay's rows are produced by running
`map_key` itself.

`crates/shep-cli/src/lookout/keymap.rs`:

```rust
/// One row of the overlay: the key column, the group it sits in, and the
/// sentence beside it.
pub struct Binding {
    pub keys: &'static str,
    pub group: Group,
    pub does: &'static str,
}

/// The four columns, in the order they draw.
pub enum Group { Moving, Looking, Changing, Doing }

/// What `press` means, for the overlay to print.
///
/// Exhaustive on purpose, with no wildcard arm: a forty-third `KeyPress`
/// variant does not compile until it has a row here, so a binding cannot
/// reach the reducer without also reaching the overlay.
const fn binding(press: &KeyPress) -> Binding

/// Every row the overlay draws, in group order, deduplicated.
///
/// Built by running `PROBE` through `map_key`, so a row exists only where a
/// real key produces a real `KeyPress`. Nothing here can name a key the
/// reducer does not dispatch on.
pub fn rows() -> Vec<Binding>
```

`binding` matches on `KeyPress::Action(ActionVerb::Stop)`, `Restart` and `Reload`
separately rather than on `Action(_)`, so `ActionVerb` is exhaustive too and the
three destructive keys get three rows.

`PROBE` is the list of `(KeyCode, KeyModifiers)` pairs the overlay claims. Two
tests hold it honest, in both directions:

- **`every_key_map_key_binds_is_in_the_probe`** sweeps `0x20..=0x7E` plus every
  named `KeyCode` `map_key` can match (`Esc`, `Enter`, `Backspace`, `Tab`,
  `BackTab`, the four arrows, `Home`, `End`, `F(1)..=F(12)`), bare and with
  `CONTROL`, in both modes, and asserts every `KeyPress` that comes back is also
  produced by some `PROBE` entry. A binding added to `map_key` and not to `PROBE`
  fails here.
- **`every_probe_entry_binds_something`** asserts the reverse, so `PROBE` cannot
  accumulate keys that stopped being bound.

The sweep is the complete surface: `map_key` matches on no other `KeyCode`
variant.

## Layout, and its arithmetic

The design draws the box 128 cells wide starting at column 17 of a 160-column
frame, rows 8–26, with an interior of exactly 126 cells. Centring reproduces
that rather than special-casing the target width:

```
box       128 = 126 interior + 1 border cell each side
left      (width - 128) / 2 = 16 at width 160, so column 17 1-based
interior  126 = 4 columns × 30 + 3 gutters × 2
column    30 = 12 key + 1 gap + 17 text
rows      19 = 1 border + 17 interior + 1 border
interior  1 heading + 12 entry + 1 blank + 1 gate + 2 closing = 17
floor     130 = 126 interior + 2 border + 1 margin each side
```

**The floor is 130, and the rulings' 132 is wrong.** 1g's `BOX_FLOOR` is
`BOX_WIDTH + 4`: 86 interior plus two border cells plus one margin cell each side
is 90, and `draw_boxed_close_dialog`'s `margin = (width - (BOX_WIDTH + 2)) / 2`
comes out at 1 there. The same formula over a 126-cell interior gives 130. The
rulings say 132, which would be a two-cell margin that 1g does not ask for and
this frame has no reason to.

So both floors stay one expression, `interior + 4`, and `rulings.md` and
`docs/lookout/design-files/README.md:332` are corrected rather than honoured. The
rulings win where they and a frame disagree about behaviour; an arithmetic slip
is not a disagreement. Two frames' floors coming out of two formulas is the thing
a later reader gets wrong, and it is cheaper to fix the number than to carry the
explanation.

The entry-row budget is 12 and the tallest group has 12. That is zero headroom,
and a test carries it rather than a comment:

```rust
/// Every group fits the rows its column has.
///
/// Zero spare at the time of writing: `Looking` has twelve entries against
/// twelve rows. A forty-third binding compiles (`binding` gives it a row)
/// and then overflows the box, so this is the test that names which group
/// grew rather than letting it clip on screen.
#[test]
fn every_group_fits_its_column()
```

## The rows

Thirty-five rows, covering all forty-two `KeyPress` variants.

```
MOVING                        LOOKING                       CHANGING                      DOING
j/k  ↑/↓     select a row       h  ?         this keymap        e            the config pane    x            stop
g/G home/end first / last       b            feed, full screen  space        cycle a value      R            restart
J/K          next / prev sheep  F            gather by fold     d            restore default    L            reload
ctrl-d/u     page the feed      z            collapse a fold    u            undo the edit
←/→          environment tab    o            out / err / both   s            shepherd settings              ▟█████▙
tab          next config group  m            minimum level      S            secrets                       ▟███████▙
1-8          jump to a group    f            follow the tail    D            delete a secret               ▜██▀ ▀██▛
n/N          next / prev match  w            wrap long lines    c            leave it running                ▀▘ ▀▘
↵            open selection     /            filter, or match   a-z 0-9      types into a box
esc          back one level     r            refresh now        ⌫ ↵ esc      erase, file, drop
                                v            reveal for 10s
                                y            copy the value

the three above each arm a confirm  ·  ↵ confirms  ·  10s to answer  ·  █ control enabled
colour is decoration only: every coloured cell says the same thing in words. NO_COLOR loses nothing but the colour.
h or ? closes this  ·  q or ctrl-c quits lookout
```

Every variant's row: `Quit` → the closing line, `Escape` → `esc`, `SelectUp`/
`SelectDown` → `j/k`, `SelectFirst`/`SelectLast` → `g/G`, `Refresh` → `r`,
`Action(Stop|Restart|Reload)` → `x`/`R`/`L`, `Confirm` → `↵`, `FilterStart` →
`/`, the four text-mode variants → `a-z 0-9` and `⌫ ↵ esc`, `Settings` → `s`,
`Secrets` → `S`, `Reveal` → `v`, `Copy` → `y`, `TabPrev`/`TabNext` → `←/→`,
`Cycle` → `space`, `Edit` → `e`, `Help` → `h  ?`, `Remove` → `d`, `StepUp`/
`StepDown` → `J/K`, `FoldView` → `F`, `Collapse` → `z`, `Bleats` → `b`,
`StreamCycle` → `o`, `LevelCycle` → `m`, `PageDown`/`PageUp` → `ctrl-d/u`,
`FollowToggle` → `f`, `WrapToggle` → `w`, `MatchNext`/`MatchPrev` → `n/N`,
`SecretDelete` → `D`, `NextGroup` → `tab`, `Group(_)` → `1-8`, `Undo` → `u`,
`Continue` → `c`.

### Three deviations from the frame, each for arithmetic

- **The gate sentence is one full-width line, not printed beside the three
  keys.** `each one arms, ↵ confirms, 10s to answer` plus the control label is 57
  characters against a 17-cell text field. Split across three stacked cells
  inside DOING it reads worse than stated once across the interior.
- **The sheep sits in DOING's own 30 cells, not the frame's 38-cell cell.** The
  rightmost 38 cells of a 126-cell interior start at 88, inside CHANGING's column
  (64–93), which runs to entry row 10 and leaves only three free rows there. The
  art is 10 columns wide, so DOING's column at 96–125 holds it with room, rows 5
  through 8.
- **`q  ctrl-c` has no row of its own**, which is what bought LOOKING its twelfth
  entry. It goes on the closing line beside `h or ? closes this`.

### The overlay is a global reference, not a per-body hint

It lists every binding at every body, and each row says where its key applies.
That does not contradict `view/status.rs`'s rule that "a hint naming a key that
is inert where the operator is standing is worse than no hint" — a status bar is
a hint about here, and this is a reference about everywhere. A fixed 128×19 box
with four fixed groups cannot reflow per pane in any case.

## Under the floor, and under the height

Below the floor the rulings say draw full width with no border box rather than
clipping. Four columns need 126 cells, so the borderless form also drops columns.
`n` columns take `32n - 2` cells, so `n = (width + 2) / 32`, clamped to 1..=4:

| width | form | columns | groups per bank |
|---|---|---|---|
| 130 and up | boxed | 4 | all four side by side |
| 126–129 | borderless | 4 | all four side by side |
| 94–125 | borderless | 3 | MOVING LOOKING CHANGING, then DOING |
| 62–93 | borderless | 2 | MOVING LOOKING, then CHANGING DOING |
| 33–61 | borderless | 1 | one group per bank, four banks |

The 126–129 band is four columns wide and easy to miss: it is the only place the
full four-column layout draws without a border, and it exists because the box
needs two cells the columns themselves do not.

Banks stack, separated by a blank row, so the rows get taller as the terminal
gets narrower — tallest exactly where there is least room, the same shape
`shed_dialog_rows` already handles for 1g.

Shedding order, when the height will not hold the form:

1. **the sheep.** The design calls it "the one decoration in the whole TUI that
   holds no information", which makes it the only thing here that can go without
   the overlay saying something false.
2. **the `NO_COLOR` line.** A statement about the design, not about a key.
3. **the gate sentence's tail**, down to the control label alone.

Below that, the overlay refuses with a sentence naming the height it needs rather
than drawing a partial key list, the way the secrets pane refuses a narrow
terminal rather than clipping it. A key list missing rows silently is worse than
one that says it cannot fit.

## The frozen dashboard

`FROZEN_HINT` (`view/status.rs:65`) reads `q quit   j/k still moves   every other
key is refused while the link is down`, and its doc calls that last clause "the
whole rest of the keymap, said once rather than discovered a keypress at a time".
There is no blanket refusal in `on_key`: only `Refresh` and `arm` test
`Link::Lost`.

The overlay opens when the link is gone, because a key list is most wanted on the
screen where nothing else works. Two things follow:

- **`FROZEN_HINT` gains it**: `q quit   h keymap   j/k still moves   every other
  key is refused while the link is down`. Appended before the refusal clause, so
  the sentence stays true.
- **The gate line names the link instead of the control gate.** With
  `Link::Lost`, `█ control enabled` becomes `█ the link is down · nothing acts`,
  because `x`/`R`/`L` then refuse for a reason `Control` does not carry.

The three states of that line:

| link | control | the line reads |
|---|---|---|
| live | `Allowed` | `the three above each arm a confirm · ↵ confirms · 10s to answer · █ control enabled` |
| live | `ReadOnly` | `the three above each arm a confirm · ↵ confirms · 10s to answer · █ read-only` |
| lost | either | `the three above are refused · █ the link is down · nothing acts` |

`"read-only"` and `"control enabled"` are `view/status.rs:278`'s own strings,
reused rather than restated.

## The box machinery is 1g's, extracted

1g built a box, a borderless fallback and a mute pass, all private to
`view/pane.rs` and all specialised to the close dialog: `BOX_WIDTH`, `BOX_FLOOR`,
the eight border glyphs, `dialog_is_boxed`, `boxed_dialog_height`,
`draw_boxed_close_dialog`, `draw_borderless_close_dialog` and `blank_row`, plus
the two `set_style` calls in `draw_pane` that dim the pane behind.

1k needs every one of those with a different width and a different content
builder. They move to `crates/shep-cli/src/lookout/view/overlay.rs`,
parameterised on the interior width, and both frames call them. Nothing about
1g's rendering changes; its four pinned snapshots must not move.

This is the consolidation rather than a second copy of it. The alternative is two
box drawers differing in one constant, and the four `create_*_file` helpers in
shep-core are the argument against that.

The mute pass moves with them, comment intact — it is two `set_style` calls
rather than one for a reason `view/pane.rs:2150` documents, and that reason
applies identically here.

Where the overlay draws is new, though. 1g draws inside `draw_pane`, because the
config pane is the only thing it covers. 1k covers whatever body is showing, so
its hook is at the end of `view::draw`, after the chrome:

```rust
if app.keymap_open() {
    overlay::mute(buffer, area, palette);
    view::keymap::draw(app, area, buffer);
}
```

## Glyphs

The border vocabulary is the one 1g checked:
`the_border_vocabulary_is_the_one_that_was_checked` (`view/pane.rs:2794`) pins
`▛▜▙▟▐▀▄▌` at one column each, and `view/pane.rs:65` records that three of them
are East-Asian Ambiguous and kept anyway, since `─` and `█░` already are.

Two glyphs are new and join that test rather than inheriting its answer:

- `▘` (U+2598), in the sheep's last row.
- `⌫` (U+232B), in the text-mode row. Below the rulings' U+2600 ceiling, but
  absent from the design's own vocabulary table, so it gets the same check the
  border glyphs got.

## Testing

Every test below is written to fail when what it claims to pin changes, and each
one is checked by mutating that thing, watching it fail, and restoring it. Six
tests across the panes already shipped pinned nothing, two of them passing on
text that was present for an unrelated reason, which is why this paragraph is
here rather than assumed.

**The derivation, in both directions.**

- `every_key_map_key_binds_is_in_the_probe` — the ASCII-plus-named-codes sweep
  above. Mutation: add a binding to `map_key` and not to `PROBE`.
- `every_probe_entry_binds_something` — the reverse. Mutation: leave a `PROBE`
  entry behind after unbinding its key.
- `every_group_fits_its_column` — the row budget. Mutation: add a thirteenth
  `Looking` row.
- The compiler covers the third direction: `binding`'s match has no wildcard, so
  a new `KeyPress` variant is a build failure. Checked by adding a scratch
  variant and confirming `cargo check` refuses it.

**The keys.**

- `question_mark_opens_the_keymap_and_slash_still_filters` — `?` is `Help` and
  `/` is still `FilterStart`, so the shifted pair did not collide.
- `the_overlay_swallows_a_movement_key` — with the overlay up, `j` returns
  `Effect::None` and the selection index is unchanged. This is the test that
  catches a `j` reaching the table under the box, and it asserts on the index
  rather than on the effect alone.
- `the_overlay_closes_on_h_and_on_question_mark_and_on_esc`.
- `q_still_quits_with_the_overlay_up`.
- `the_close_dialog_keeps_the_keyboard_from_the_overlay` — with a dialog up, `h`
  does not open the overlay. Pins the `on_key` ordering.
- `h_in_a_filter_box_types_a_letter` — text mode stays ahead of the overlay
  branch.

**The config pane's help, now that the key is gone.**

- `the_blurb_draws_at_a_width_with_no_panel` — at 89 columns the field under the
  cursor has its help text on screen. Mutation: revert `top_line` to its
  `help_open` condition; the test fails because no key is pressed.
- `the_blurb_follows_the_cursor_with_no_panel` — move the cursor, assert the text
  changes. Catches a blurb that draws the first field's help at every cursor
  position.
- `the_panel_is_the_only_blurb_where_it_draws` — at 160 columns the help string
  appears once, not twice. Pins that the fix did not double the text at the
  design target.

**The overlay's own rendering.** Gallery scenes, through `frames.rs`, each
pinning what it exists to show rather than only that the overlay drew:

| scene | width × height | pins |
|---|---|---|
| `Keymap` | 160 × 48 | the box at 128 cells, four columns, the sheep, all 35 rows |
| `KeymapFloor` | 130 × 48 | the narrowest boxed form: border intact, margin 1 each side |
| `KeymapBorderlessWide` | 128 × 48 | one column under the floor: four columns, no border |
| `KeymapNarrow` | 100 × 48 | three columns, DOING on its own bank |
| `KeymapTwoColumn` | 70 × 48 | two columns, two banks |
| `KeymapFrozen` | 160 × 48 | the gate line naming the dead link, not the control label |
| `KeymapReadOnly` | 160 × 48 | `█ read-only` in the gate line |
| `KeymapShort` | 160 × 20 | the sheep shed, the key rows intact |

`KeymapFloor` and `KeymapBorderlessWide` are two columns apart on purpose: 130 is
the narrowest box and 128 the widest borderless form that still carries all four
columns. Either one alone would leave the boundary untested, and the 126–129 band
is narrow enough that a scene at 100 never reaches it. A scene pinned one column
short of its own column set silently drops the thing it exists to show, which
happened once already in this bundle.

Each width scene asserts the **column count it exists to show**, by counting
occupied column starts on a row it knows has entries in every group, not by
asserting the overlay drew at all. A scene that only checks for the word `MOVING`
passes at every width in the table.

## Docs

`CLAUDE.md`'s docs trigger fires: this adds a key an operator types.

- **`web/src/pages/docs/lookout.astro`** gets a keymap section: `h` or `?`, what
  the four groups are, that the overlay swallows other keys, and that it works
  when the link is down. The page's existing description of `h` as the config
  pane's field-help key (lines 1034 and 1089) is now wrong twice over — the key
  moved and the help is automatic — and both need rewriting.
- **`docs/lookout/design-files/README.md:317`** gets `g` → `S` and `l` → `b`, and
  the `h` or `?` claim becomes true. Line 332's `132 columns` becomes `130`.
- **`docs/lookout/design-files/rulings.md`** gets a correction paragraph in the
  form its 1h section already uses, recording that the 132-column floor it states
  is 130: `interior + 4`, the same expression 1g's `BOX_FLOOR` uses. The rulings
  are the authority on which claims survive contact with shipped shep, so a
  number they get wrong is corrected there rather than worked around here.
- **`docs/lookout/README.md`** documents the overlay and the amended
  `FROZEN_HINT`.
- **`web/scripts/generate-cli-reference.sh`** is run and its diff checked. No
  flag changes, so the expectation is no diff; the run is what confirms it.
- `npx astro build` and `npx astro check` both, since `check` is what catches a
  wrong prop and `build` does not.

## Not in scope

- **A third `InputMode`.** Stated because it is the obvious wrong turn: an
  overlay that swallows keys looks like a mode and is not one.
- **Per-body filtering of the row list.** The box is fixed and the groups are
  fixed.
- **A second decoration.** The design allows the sheep "exactly once, here".
- **Rebinding anything.** `?` is added; nothing that is bound today changes
  meaning, apart from `h` inside the config pane, which this spec covers.
