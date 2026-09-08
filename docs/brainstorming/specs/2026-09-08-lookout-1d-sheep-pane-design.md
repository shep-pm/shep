# Design: the sheep pane

Status: designed 2026-09-08.

Frame 1d from
[the Claude Design bundle](../../lookout/design-files/README.md), fourth in the
build order and the first pane to draw a chart. Everything one sheep has on
one screen: two histories on a shared time axis, the process facts, the
config, the env keys, and a tall feed.

Read [rulings.md](../../lookout/design-files/rulings.md) alongside the frames.
Where this spec and frame 1d disagree, this spec is the one to build. Where
this spec and rulings.md disagree, this spec is newer: rulings.md is corrected
in the same commit, and the corrections are listed at the bottom.

## The problem

Opening a sheep today gives a two-row detail band under the flock table. It
carries the facts and nothing else. To see whether a sheep's memory has been
climbing for five minutes or spiked once, an operator watches the table and
remembers.

The frame answers that with two charts, and the charts have nowhere to read
from. Nothing in shep keeps a series.

## What already exists

**A CPU ring buffer, client side.** Frame 1a shipped `cpu_history:
HashMap<u32, VecDeque<f32>>` on `App` (app.rs:1415), 140 samples deep
(`HISTORY`, app.rs:1287), one sample per poll, dropped entirely when a sheep
leaves the flock. Its own doc comment says it was sized for this pane. There
is no equivalent for memory.

**The sample is not what it looks like.** The flock poll is two seconds
(link.rs:29). The daemon's `cpu_percent` divides a CPU counter by the wall
time since a baseline, and that baseline is written only by the fifteen second
enforcer tick (limits/mod.rs:122, `MEMORY_POLL_INTERVAL` at limits/mod.rs:33).
So consecutive polls share a baseline and each reading is a running mean over
a window that grows from two seconds to fifteen and then resets.

A sheep that burns one core for one second, right after a reset, then idles:

```
t (s)      2    4    6    8   10   12   14  | 16   18
reads %   50   25   17   13   10    8    7  |  0    0
truth %   50    0    0    0    0    0    0  |  0    0
                                reset at 15s ^
```

Buffered as they arrive, those readings draw a fourteen second decay ramp for
a one second spike, and a cliff at every reset. 1a's sparkline has this today.

**A config pane.** `ConfigPane` (pane.rs:734) is built from a
`SheepConfigView` fetched by `Request::SheepConfig`, walks the eight groups in
`GROUP_ORDER` (config/scaffold.rs:85), and marks fields that are pending or
overridden. It carries an `EnvPane` (pane.rs:295) holding key names.

**A feed pane.** `BleatsPane` (pane_bleats.rs:238) owns the three filter axes,
follow, wrap and level cycling, and reads the log files rather than the bus.

**A gauge and a sparkline.** `cell::gauge` and `cell::sparkline`
(view/cell.rs:68), both unit tested against strings. `mem_ceil_cell`
(view/flock.rs:1205) refuses a guessed denominator: with no `max_memory` it
draws a muted empty bar rather than inventing a ceiling.

**Nothing samples a dog.** `stats.watch` has one call site, on the sheep
arming path (extras.rs:455), and `dogs.rs` has no stats wiring. A dog row
comes back with `cpu_percent` and `memory_bytes` both `None`.

## Decision 1: the daemon publishes its counter, and lookout differences it

`SheepStats` and `ProcessInfo` gain `cpu_ms: Option<u64>`, the cumulative tree
counter `sample_now` already computes as `observed_cpu_ms` (limits/stats.rs:174)
and throws away. `ProcessInfo::builder` gains a `cpu_ms` setter.

Lookout differences consecutive polls. Between two readings it has the CPU
milliseconds spent and the wall time elapsed, which is the true mean over
those two seconds and nothing else. The 15s baseline stops mattering, and the
table above collapses to its `truth` row.

The percent formula moves out of `limits/stats.rs:284`, where it is private,
into shep-core, so the daemon's `cpu_percent` and lookout's differenced series
cannot drift apart about what a percent of one core means.

Four edges, each with a test:

- **The first reading for a sheep** records the baseline and appends nothing.
  There is no sample behind it to slide, so a short buffer is honest where a
  zero would not be.
- **`cpu_ms` of `None`** appends a zero and clears the stored reading, so a
  stop is never differenced across. This keeps 1a's existing rule that a
  missing reading is a zero rather than a gap.
- **A counter that went backwards** clamps to zero, the same `saturating_sub`
  the daemon already applies, and for the same reason: the tree under the pid
  is not the one the last reading came from.
