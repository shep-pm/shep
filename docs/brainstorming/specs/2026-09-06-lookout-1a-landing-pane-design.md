# Design: the lookout landing pane

Status: designed 2026-09-06.

The first of the panes in
[the Claude Design bundle](../../lookout/design-files/README.md), and the one
every other pane borrows from: the bands, the selection gutter, the shared
cell functions and the status bar all land here and get reused by 1d, 1e, 1g,
1j, 1k and 1l.

Read [rulings.md](../../lookout/design-files/rulings.md) alongside the frames.
The frames are concepts and several of their behaviour claims are false of
shipped shep. Where this spec and frame 1a disagree, this spec is the one to
build.

## The problem

The landing pane draws four regions in plain text: the flock table, a host
strip, a sheep detail band and the bleats feed. Nothing on it carries
magnitude. A sheep at 2% CPU and a sheep at 90% differ by a number in a
column, and a sheep about to trip its memory ceiling looks exactly like one
using a tenth of it.

Nothing on it carries mode either. `shep lookout` in read-only, in a filter,
and frozen after the link died all draw the same chrome, with the difference
stated in words somewhere on the screen.

## What already exists

**The table.** `view/flock.rs` builds `Line`s directly rather than through
`ratatui::widgets::Table`, with fixed column widths, because "a live table
whose columns resize as a pid gains a digit is a table that shivers"
(flock.rs:3). Twelve columns in `ALL`, and a `TIERS` ladder that sheds the
least diagnostic first: CFG at 122 columns, then SMIT, FOLD, EXIT, RESTARTS,
PID, MEM, CPU, down to an ID/NAME/STATUS floor at 41.

