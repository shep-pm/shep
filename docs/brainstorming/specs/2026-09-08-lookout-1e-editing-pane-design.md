# Lookout pane 1e: the editing pane

**Status:** approved 2026-09-08.

The config pane, redrawn as a field list beside an explanation panel, and
rewired from a write per edit to one batch on close.

`docs/lookout/design-files/rulings.md` is the authority on which of the bundle's
claims survive contact with shipped shep. Where a frame and a ruling disagree,
the ruling wins.

## What already exists, and why that changes the job

The handoff reads as though this pane is new. It is not. `Body::ConfigPane` is
bound to `e` today, `ConfigPane::sheep` reads the live Flockfile schema at
runtime, and the pane already pulls `blurb`, `group`, `suggest` and `default`
per field and renders an `ApplyGroup` in a COST cell. Most of what the handoff
calls the cheap part of 1e shipped in Phase 15.

So this is a redesign of a working surface, and the two things it actually
changes are the layout and the write model. The second is the one with
consequences.

### The write model

```
today   select field -> space or enter -> Armed -> enter confirms
        -> one write, one ticket -> await the shepherd -> settle
        (per field, per round trip)

1e      select field -> space or enter -> into `edits` -> repeat
        esc -> validate -> send the set -> close
```

1g, the close dialog, only has a subject under the second model: with a write
per edit there is no set to ask about. `u` undo is likewise meaningless until
there is something unsent to undo. That is why the pending set is an interface
other frames consume rather than a detail of this one.

Batching is not a new outcome, only a new shape. The shepherd already parks a
write that needs a respawn rather than refusing it, and the flock table already
reports that as `cfg !N pending`. Five batched writes land exactly as five
sequential writes land today.

## The pending edit set

Lives in `crates/shep-cli/src/lookout/edits.rs`, a new module with its own
tests. `ConfigPane` holds one. Nothing lands in `app.rs` except a reducer arm.

The handoff sketches `edits: BTreeMap<FieldId, (Value, Impact)>`. Three parts of
that shape do not survive contact with the existing types:

- A key is not one string. A config field key and an env key are both `String`,
  and `env` is itself an `AppConfig` field name. `PaneEdit` already splits `Set`
  from `SetEnv` for this reason.
- A value is not one type. A field carries `FieldValue`, JSON with a redacted
  `Debug`. An env key carries `Option<EnvValue>`, where `None` removes the key.
  `PaneEdit` already carries both, so the set stores one of those rather than a
  parallel value enum of its own. A second enum beside it would say the same
  thing twice and make a mismatched key and value expressible.
- Impact is optional. `ConfigPane::cost` returns `None` for a dog, which has no
  `apply_group` table, and dogs are in scope.

```rust
pub enum EditKey { Field(String), Env(String) }

pub struct Edit  { edit: PaneEdit, impact: Option<ApplyGroup> }

pub struct Edits {
    entries: BTreeMap<EditKey, Edit>,
    order:   Vec<EditKey>,
}
```

The key is derived from the `PaneEdit` on the way in, so a config value can
never be filed under an env key.

Two collections rather than one, the same shape `Filters` in `pane_bleats.rs`
uses so `esc` can drop its newest chip. `entries` gives render order, `order`
gives undo order.

### The public surface, and who reads it

| Method | Reader |
|---|---|
| `worst_impact() -> Option<ApplyGroup>` | 1g, to decide whether it appears |
| `len()`, `is_empty()` | the title band, the legend, 1k's count |
| `iter()` | the `pending edits` section, one `old -> new` per row |
| `undo() -> Option<EditKey>` | `u` |
| `into_writes() -> Vec<PaneEdit>` | close, straight onto the existing wire type |

`worst_impact` ranks locally, `Live < NextSpawn < NeedsRespawn`. `ApplyGroup` is
`#[non_exhaustive]` and derives no `Ord`, and a rank function is an ordering over
one notion of cost rather than a second notion of it. `Structural` cannot appear:
those fields carry `Lock::Refused` and no key reaches them.

### Four behaviours decided here rather than discovered later

- **An edit back to the stored value removes the entry.** Otherwise the title
  band counts a no-op and 1g asks about nothing.
- **Re-editing a field moves it to newest in `order`.** `u` should undo what was
  last touched.
- **Undo restores nothing.** It removes the entry, and the row falls back to the
  stored value it was already reading. There is no `was` field to keep in sync.
- **The set survives a config re-read.** `r` refetches `SheepConfig` while the
  pane is open. The values are the shepherd's and get replaced; the edits are the
  operator's and do not.

`Debug` derives on all three types. `PaneEdit`'s own `Debug` already withholds
both value types, because `FieldValue` and `EnvValue` each redact themselves
(IR-41).

## Dogs

In scope, same layout and same batch. A dog's COST, env and group regions are
simply empty, which they already are.

One consequence: `ConfigPane::edited_section` folds a single `PaneEdit` into the
dog's `[<name>]` TOML section with comments and key order intact. Batching means
folding N edits into one document, which is the same `toml_edit` walk in a loop,
and a dog then takes one section write instead of N.

## Layout