- **A respawn** is exactly that case, so it costs one dropped sample.

Alternatives considered. Keeping the raw readings and stating the window in
the axis label was cheaper, but the sentence explaining the decay ramp is
longer than the fix and the chart still draws a shape the process never had. A
daemon-side ring buffer filled at the enforcer tick survives a lookout restart
and serves every client the same series, but its resolution is fifteen
seconds, so six minutes is 24 points across a 140 cell chart body.

## Decision 2: the window is the chart's own width, stated where it is drawn

The frame says `six minutes, 5s samples`. Neither is true and neither can be
made true by a constant. One cell per poll at two seconds makes 140 cells
worth 4m40s, and the chart body is 140 cells only on a 160 column terminal.

So the window is computed at draw time from the cells actually available, and
rendered into both the section header and the axis line. At 160 columns the
header reads `4m40s, one 2s sample per column`. At 140 columns the body is 120
cells and it reads `4m00s`.

The drawn body is capped at `HISTORY`, so a terminal wider than 160 grows the
right margin rather than leaving cells that can never fill.

The buffer starts empty on every launch and dies with the process, so the pane
says so: while the buffer is shorter than the body, the chart pads on the left
and the header reads `collecting · 1m10s of 4m40s`. The newest cell is up to
one poll stale, which is why `now` labels a column rather than an instant.

No `w` key. With one cell fixed at one poll there is nothing to widen to, and
`w` is already `WrapToggle` in the feed the pane embeds.

## Decision 3: each chart names its own ceiling

**CPU** scales to the highest sample in the drawn window, floored at
`CPU_CEILING_FLOOR` (app.rs:1296) and rounded up a 1-2-5 ladder so the gutter
labels land on round numbers. Not the flock ceiling `App::cpu_ceiling`
(app.rs:4118) computes: that exists to make rows comparable down a table, and
this pane has one sheep and no column to be comparable with. The cost is that
`J` and `K` rescale the chart, which is why the gutter carries numbers and the
shape alone is never the message.

**Memory** scales to `max_memory` when the sheep has one, rounded up the same
ladder so the limit sits below the top of the chart with headroom, and the row
nearest the limit draws `╌` in butter with `ceiling` in the right margin.

With no limit set there is no ceiling row, the scale top is the window peak
rounded up, and the header states the substitution: `no limit set · scaled to
peak 48.3M`. This follows `mem_ceil_cell`'s existing refusal to guess a
denominator, and satisfies the frames' second design rule, that every
measurement states what it is measured against.

## Decision 4: the config grouping comes out of `ConfigPane`

1d's left column and 1e's editing pane list the same fields in the same eight
groups with the same pending and overridden markers. The walk that produces
them moves into a function over `&SheepConfigView` returning grouped rows.
`ConfigPane` keeps its cursor, its edit buffer and its sub-screens on top of
that function; 1d renders its output flat and read only.

Not by holding a real `ConfigPane` and drawing it without its cursor. The pane
is 2,585 lines built around editing, and 1d would carry an edit buffer and an
`EnvPane` it never uses. Not by writing a second listing either: group order,
the `!` marker and the `(unset)` and `(default)` rendering would then exist
twice, which is the pair that drifts.

## Decision 5: the view says which env keys are sealed

`SheepConfigView::new` (request.rs:1404) clears `env` before the struct is
built, so no pane can show a value. It also cannot currently show which keys
resolve from the secret store, because `env_keys` is a bare `Vec<String>` and
`EnvPane` holds the same.

`secrets::references` answers the other direction: given a config it returns
the secret names, deduplicated across `env`, `args`, `out_file` and
`err_file`. For `DB_URL = "postgres://u:{{secret:pg/PASSWORD}}@h"` it says
`pg/PASSWORD`, and 1d needs `DB_URL`.

So a sibling helper lands beside it:

```rust
/// The env keys whose value names at least one `{{secret:...}}` reference,
/// in `env`'s own order. The keys, never the references: [`references`]
/// answers the other direction.
pub fn sealed_keys(config: &AppConfig) -> Vec<String>
```

`SheepConfigView::new` calls it on the line above `config.env.clear()` and
stores the result as `env_secrets`. `config::template::walk` is `pub(crate)`
and both live in shep-core, so nothing changes visibility. The type is
`#[non_exhaustive]` with `new` as its only constructor, so the field is
additive for out of tree consumers.

1d draws a sealed key as a butter block run with the word `sealed`, and a
Flockfile key plain. 1e inherits the same distinction, which its own frame
asks for and could not have.