Dogs already sit in their own section under a `Dogs` header, from decision 1
of [the pane design pass](2026-09-05-pane-design-pass.md). The frames' `██
FLOCK` and `██ DOGS` bands replace those headers rather than introducing the
grouping.

**The palette.** `theme.rs` carries four roles, meadow, bark, butter and
ink-3, over three tiers: truecolor-ish indexed for a 256-colour terminal, the
named ANSI colours for 16, and a flattened palette under `NO_COLOR`. Four
tests pin it, including `the_anstyle_binding_agrees_with_this_ones_colours`,
which cross-checks against the CLI's separate colouring so the TUI and `shep
flock` cannot drift.

It has never painted a background. The only style constructor is
`fg(Option<Color>)` (theme.rs:133), and the module doc states the reason:
"`--paper` is never painted: it would fight the operator's own terminal
background, so ordinary text stays `Color::Reset`" (theme.rs:7).

**The selection marker.** `mark()` returns a plain ASCII `>`, and its doc
records why it is not `▸`: "East-Asian *Ambiguous* width, and a terminal that
renders it double-wide would shift every column of that one row by a cell"
(flock.rs:46). A test pins it at one column in both states.

**The gallery.** `frames.rs` holds 33 scenes rendered through one text and
ANSI dump, pinned by snapshot tests and written to `docs/lookout/frames.txt`
and `frames.ansi` by an ignored test. `sgr()` emits foreground only and never
reads a cell's background.

**Nothing keeps history.** The daemon holds one CPU baseline per pid
(`limits/stats.rs:53`), overwritten each tick, and lookout replaces the whole
flock map on every snapshot (`app.rs:1335`). The poll is two seconds
(`link.rs:29`).

## Decision 1: the sparklines ship here, and the header states their real span

The `CPU` column gains a sibling holding a ten-cell sparkline, and the host
strip gains an eight-cell one for the flock total. A `VecDeque<f32>` per sheep
id, capped at 140, pushed from the existing two-second poll and pruned when a
sheep leaves the snapshot. One more series, flock-wide, for the strip.

The column's header is `CPU 20s`, not the frames' `CPU 60s`. Ten cells at one
sample each on a two-second poll is twenty seconds.

**Why 140 rather than 10.** 1d's charts want a six-minute window over the same
data. Sizing the buffer for them now costs 560 bytes per sheep and means 1d
inherits a filled buffer instead of starting cold on a pane an operator just
opened.

**Why the honest header.** The design's own second rule is that every
measurement states its denominator, and `CPU 60s` over twenty seconds of
samples breaks it on the pane that introduces the rule.

**What this does not fix.** `cpu_percent` is a delta against a baseline the
daemon resets every fifteen seconds (`limits/mod.rs:33`), so it is an average
over a window that cycles rather than a reading at an instant. The sparkline
inherits that. Aggregating it into true fixed-width buckets is 1d's problem,
and 1d has to say what it did on its axis.

On a fresh start the column draws blank until ten polls have landed. Blank,
not a short line: a two-sample sparkline reads as low CPU rather than as no
data.

## Decision 2: fourteen columns, and the margin goes

`ALL` grows from twelve to fourteen: the two new block columns join, and EXIT,
CFG and SMIT stay. `NAME` keeps taking the remainder, so the frames' fixed
24-cell name column and their 33-cell right margin both go.

| Column | Width | |
|---|---|---|
| gutter | 2 | |
| ID | 4 | unchanged |
| NAME | remainder | 44 cells at 160, against 66 today |
| STATUS | 15 | unchanged |
| CPU 20s | 11 | new |
| CPU | 6 | unchanged |
| MEM/CEIL | 11 | new |
| MEM | 8 | unchanged |
| PID | 7 | unchanged |
| RESTARTS | 8 | unchanged |
| EXIT | 9 | unchanged |
| CFG | 4 | unchanged |
| UPTIME | 8 | unchanged |
| FOLD | 10 | unchanged |
| SMIT | 13 | unchanged |

Fixed total 116, against 94 today.

**Why keep the three the frames drop.** All three are shipped, tested columns.
CFG is the one whose loss costs most: it marks every row that has drifted from
its Flockfile, and the frames move that signal into the detail band, which
shows one sheep at a time. An operator would have to select each row in turn
to learn what the column says at a glance.

**Why the margin goes.** Thirty-three empty columns are thirty-three columns
`NAME` is not getting. The remainder mechanism already ships and already
degrades correctly.

## Decision 3: the ladder gains two rungs and nothing below them moves

`TIERS` keeps every existing threshold and its tests untouched. Two rungs go on
top:

- 144 and above: everything.
- 133: drop `MEM/CEIL`, keeping the `MEM` number.
- 122: drop `CPU 20s`, keeping the `CPU` percentage.
- 122 and below: today's ladder, verbatim.

122 is where the current ladder starts, so the two new rungs restore exactly
today's table before any existing column is shed. That also matches what the
frames ask for, the gauge going before the sparkline and both before the old
columns.

## Decision 4: bands are reverse video, rows are painted

`Palette` gains four roles beside the existing four: `sky` for memory, `line`
for rules and dividers, `paper2` for a painted row ground, and `gauge_rest`
for a gauge's unfilled tail. It gains two style constructors:

- `band(role)`: the role as foreground plus `REVERSED`. No background is ever
  named, so the terminal supplies the text colour from its own background and
  a light terminal is right for free.
- `ground(role)`: a real background, used only by the selected row and the
  status bar.

**Why two mechanisms rather than one.** A band wants the loudest possible
inversion and does not care what the text colour is. A selected row wants a
quiet ground that is legible under ordinary text, which reverse video cannot
give. Painting a background at all reverses the rule at theme.rs:7, and that
comment gets rewritten to say what is now painted and why the ordinary ground
still is not.

## Decision 5: `NO_COLOR` keeps the reverse video

`NO_COLOR` drops every colour as it does today and keeps `REVERSED`. Bands
survive as inverted rows, so the design's first rule, that a band names the
mode, still works on a monochrome terminal.

**Why.** `NO_COLOR` is about colour, and SGR 7 is a modifier. Under it the
butter editing band and the meadow looking band become the same inverted band,
distinguished by the words in them, which the design's third rule already
requires to be true.

The selected row loses its ground, since that one is a real colour, and falls
back to the ASCII marker (decision 6).

## Decision 6: the selection edge is a painted space, and the block glyphs ship anyway

The gutter draws a space with a butter background, exactly one column on every
terminal, falling back to `mark()`'s ASCII `>` under `NO_COLOR`.

Everything else in the vocabulary uses the block glyphs unconditionally.
Twenty-one of the twenty-nine glyphs the frames name are East-Asian Ambiguous,
including every sparkline step, the gauge fill and the rules, and a doubled
cell shifts every column to its right.

**Why ship them anyway.** The design does not exist without them, and every
comparable tool draws them unconditionally. The `▸` rejection at flock.rs:46
was a marker with a free ASCII alternative; a sparkline has none.

**Why the marker still gets the strict treatment.** It is the one place where
an alternative exists, so it keeps taking it, and flock.rs's rule and its test
survive unedited.

If a real report ever arrives, the fix is an explicit override that drops the
two block columns through the ladder, leaving the percentage and the RSS number
that already say the same thing. Not built now.

## Decision 7: `MEM/CEIL` measures against the ceiling, which joins the wire

`max_memory` is not on `ProcessInfo`, so the client does not know any sheep's
ceiling today. It joins as `Option<u64>` with `serde(default)`, populated by
the daemon from `AppConfig`.

The gauge draws RSS against it, butter at or above 90%, with the unfilled tail
in `gauge_rest`. A running sheep with no ceiling draws all `░` in ink-3, the
same as a stopped one, and its `MEM` number carries the row.

**Why the ceiling and not the host.** The gauge answers how close this is to
the threshold that restarts it. Against host memory it would answer a
different question, and every row would need its denominator restated.

**What this costs.** `max_memory` defaults to `None`, so on a flock where
nobody has set ceilings the column is empty on every row. That does say plainly
that no ceiling is set, which is worth knowing, but it is a column of nothing
until someone configures one.

## Decision 8: the shared header vocabulary stays, and the test widens

Lookout keeps `RESTARTS`, `MEM` and `CPU`. The frames' `RST`, `RSS` and `%` do
not ship.

`the_full_column_set_matches_flock_rows_headers_exactly` (flock.rs:605) becomes
a subset check: every header lookout shares with `shep flock` matches it
exactly, and lookout may carry columns `shep flock` has no way to draw. The two
new columns are block glyphs over retained samples, and `shep flock` is a
static listing with no history and no cell to put one in.

**Why not rename in both.** `shep flock`'s headers are public output that
scripts read, the CLI reference would need regenerating, and `RSS` is a Unix
term that means nothing on Windows.

## Decision 9: gauges take colour, the stream tag does not

The host strip's load gauge is butter and its memory gauge is sky. The bleats
feed's `out` and `err` tags stay muted, as they draw today.

**Why the split.** Two gauges side by side on one line need telling apart, and
that is meaning rather than decoration. host.rs:22 argues that colour on that
line "would be decoration with no meaning behind it", which was written when
the line held only words; that comment gets extended to say why a gauge is
different. bleats.rs:93's argument is narrower and still holds: "the word
carries the whole meaning, and a red `err` would say a stderr line is damage".

## Decision 10: the feed does not parse timestamps or levels

The feed gains the `BLEATS` chip and keeps the provenance line it already
prints. It does not split a line into timestamp, level word and message.

**Why.** A bleat is whatever the app wrote. Most runtimes emit lines with no
timestamp and no level, the frames never say what an unparseable line renders
as, and a format guess that misses on two thirds of lines is worse than leaving
the text alone.

## The rest of the pane

**Host strip.** Keeps its single line and its truncate-from-the-right drop
order, which is the only drop mechanism it has. Gains the two gauges, the
eight-cell flock sparkline, and a right-hand `N errored · N parked`. Both
counts come from data already on the wire, summed the way the strip already
sums CPU and memory.

**Detail band.** The two path lines merge into one with a `│` divider in the
`line` role, plus the log's size on disk, which is a new `fs::metadata` call
per path. When the row does not fit, the size goes first, then the paths
truncate from the left, since a log path's tail is the half that identifies it.
Gains the `SHEEP N` chip and the `cfg !N pending` cell.

**Status bar.** Keys in butter over a `paper2` ground, and the right-aligned
control indicator, reusing the read-only string status.rs already carries.

## New code

`view/cell.rs`, four pure functions returning `String`, no ratatui types:
`gauge(value, ceiling, cells)`, `sparkline(&[f32], cells)`, `rule(cells)` and
`band(label, cells)`. They follow the module's existing convention of building
text by hand rather than reaching for `Sparkline` or `Gauge`, both of which
ratatui 0.30.2 offers and this module deliberately never uses.

`frames.rs`'s `sgr()` gains a background and `REVERSED` branch. Without it the
generated `frames.ansi` renders every new band as plain text, and the gallery
the design wants to replace these mockups with would be wrong about the one
rule the design calls its acceptance criterion.

## Wire

One additive field, `ProcessInfo.max_memory: Option<u64>`, `serde(default)`.

Neither `PROTOCOL_VERSION` nor `SCHEMA_VERSION` moves. `instance` and
`handshook` set the precedent for an additive `ProcessInfo` field, and the
envelope's own rule is that only a rename, a removal or a retype bumps it. The
2026-09-04 decision that bumps for additive variants covers a new `Request` a
running CLI sends on an ordinary path, which this is not.

`request_wire_v4` and `reply_wire_v4` regenerate.

## Out of scope

- 1d's charts, and the aggregation that would make a fixed-width sample bucket.
- Any new `Request`. Nothing here needs one.
- The overlay machinery 1g and 1k want. No pane in 1a is an overlay.
- An ambiguous-width override or terminal probe (decision 6).
- Timestamp and level parsing in the feed (decision 10).

## Testing

- `cell.rs`: each of the four functions against literal strings, including a
  zero value, a value at the ceiling, one above it, and a `None` ceiling.
- `theme.rs`: four tests beside the existing four, covering the two new
  constructors at both tiers and `REVERSED` surviving `NO_COLOR`. The `anstyle`
  cross-check extends to the new roles.
- `flock.rs`: the widened header check, the two new ladder rungs, and the
  gutter at one column in both the painted and the `NO_COLOR` case.
- `frames.rs`: new `Scene` entries for the redesigned pane, and the gallery
  regenerated so `docs/lookout/frames.txt` and `frames.ansi` cover it.

## Docs

`docs/lookout/README.md`'s drop-order sentence is rewritten. It is already
stale: it omits `CFG`, which is the first column the shipped ladder sheds.

The `web/` trigger applies, since the table an operator sees changes. The CLI
reference regenerates and `web/src/pages/docs/*.astro` get read for anything
naming the flock table's columns.