Design target 160x48. Left column 88, right panel 72.

### Width

**Corrected 2026-09-11.** This section previously said that `LANDS` and the panel
say the same thing, so exactly one of them is on screen at any width. That was an
invention of mine, and the frame refuses it: at 160 the frame draws the `LANDS`
header and the `FOCUSED` panel on the same row, and `rulings.md` says 1e goes
ahead as drawn.

They do different jobs. `LANDS` is a column an operator scans to read every
field's cost at once. The panel is a sentence about the one field under the
cursor. Nothing but the column answers which of forty fields will cost a respawn
without walking the cursor through all of them.

What survives is the drop order rather than the exclusion. Where both fit, both
draw. Where they cannot, `LANDS` gives way first, because the panel restates the
focused field's cost in words and nothing restates the column.

| Terminal width | Left | Panel | `LANDS` |
|---|---|---|---|
| 160 and up | 88 | 72 | present, both draw, as the frame shows |
| 90 to 159 | the remainder | 45% of width, clamped to 50..72 | dropped, the panel still names the focused field's cost |
| below 90 | today's `widths()` cascade | dropped | back, since nothing else carries cost |

45% of 160 is 72, so the design target falls out of the formula rather than being
special cased. Below 90 the panel cannot hold a wrapped blurb and a validation
list, and the pane degrades to what ships today.

### Height

The body sheds in this order:

1. the legend and its hairline, which name markers still visible beside them
2. the provenance row
3. the `FIELD / VALUE / LANDS` header row
4. below that, the existing `cursor_only` floor, unchanged

The group tab row never drops. Without it nothing on screen names the group the
list is showing.

The panel truncates from the bottom: blurb, then `now` / `default` / `example`,
then the impact sentence, then `VALIDATION`, then `NEIGHBOURS`.

### Row order in the left column

The active group's fields, blank, the `pending edits` rule and one row per edit
across all groups, blank, the `env` rule and its keys, then `+ add a key`.

The cursor walks the group's fields and the env rows. Pending edit rows are
display only: `u` already reaches them, and selecting a mirror of a row three
lines above buys nothing.

### The marker column

Two marker cells already ship. The frame collapses them into one and reuses a
glyph that is taken.

| Glyph | Means today | Frame 1e wants |
|---|---|---|
| `=` | `Lock::Refused`, no surface writes it | same |
| `~` | `Lock::NoWidget`, this pane has no editor for the shape | same |
| `!` | parked on the shepherd, the `!` in `cfg !2 pending` | changed by you, unsent |
| `*` | an override the Flockfile does not declare | no home in the frame |

Taking `!` would give it opposite meanings on two panes an operator moves
between with one keypress. All four shipped meanings stay, and a client-side
edit gets no marker: its `VALUE` cell already reads `(unset) -> 52M` in butter,
the same `old -> new` the `pending edits` section prints. No cell is glyph only,
which is what the design's third rule asks for. The legend's `! changed by you`
becomes `-> changed by you, not yet written`.

### Group tabs

Eight groups, in `GROUP_ORDER`: process, logging, inputs, restart, readiness,
shutdown, watch, cron. The frame's tab row draws a different order and loses to
the constant, which `shep init` already orders its scaffold by.

## Keys

No third `InputMode`. `map_key` keeps dispatching on mode, and everything below
is interpreted by the config pane's reducer branch on `Body`, the way
`on_bleats_key` works.

| Key | `KeyPress` | Note |
|---|---|---|
| `tab` | `NextGroup` | new, unbound today |
| `1`..`8` | `Group(u8)` | new, digits unbound in `Normal`, carries data like `Action(ActionVerb)` |
| `u` | `Undo` | new, bare `u` unbound, only `ctrl-u` is taken |
| `d` | `Remove` | renamed from `ListRemove` |

`esc`, `j`/`k`, enter, `space` and `h` already map to `Escape`,
`SelectDown`/`SelectUp`, `Confirm`, `Cycle` and `Help`.

The rename is not cosmetic. `d` restores a field's default, which under batching
means removing the operator's value so the default shows through, and that is
the same verb the list sub-screen's `d` performs on an element. One name honest
in both places beats a name that lies in one.

### What `PanePending` becomes

`Armed` goes: nothing leaves per edit, so there is nothing to confirm. `Typing`
stays as the text editor. `Sent` stays, once, for the close-time write.

### Validation runs on entry

An edit joins the set only if it parsed, so the set is always sendable and `esc`
always writes. The frame's `checked as you type, refused on close` becomes
`checked as you type`. There is no cross-field rule in the repo to check at
close, and inventing one would widen the grammar past the spec. `apply_typing`
already does the on-entry half for integers, holding the editor open rather than
refusing.

A shepherd-side refusal now arrives after the pane has closed, so it lands as an
`App::notice` naming the field. That is a real cost of batching and is recorded
rather than hidden.

### Read-only refuses at the first keypress

The gate sits on the write today. Under batching that would let an operator
build five edits and lose all of them at `esc`. The first `space` is refused
instead, with the shipped sentence (`read-only: from --read-only or
lookout.allow_control`). Opening the pane stays allowed, since reading config is
a legitimate thing to do with no control.