## Decision 6: the feed is a `BleatsPane` at 83 cells

1d owns a `BleatsPane` scoped to the selected sheep and renders it into the
right column. The frame's header line advertises exactly what that pane
tracks, so the filters, the level cycling and the stream toggle all work
inside 1d, and `b` hands the same pane the whole screen.

The alternative, a bare tail reader with filtering left to `b`, would need the
header copy changed to stop promising a filter that is not there.

## Decision 7: `J` and `K` become `StepUp` and `StepDown`

The two keys map to `KeyPress::ListMoveUp` and `ListMoveDown`, inert
everywhere except inside a config pane's list editor (app.rs:2510,
app.rs:3547). The variants get renamed for the key rather than for one pane's
use of it. `map_key` is untouched and still dispatches on mode, not on pane;
the body decides what a step means. A config pane reads it as a list reorder,
1d as the previous or next sheep in the current sort and filter.

Six match arms, mechanical, and the same shape `d` and `ListRemove` already
have.

`↵` is `KeyPress::Confirm` (input.rs:81), so it opens 1d only when no action
is armed. An armed `x`, `R` or `L` prompt keeps it, which is the existing
confirm contract and not a new rule.

The full feed opens on `b`, the key it shipped on. The frame's `l` is a copy
error of the kind rulings.md already lists several of.

## Decision 8: the responsive ladder, with the arithmetic written down

```
160 = 8 gutter + 140 body + 12 margin       body = min(width - 20, HISTORY)
160 = 76 config + 1 divider + 83 feed
```

| Width | Draws |
|---|---|
| 140 and up | both charts, body `min(width - 20, 140)` cells, window `body × 2s` |
| 100 to 139 | CPU chart only; memory becomes one line, `rss 48.3M of 52M` and `cell::gauge` at 10 cells |
| under 100 | 1a's `CPU 20s` sparkline and `MEM/CEIL` gauge on one row |
| under 31 | refused, as today |

Rows: the charts hold rows 2 to 17. Under 26 rows the memory chart goes; under
20, both. The config and feed columns are what the pane is for, so they are
last to give ground.

Every gallery scene states its own width sum in a comment and asserts it is
wide enough for the tier it exercises. A scene one cell short of its own
column set drops the thing it was written to show, silently, which is how 1a's
gauge went missing from a scene once.

## The rest of the pane

Row allocation at the 160×48 design target:

| Rows | Content |
|---|---|
| 0 | title band, meadow |
| 1 | identity band: the chip, the facts `view/detail.rs` already renders, and `!N pending` from the config view |
| 2 | `██ CPU` label and the header from decisions 2 and 3 |
| 3 to 10 | CPU chart, 8 rows, 16 half steps |
| 11 | `██ MEM` label and its header |
| 12 to 16 | memory chart, 5 rows |
| 17 | shared x axis, `now` on the last column |
| 18 | hairline rule |
| 19 | column headers, `██ CONFIG & ENV` left and the `BLEATS` chip right |
| 20 to 45 | the two columns |
| 46 | blank |
| 47 | status bar |

Both charts are drawn by one function beside `gauge` and `sparkline`:

```rust
/// 16 half steps in 8 rows. `h = round(v / ceiling * rows * 2)`; for row
/// `r` counted from the top, `s = h - (rows - 1 - r) * 2`; the cell is `█`
/// when `s >= 2`, `▄` when `s == 1`, blank otherwise.
pub fn chart(samples: &[f32], ceiling: f32, cols: usize, rows: usize) -> Vec<String>
```

Both are the same width over the same window, so a memory step and a CPU spike
line up vertically. That alignment is why this frame was picked over
side by side charts.

Status bar: `esc flock`, `e edit`, `b full log`, `J/K next sheep`, `/ filter`,
`x stop`, `R restart`, `L reload`, with 1a's right aligned control marker.

Config is fetched on open, on `r`, and on `J` or `K`, never on the two second
poll. The pending count can be one action stale, which is what the shipped
config pane already does.

## New code

| Where | What |
|---|---|
| `shep-core/src/secrets.rs` | `sealed_keys` |
| `shep-core/src/protocol/request.rs` | `SheepConfigView::env_secrets`, `ProcessInfo::cpu_ms` and its builder setter |
| `shep-core` | the percent formula, moved out of the daemon |
| `shep-daemon/src/limits/stats.rs` | `SheepStats::cpu_ms` |
| `shep-daemon/src/rpc.rs` | carry it through `with_live_stats` |
| `lookout/view/cell.rs` | `chart` |
| `lookout/pane.rs` | the grouping function, extracted |
| `lookout/app.rs` | `rss_history`, the stored last reading, `Body::Sheep`, a discriminator on `Sent::SheepConfig`, the two renamed arms |
| `lookout/input.rs` | the rename |
| new `lookout/pane_sheep.rs` and `lookout/view/sheep.rs` | the pane and its rendering |
| `lookout/frames.rs` | a scene per responsive tier |

