# Lookout pane 1g: the close dialog

**Status:** approved 2026-09-12.

The question the config pane asks when it closes carrying changes the running
process has not taken: restart now, reload, or leave them parked.

`docs/lookout/design-files/rulings.md` is the authority on which of the bundle's
claims survive contact with shipped shep. Where a frame and a ruling disagree,
the ruling wins. The rulings list 1g among the frames that go ahead as drawn,
and refuse two of the sentences it draws.

## What already exists, and why that changes the job

1e landed on 2026-09-12 (#206) and the pending edit set is there, tested, with
`Edits::worst_impact` carrying `expect(dead_code, reason = "frame 1g's close
dialog reads it; that frame is not built yet")`.

**It turns out this frame does not read that one.** `worst_impact` answers with
the heaviest group in the set, and the dialog names fields: `cwd and err_file
take hold when the process starts again` is a list, not a maximum. So the half
of the set this frame needs comes through `Edits::iter()` and `Edit::impact`,
both of which 1e also built and one of which the COST column already reads.
`worst_impact` and its `rank` helper have no caller and should be deleted here,
rather than left carrying a reason that names a frame which went past them.
Recorded because the 1e spec named `worst_impact` as 1g's hook and it is worth
saying plainly that the coarser answer was the wrong shape, not that it was
missed.

Three more things the handoff does not know:

**An offer already fires on this keypress.** `PaneMenu` (`app.rs:1375`) is a
`Copy` struct of `{ parked, reload, at }`, raised by `apply_offer`
(`app.rs:4844`) when `esc` finds `parked_count() > 0`. It acts on the press
rather than arming, expires on the same ten seconds, and draws as one line at
the top of the field list. It is the press-to-act carve-out
`docs/lookout/README.md:72` documents, and it predates 1g by two phases.

**That offer misses the case it exists for.** `esc` writes first
(`app.rs:4602`) and asks second, and `parked_count()` reads
`SheepConfigView::pending` from the fetch that opened the pane. An edit made in
this pane is not in it. So an operator who changes `cwd`, presses `esc`, and had
nothing parked beforehand gets no offer at all, on precisely the keypress that
created something to offer about. 1g fixes that by asking before it writes.

**lookout has never drawn an overlay.** `view/mod.rs:262` calls every
full-screen surface "a swap, not an overlay, so nothing below draws while one is
up", and `view/status.rs:188` says "there is no overlay anywhere in this
module". Both are accurate about what shipped. The rulings put 1g ahead as
drawn, and as drawn it is a bordered box over a dimmed 1e, so this frame is
where that stops being true. It needs a mute pass and a box, and nothing in the
module does either today.

## When it appears

`esc` stops writing on its own. It asks, and the answer writes.

```
esc ─┬─ nothing filed, nothing parked ─────────────────→ close
     ├─ nothing filed reaches the running child ───────→ write, close
     ├─ the sheep is not running ──────────────────────→ write, close
     ├─ a dog ─────────────────────────────────────────→ write, close
     ├─ read-only ─────────────────────────────────────→ close
     └─ anything the running child has not taken ──────→ 1g
```

The last line is one predicate over two sources, not two rules:

| Half | Source | Authority |
|---|---|---|
| unsent | `Edits::iter()`, each key put through `reaches_running` | a prediction |
| parked | `SheepConfigView::pending`, non-empty | the shepherd's own answer |

Either half raises the dialog. Both raise one dialog, and the heading names both
counts. Two dialogs on one keypress is the outcome this arrangement exists to
avoid.

### `reaches_running`, and why it moves to core

The two halves disagree about `ApplyGroup::NextSpawn` unless something is done
about it, and the daemon already knows the answer. `supervisor.rs:5117`:

```rust
let in_force =
    !park_all && (group == ApplyGroup::Live || matches!(key, "autostart" | "depends_on"));
```

with the comment above it saying why: both are read at a muster, a boot or an
ordered walk rather than at a spawn, so "telling an operator to restart for
either would be telling them to do nothing". The other three `NextSpawn` fields,
`kill_signal`, `listen_timeout` and `readiness_probe`, do come off the per-sheep
task's `ResolvedApp` and a respawn is what applies them.

That fact currently lives in one inline `matches!` inside a 12,000 line file,
and the pane needs it too. It moves to `crates/shep-core/src/config/apply.rs`,
beside `apply_group`:

```rust
/// Whether a write to `field` reaches a child that is already running.
///
/// `false` for every field a respawn is what applies, which is what the
/// close dialog asks about and what the shepherd parks.
#[must_use]
pub fn reaches_running(field: &str) -> bool
```

`supervisor.rs` reads it instead of spelling its own copy, so the dialog's
prediction and the shepherd's `pending` list cannot drift apart. `park_all` is
not part of it: that is a normalize failure on a subset, which no client can
predict and the refetch corrects anyway.

Reading `ApplyGroup` and then deciding independently what a change costs is the
thing this avoids. The pane classifies nothing.

## Three cases that get no dialog, each because the alternative says something false

**A sheep that is not running.** Nothing holds the old config, so there is
nothing to respawn and every edit takes hold at the next start by definition.
Worse, `R` on a stopped sheep starts it, which is not what a config pane was
asked to do. `App` reads the status off the flock map by name, the same lookup
`flock_target` already makes.

**A dog.** `ConfigPane::cost` answers `None` for a dog, which has no
`apply_group` table, so `worst_impact()` is `None` by construction and no
filtering is needed to get there. Its write is one `SetDogSection` and it
closes.

**Read-only.** 1e refuses the first keystroke that would file an edit, so the
unsent half is always empty, and `apply_offer` already returns `None` under
`Control::ReadOnly` for the parked half. Both are covered before the dialog is
reached, and `apply_parked` re-reads the gate on the send anyway.

## Keys

| Key | Writes | Sends | Pane |
|---|---|---|---|
| `R` | yes | `Restart`, once the writes are answered | closes |
| `L` | yes | `Reload`, once the writes are answered | closes |
| `c` | yes | nothing, the edits park | closes |
| `esc` | no | no | stays open, edits still filed |
| `q`, Ctrl-C | no | no | quits |
| anything else | no | no | the dialog stays up |

`esc` writing nothing is what makes `keep editing` a true label, and it is the
behaviour `view/status.rs:427` predicted in a doc comment: the hint reverts from
`esc write & close` to `esc close` when this frame lands.

**An expiry is an `esc`, not a `c`.** Ten seconds on `CONFIRM_EXPIRY`, the same
tick that expires `PaneMenu` and an armed action today. It writes nothing and
leaves the pane open. A dialog nobody answered is not consent to write.

Of the three, only `c` is unbound today, so `KeyPress` gains one variant.
`R` and `L` already arrive as `KeyPress::Action(ActionVerb::Restart | Reload)`.

### Routing

No third `InputMode`. `map_key` keeps dispatching on the two it has, and the
dialog is interpreted where `PaneMenu` is interpreted now: a nested check at the
top of `on_pane_key`, ahead of everything else.

```rust
if self.close_dialog.is_some() {
    return self.on_close_dialog_key(key);
}
```

`PaneMenu`, `apply_offer`, `on_pane_menu_key`, `menu_text` and `top_line`'s menu
branch all go. `apply_parked` stays as the send path, taking the verb the dialog
held.

## Write, then act

Order is not optional: a restart sent before the write lands respawns into the
old config, which is the outcome the operator pressed `R` to avoid. It is also
already guaranteed. `link.rs:179` takes one request off the channel at a time
and awaits it inline, holding the other arms, so an action last in the same
`Effect::SendAll` goes out only after every write has been answered.

What the batch cannot express is the refusal case, and refusals are rare rather
than impossible now that 1e validates on entry: a `cwd` the daemon's user cannot
enter still comes back refused.

**The action fires when at least one write landed, and is dropped when every
one was refused.** Edit `cwd` and `err_file`, have `cwd` refused, press `R`: the
restart still goes, because `err_file` needs it. Edit only `cwd`, have it
refused: no restart, because bouncing a healthy process to apply nothing is the
one outcome with a cost and no benefit.

So `App` holds the verb rather than queueing it, counts the tickets it is
waiting on, and sends on the last reply. Refusals land as the notice 1e already
raises, naming the field. The held verb expires on `CONFIRM_EXPIRY` like
everything else, so a reply that never comes cannot strand it.

## Layout

Two forms. The box, and the same content full width with no border.

```
width >= 90     box, 86 cells wide, centred
width  < 90     full width, no border box
```

86 plus a border cell and a margin cell each side is 90, which is where the
floor comes from. The rule is `docs/lookout/design-files/README.md:332`, not
rulings.md: "The 1g and 1k overlays need 90 and 132 columns; below that, draw
them full-width with no border box rather than clipping." **Corrected
2026-09-12.** This section cited rulings.md, which is 92 lines and says nothing
about clipping or a floor. The box is 12 rows: a border pair, the heading, two sentences, three
option rows with a continuation for the long reload line, and the `esc` line.
The borderless form sheds its blank rows first and floors at 6, below which the
field list it is drawn over cannot render either.

Rows 19 to 30 at the design height of 48, centred horizontally: at 160 that is
37 dimmed columns each side.

### Dimming

The pane draws exactly as it draws now, then a mute pass walks the body rect and
replaces every style with the muted ink, then the box draws on top. 1e's own
render is not touched, which is what should keep its four pinned snapshots from
moving. If they move, something else did.

Muting is a colour operation, so under `NO_COLOR` the pane behind does not dim
and the border carries the separation. **Corrected 2026-09-12**, having said
"the border plus the reverse-video heading": `REVERSED` is added only by
`Palette::band` (`crates/shep-cli/src/lookout/theme.rs`), which this dialog
never called, so the heading was plain text with no colour behind it. The
heading takes `Palette::band` for that reason, which is the same rule 12a
settled: colour is redundant with text, so `NO_COLOR` loses decoration and
never information.

### The border, checked as the rulings ask

The rulings flag `▌` as East-Asian Ambiguous and say the rest of the glyph
vocabulary needs the same check. Run against `unicodedata.east_asian_width`:

| Glyph | Where | Class |
|---|---|---|
| `▛ ▜ ▙ ▟` | corners | Neutral |
| `▐` | left edge | Neutral |
| `▀ ▄ ▌` | top, bottom, right edge | Ambiguous |

The vocabulary is kept. The bar in this repo is not "no Ambiguous glyph": `─`
already draws every hairline rule at full width and `█░` fill every gauge, and
all three are Ambiguous. What `flock.rs:64` rejected `▸` and `▌` for was the
gutter, where a doubled cell shifts a whole row.

A box has one position with that exposure, the right edge, and no Neutral
right-half block exists to swap in. So `▌` stays, the three Ambiguous glyphs are
named in a comment beside the border, and a test pins the set so a later glyph
change gets the same check rather than inheriting the answer.

## The copy

Every number the frame draws is invented. `About 1.6s of downtime` is a guess,
and `No downtime, slower` is the sentence the rulings refuse outright, because
an app with `reuse_port false` and a readiness probe reloads serially and an app
that does not really set `SO_REUSEPORT` takes `EADDRINUSE`. Both are replaced by
fields the pane already holds.

```
▛▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▜
▐  2 EDITS NEED A RESPAWN            catcher is online, pid 71578                     ▌
▐    cwd and err_file take hold when the process starts again.                        ▌
▐    Everything else you changed is already live.                                     ▌
▐                                                                                     ▌
▐    R   restart now      stop, then start. The stop takes up to 5s.                  ▌
▐    L   reload           drains it, then starts the replacement. Up to 10s,          ▌
▐                         so slower than a restart for the same gap.                  ▌
▐    c   continue         write them and leave it running. They wait for a respawn.   ▌
▐                                                                                     ▌
▐    esc  keep editing, write nothing   ·   this prompt expires in 7s ███████░░░      ▌
▙▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▟
```

`5s` is the sheep's `kill_timeout`, the grace between the stop signal and
SIGKILL. `10s` is its `graceful_timeout`, the drain window a serial reload gets.
Every measurement states its denominator, which is the second of the design's
own rules.

### The reload line

One of four, off `ConfigPane::reload_kind()` (`pane.rs:1636`, already shipped
and already predicting the daemon's `ReloadMode::of`) and `instances`:

| Mode | Instances | Line |
|---|---|---|
| Overlap | 1 | the replacement starts alongside and takes over. No gap, if the app sets `SO_REUSEPORT` itself. |
| Overlap | N | one instance at a time, each replacement alongside the one it replaces. No gap, if the app sets `SO_REUSEPORT` itself. |
| Serial | 1 | drains it, then starts the replacement. Up to `<graceful_timeout>`, so slower than a restart for the same gap. |
| Serial | N | one instance at a time, each drained before its replacement starts. Up to `<graceful_timeout>` each, 1 of N down at a time. |

The `SO_REUSEPORT` caveat rides on both overlap lines rather than only the
`reuse_port` one. `AppConfig::reuse_port`'s own doc comment is why: an app with
no readiness probe overlaps either way, and "both of those need `SO_REUSEPORT`
as much as a `reuse_port` app does if they bind an address".

Never "lamb". An instance is not one, which `docs/terminology.md:20` says in
those words. The rulings found four places in the bundle calling them lambs
anyway, and this frame is one of them.

### The heading

Three forms, one per half that fired:

```
2 EDITS NEED A RESPAWN
1 FIELD IS ALREADY WAITING
2 EDITS NEED A RESPAWN, 1 FIELD ALREADY WAS
```

Numerals rather than the frame's `TWO`, matching the title band of the pane
underneath, which already says `2 edits`. The sentence below names the fields
and truncates past three: `cwd, err_file and 3 more`. `Everything else you
changed is already live` draws only when there is something else.

## Terminology

`pending` still means what 1e settled it means, and this dialog sits exactly on
the seam. The shepherd's `pending` is fields written and parked. The pane's set
is edits filed and not yet written. The heading keeps the two apart by naming
`EDITS` for one and `FIELD` for the other, and the `c` line says `write them`,
which only makes sense for the unsent half.

## Testing

Every assertion is mutated, watched fail, and restored before it counts as
written, and then asked what else could make it pass. Three traps are specific
to this frame and are called out so they are not rediscovered:

- **A frame-wide `contains("respawn")` passes off 1e's legend row**, which is on
  screen underneath the dim. This is not hypothetical the way it usually is: the
  legend literally renders the word behind the box. Assert on the dialog's own
  rows by slice.
- **A dim test that asserts text is asserting nothing.** Dimming changes style
  and leaves every character alone. It asserts the style of a row behind the
  box.
- **The carve-outs are tested through `esc`, not through the predicate.** A
  predicate test passes happily over a branch the key never reaches.
  `no_dialog_for_a_stopped_sheep` files a real edit and presses a real `esc`.

Also pinned: that `reaches_running` and the daemon's `in_force` agree for every
`AppConfig` field, which is the assertion that keeps the hoist honest after the
next field is added.

### Frame gallery

Four scenes, each driven by real key presses, each carrying its width
arithmetic in a comment beside it, because a scene pinned one column short of
what it exists to show fails silently.

| Scene | Size | Why that number |
|---|---|---|
| `CloseDialog` | 160x48 | the design target: (160 - 86) / 2 = 37 dimmed columns each side |
| `CloseDialogFloor` | 90x48 | the floor exactly: 86 + 2 + 2. At 89 the border is dropped |
| `CloseDialogNarrow` | 89x48 | one below the floor, borderless and full width |
| `CloseDialogParked` | 160x48 | the parked half alone, no unsent edits, the second heading |

Then regenerate:

```bash
cargo test -p shep --lib --all-features -- --ignored write_the_gallery
```

## Docs

The trigger fires: this changes what an operator sees and adds a key.

- `docs/lookout/README.md` gains a `What 1g settled` section, and its 12a
  bullet about the apply menu changes, since the menu that bullet describes is
  deleted here.
- `view/status.rs:427`'s doc comment reverts as it said it would, along with the
  hint string it guards.
- `web/src/pages/docs/*.astro` get grepped for the lookout keymap and the config
  pane's close behaviour before any of them is assumed fine.
- `cargo build --release && ./web/scripts/generate-cli-reference.sh`, then
  `git diff`. No verb or flag moves, so the reference should not change. If it
  does, something else drifted.
- `cd web && npx astro build`, then `npx astro check`. Both, because a wrong
  prop builds clean and only `check` reports it.

## Not in scope

- 1k, the keymap overlay. It needs 132 columns and the same box, so it should
  reuse whatever this frame builds rather than the reverse.
- A general layering primitive. This draws one box; a second caller decides
  whether that becomes an abstraction.
- Per-edit choice. The dialog answers for the whole set, which is what a set
  written in one pass can support.
- The frozen-link case. A dead link already refuses the send at `apply_parked`
  and swallows the writes at `take_pane_writes`, which is a pre-existing gap and
  not this frame's to close.