## Env

Folds into the left column under an `env` group rule, with `+ add a key` closing
the list. The write-only sub-screen goes away and its cursor and viewport merge
into the list.

Every value renders `(set)`. `SheepConfigView::new` clears `config.env` before
the struct is built and keeps only `env_keys`, so no value for any key reaches
this pane, Flockfile or store. The frame's `NODE_ENV production` is one of the
claims the rulings already refused.

## The explanation panel

Five regions. Three have a source today, two do not.

| Region | Source | Exists |
|---|---|---|
| blurb | `init.blurb` | yes, `Field::help` |
| `now` | `ConfigPane::display_value` | yes |
| `default` | JSON Schema `default` | yes, `Field::default` |
| `example` | `init.example` | in the schema, dropped by `field.rs` |
| impact sentence | `apply_group` | yes |
| `VALIDATION` | nothing | no |
| `NEIGHBOURS` | nothing | no |

`Field` gains four members: `example`, `accepts`, `refuses`, `neighbours`, all
read by the `field_from` walk that already pulls `blurb` and `suggest`.

### The type table, for the 22 fields a parser describes

Keyed on `field::ValueKind` and `field::FieldKind`, roughly six entries, living
in the CLI beside `cost_label` because it is rendering copy.

```
UpDuration  accepts: 500ms, 2s, 1m30s, a bare number is milliseconds
            refuses: a negative, a unit shep does not know
MemSize     accepts: 512M, 2G, a bare number is bytes
Bool        accepts: true, false
Integer     accepts: a whole number, no unit
Choice      accepts: one of the listed names
```

**The table verifies itself, which is what makes keeping it away from the parser
safe.** Every string under `accepts` is fed through the real parser in a test and
must parse; every string under `refuses` must fail. The table cannot go on
claiming `2s` works after `UpDuration` stops taking it.

### Per-field annotations, for the 19 fields a type cannot describe

Three new keys under `init` in `crates/shep-core/src/config/app.rs`, beside the
four already there:

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
                   {"field": "ignore_watch", "note": "globs are rooted here"}]
})))]
pub cwd: Option<String>,
```

Four rules keep this from spreading:

- Per-field replaces, never merges. A field writing `accepts` gets those bullets
  and none from the type table. One rule, no merge semantics to reason about.
- Missing means absent. A field with no `neighbours` renders no `NEIGHBOURS`
  heading, the way the detail pane shows nothing rather than `cfg !0 pending`.
- Every name under `neighbours` must be a real field, tested. That catches the
  typo, which is the only failure mode a hand-written cross-reference has.
- Annotate where it helps. Not all 19 fields need all three keys.

`shep schema` prints this, so its output grows and a golden test of it moves.
That is additive to an extension blob rather than a contract change, and
`SCHEMA_VERSION` governs the JSON envelope rather than this.

## Terminology

`pending` means two things one keypress apart, and the pane has to say which.

- The shepherd's: fields written and parked until a respawn.
  `SheepConfigView::pending`, the flock table's `cfg !2 pending`, the detail
  pane's cell. This keeps the word.
- The pane's: edits made and not yet written. The title band says `2 edits`, not
  `2 edits pending`, and the legend says `not yet written`.

## Testing

### Nothing ships that no key can reach

- Every new `KeyPress` variant gets a test that sends it through
  `App::update(Msg::Key(..))` and asserts an observable change, never by calling
  a pane method directly.
- `tab` is walked through all eight groups, asserting each group's own fields
  appear.
- `1`..`8` are asserted to reach the same eight groups `tab` does.
- Every field of `Edits` is both written and read by a key-driven test.

### Assertions are bounded

**Assert on a slice of known rows, never `contains` over the whole rendered
frame.** A frame-wide `contains("respawn")` passes off the legend row. Where a
whole-frame search is genuinely right, assert the count rather than the presence.

Every test gets the mutate, watch it fail, restore pass before it counts as
written, and then one question: what else could make this assertion pass.

### Frame gallery

Four new `Scene` variants, each driven by real key presses the way
`Scene::Bleats` is: a fresh pane, a pane with two edits, a pane at 120 columns
with the panel squeezed, a pane below 90 with the panel gone and `LANDS` back.
Then regenerate:

```bash
cargo test -p shep --lib --all-features -- --ignored write_the_gallery
```

## Docs

The docs trigger fires. This changes what an operator types and sees.

- `docs/lookout/README.md` gains a `What 1e settled` section, matching the five
  already there.
- `web/src/pages/docs/*.astro` get grepped for the lookout keymap and the config
  pane before any of them is assumed fine.
- `cargo build --release && ./web/scripts/generate-cli-reference.sh`, then
  `git diff`. No verb or flag moves, so the generated reference should not
  change. If it does, something else drifted.
- `cd web && npx astro build`, then `npx astro check`.

## Not in scope

- 1g, the close dialog. `Edits::worst_impact` is the hook it needs.
- 1k, the keymap overlay, which comes after both.
- 1h, secrets, which waits for the store.
- Per-row undo. `u` pops the newest and the frame draws no other affordance.