`Sent::SheepConfig` needs the discriminator because `on_sheep_config` sets
`Body::ConfigPane` unconditionally. Without it, `e` pressed inside 1d comes
back and fills 1d instead of opening the editor.

## Wire

Two additive fields, `ProcessInfo::cpu_ms` and `SheepConfigView::env_secrets`.
Neither is a rename, a removal or a retype, so `PROTOCOL_VERSION`,
`MIN_SUPPORTED` and `SCHEMA_VERSION` all stay where they are.

`ProcessInfo` is the `--json` envelope, pinned by
`the_json_envelope_shape_is_pinned.snap`, so `cpu_ms` appears in `shep list
--json` and the snapshot changes with it. That is a payload shape change, and
it fires the docs trigger on its own.

It reaches further than the envelope, and this paragraph undercounted it until
Task 1 found out. Every `ProcessInfo` field has to be either a table column or
listed in `output/rows.rs`'s `JSON_ONLY` drift guard, whistle's `SheepRow`
mirrors the type under a schema test, and `tests/cli_e2e.rs` compares against
three committed fixtures. So `cpu_ms` lands on the MCP surface too. Forced by
guards that already existed rather than chosen, and additive in both places.

## Out of scope

- **Dogs.** `↵` on a dog row does what it does today, and `e` still opens the
  dog config pane. The daemon does not sample a dog, so there is nothing to
  chart, and a dog's config arrives as a TOML section rather than a
  `SheepConfigView`. Whether to sample dogs at all is a daemon change and
  wants its own design.
- **A `w` window key**, per decision 2.
- **A secrets jump.** 1h binds `S` on its own branch and does not touch
  anything in this spec. 1d gains `S secrets` in its status bar when that
  merges.
- **1e and 1g**, which follow this pane as a pair.

## Testing

Every test in this pane gets the mutation check before it is called done:
break the thing it claims to pin, watch it fail, put it back, then ask what
else could have made it pass. Six tests shipped on the last two panes that
pinned nothing, two of them passing on text that was present for an unrelated
reason.

- `chart` against strings, the way `gauge` and `sparkline` are tested: a
  column at each of the sixteen half steps, a value over the ceiling, an empty
  series, a flat series, and a series longer than the columns given.
- The differencing, one test per edge in decision 1, plus one that drives two
  polls at a known counter and interval and asserts the percent.
- `sealed_keys` over a value that is entirely a reference, one that embeds a
  reference in a longer string, one with a namespace, and one with none.
- `SheepConfigView::new` keeps `env_secrets` after clearing `env`.
- Each responsive tier as a gallery scene, with its width arithmetic in a
  comment and an assertion that the scene is wide enough for what it shows.
- The window label at two widths, since a literal would pass at one of them.
- `J` and `K` step the sheep in 1d and reorder a list in a config pane, from
  the same key.

## Docs

`web/` is in scope for this change: the JSON payload gains a field and the
lookout keymap gains a pane.

1. `cargo build --release`, then `./web/scripts/generate-cli-reference.sh`,
   then read the diff.
2. Grep `web/src/pages/docs/*.astro` for the JSON envelope's fields and for
   the lookout key list.
3. `cd web && npx astro build`, then `npx astro check`. Both, because `check`
   is the one that catches a wrong prop.

`docs/lookout/frames.txt` regenerates from the new scenes through the ignored
`write_the_gallery` test.

## Corrections to rulings.md

Made in the same commit as this spec.

- **1h no longer waits.** The store shipped. `shep-core/src/secrets.rs` is
  real, env values carry `{{secret:...}}` references, and `AppConfig` has a
  field naming the namespace a sheep resolves them in. The pane itself is in
  flight and binds `S`, not the frame's `g`.
- **The charts had a decision to make and it is made**, in decisions 1 through
  3 above.

## Corrections to frame 1d

- The poll is 2s, not 5s, and the window is 4m40s at 160 columns, not six
  minutes.
- The group list has eight entries, not seven. `cron` is missing from the
  frame and `restart` sits fourth, not second: `process, logging, inputs,
  restart, readiness, shutdown, watch, cron`.
- Env values are never shown. The wire clears them.
- The full feed is `b`, not `l`. There is no `w` and no `g`.
