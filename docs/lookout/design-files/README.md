# Handoff: `shep lookout` pane redesign

> Read [rulings.md](rulings.md) first. These frames are concepts, and that
> file records which of them are being built, where the copy and the
> behaviour claims below are wrong about shipped shep, and what changed
> after the maintainer reviewed them.

## Overview

`shep lookout` (alias `dash`) is the terminal dashboard over the shepherd, built with ratatui in `crates/shep-cli/src/lookout/`. Today it draws four plain panes: the flock table, a host strip, a sheep detail pane and the bleats feed. This handoff covers a redesign of every pane plus five new ones, in a glyph vocabulary of half-blocks, shade blocks and reverse-video bands.

Nine frames are specified. All were drawn at **160×48 character cells**, which is the design target; smaller tiers degrade by the rules in "Responsive behavior".

| id | Pane | Status |
|----|------|--------|
| 1a | Landing — flock table, host strip, detail band, bleats | picked over an alternative |
| 1d | Sheep/dog pane — histories, config, env, feed | picked over an alternative |
| 1e | Editing pane — field list + explanation panel | picked over an alternative |
| 1g | Close dialog — restart / reload / continue | single proposal |
| 1h | Secrets — the KV store | single proposal |
| 1i | Bleats, full screen | single proposal |
| 1j | Fold view | single proposal |
| 1k | Keymap overlay | single proposal |
| 1l | Frozen state — the shepherd has died | single proposal |

## About the design files

`Lookout Frames v2.dc.html` is a **design reference, not production code**. It is an HTML page that fakes a terminal: each frame is a stack of 48 flex rows whose cells are sized in `ch` units so the result is a true character grid. Nothing in it is meant to be ported — the job is to **reproduce these frames in ratatui**, in the existing `lookout` module, using its existing widgets, layout constraints and the `Palette` that is already there.

Read the HTML when you need an exact column count or an exact glyph: every row states its cell widths in `ch`, and every row sums to exactly 160. Opening it in a browser is the fastest way to see a frame whole.

`Lookout Frame Gallery (round 1, all options).dc.html` holds twelve frames — the nine above plus the three alternatives that were not picked (1b rail-and-panel landing, 1c two-chart sheep pane, 1f editing workbench). Keep it for context only; do not implement from it.

## Fidelity

**High fidelity.** Column widths, row allocations, glyphs, colour roles and copy are all final and should be reproduced exactly. Two deliberate exceptions:

- **Colour** is specified by *role*, not by hex. The frames render truecolor for legibility on screen; the implementation must go through the existing `theme::Palette`, which already resolves meadow / bark / butter / ink-3 to xterm-256 indices 29 / 166 / 221 / 245 and to the 16-colour names, and flattens under `NO_COLOR`.
- **Sample data** in the frames (`catcher`, `flaky`, `web ×3`, pids, timings) is illustrative. Only the *format* of each cell is normative.

## The design system these frames follow

Three rules govern every frame. They are the acceptance criteria for the whole redesign:

1. **A band names the mode.** One full-width reverse-video row at the top of every pane. Meadow ground = you are looking. Butter ground = you can change something (editing pane, secrets, and the close dialog's border). Bark ground = the link is gone. Nothing else in a frame changes colour wholesale.
2. **Every measurement states its denominator.** A gauge with no stated ceiling is decoration. `48.3M of 52M`, `5.12 / 14 cores`, `211 of 12,904 lines`.
3. **Blocks carry magnitude, words carry meaning.** Strip every glyph and colour and the frame still reads — which is exactly what `NO_COLOR` and a 16-colour terminal do. No cell may be colour-only or glyph-only.

Whimsy lives in the chrome (`flock`, `dogs`, `bleats`, `lambs`, `afield`, `whistle`) and never in the data columns (`pid`, `rss`, `exit`, `restarts`).

## Glyph vocabulary

| Glyph(s) | Codepoints | Use |
|---|---|---|
| `▁▂▃▄▅▆▇█` | U+2581–U+2588 | sparkline, one cell per sample, 8 steps |
| `█` `▌` `▏` `░` | U+2588, U+258C, U+258F, U+2591 | gauge: full, half, sliver, unfilled remainder |
| `█` `▄` | U+2588, U+2584 | chart body at half-block resolution (16 steps in 8 rows) |
| `▌` | U+258C | selection edge, butter, column 1 of the selected row |
| `██` | U+2588×2 | section band marker, e.g. `██ FLOCK` |
| `─ │ ┼` | U+2500, U+2502, U+253C | hairline rules and column dividers, always ink-3-dark (`#35493C` role: line) |
| `▛▀▜ ▌▐ ▙▄▟` | U+259B, U+2580, U+259C, U+258C, U+2590, U+2599, U+2584, U+259F | overlay box border (dialog, keymap) |
| `▲` `●` `□` `=` `!` | U+25B2, U+25CF, U+25A1 | change-impact marks: respawn / now / next start / read-only / edited |
| `↳` | U+21B3 | instance (lamb) under its group row |
| `↵` | U+21B5 | the Enter key in status bars |
| `≥` `·` `–` `×` | U+2265, U+00B7, U+2013, U+00D7 | filter operator, separator, unset, multiplier |

All are BMP and present in the common terminal fonts. No braille (rejected during the brief), no emoji, no glyph above U+2600.

## Colour roles

Semantic roles only — resolve through `theme::Palette`. Hexes are the truecolor reference values from `docs/shep-design`, given so a truecolor terminal can be matched exactly if `Palette` is ever extended.

| Role | Dark hex | 256 | 16-colour | Used for |
|---|---|---|---|---|
| meadow | `#59C47D` | 29 | Green | online, ready, lamb ready, `out` stream, control-enabled |
| bark | `#FF7B4F` | 166 | Red | errored, ERROR lines, refusal, destructive, frozen banner |
| butter | `#F6D072` | 221 | Yellow | attention, key caps, WARN, sealed values, editing mode, pending edits, selection edge |
| sky | `#78BEEC` | — (nearest 74/75) | Blue | memory only — RSS gauges, memory chart, dogs band |
| ink-3 | `#7E9186` | 245 | DarkGray | column headers, hints, `(unset)`, `(default)`, muted rollups |
| ink | `#F0ECDC` | — | White | the selected row's own text, focused field name |
| ink-2 | `#BFCFC2` | — | default fg | ordinary cell text |
| line | `#35493C` | 238 | DarkGray | rules and dividers |
| paper-2 | `#1B2A21` | 235 | — | selected-row ground, status bar ground, overlay interior |
| gauge remainder | `#2C3B33` | 236 | — | the `░` tail of a gauge, so only the filled part reads |

`--paper` is never painted, per the existing `theme.rs` note: ordinary ground stays `Color::Reset` so the operator's own terminal background shows through. The bands, the selected row and the status bar are the only painted grounds.

Sky is new to the palette — `theme.rs` currently carries four roles. Add a fifth (`sky`, 256 index 74, 16-colour `Blue`) or, if the four-role rule is worth keeping, render memory in ink-2 and let the word `mem` carry it; the frames assume sky exists.

---

# Screens

Row indices below are 0-based within the 48-row frame. Every row is exactly 160 columns.

## 1a — Landing pane

**Purpose.** The spine of the tool: the whole flock and both dogs, their live measurements, and the highlighted row's latest output.

**Row allocation (160×48)**

| Rows | Content |
|---|---|
| 0 | title band, meadow ground, ink-dark text |
| 1 | blank |
| 2 | host strip |
| 3 | hairline rule (160 `─`) |
| 4 | blank |
| 5 | column headers, ink-3 |
| 6 | `██ FLOCK` band + rule to column 160 |
| 7–15 | 9 flock rows (`catcher`, `flaky`, `hungry`, `report`, `web ×3` + three `↳` lambs, `worker`) |
| 16 | blank |
| 17 | `██ DOGS` band + rule |
| 18–19 | 2 dog rows |
| 20–35 | empty (the viewport's spare height; the table scrolls into it) |
| 36 | hairline rule |
| 37 | detail band: `SHEEP 10` chip on meadow + facts |
| 38 | `logs` row: out path, err path, size on disk |
| 39 | hairline rule |
| 40 | blank |
| 41 | `BLEATS` chip on butter + provenance line |
| 42–46 | 5 log lines for the highlighted row |
| 47 | status bar, paper-2 ground |

**Table columns.** Left to right, widths in cells, summing to 133 with a deliberate 27-column right margin:

| Cols | Column | Format |
|---|---|---|
| 2 | selection gutter | `▌` in butter on the selected row; `▌` in bark when that row is errored; else blank |
| 5 | `ID` | integer, or `–` for a group rollup row |
| 24 | `NAME` | name; group rows append ` ×N` in ink-3; instances render `␣↳ :N` in ink-3 |
| 12 | `STATUS` | the `ProcStatus` word, coloured by `Palette::status` |
| 12 | `CPU 60s` | 10-cell sparkline, last 10 samples |
| 8 | `%` | `0.0%`, or `–` |
| 12 | `MEM/CEIL` | 10-cell gauge against `max_memory`; butter when ≥90% of ceiling; the `░` tail in the gauge-remainder colour; all-`░` in ink-3 when not running |
| 10 | `RSS` | `48.3M`, or `–` |
| 10 | `PID` | pid, `N pids` in ink-3 on a rollup, or `–` |
| 8 | `RST` | restart count |
| 12 | `UPTIME` | `19h 28m` |
| 12 | `FOLD` | fold name or `–` |
| 27 | margin | empty on purpose — do not fill it |

The earlier draft carried a `LATEST BLEAT` column here; it was cut. Latest output belongs to the bleats pane alone, which follows the highlighted row.

There is no fleece pip beside `STATUS` — it was cut as redundant with the word.

**Host strip (row 2).** `host` in ink-3, then `load` + 10-cell butter gauge (load ÷ cores) + `5.12` + the two other averages in ink-3 + `/ 14 cores`; `mem` + 10-cell sky gauge + `14.8G` + `of 48.0G`; `flock cpu` + an 8-cell sparkline + percentage; `flock mem` + total; then a right-hand summary in ink-3 (`1 errored · 0 parked`).

**Section bands.** 7 cells of `␣██␣` in the section's colour (meadow for flock, sky for dogs), then the label, then a `─` rule in the line colour out to column 160.

**Detail band (rows 37–38).** A 14-cell `␣SHEEP 10␣` chip — meadow ground, ink-dark text — then `catcher`, the status word, then `pid`/`restarts`/`up`/`cpu`/`mem`/`fold`/`lambs` label-value pairs with labels in ink-3, and `cfg !2 pending` in butter when the sheep has edits awaiting a respawn. Row 38 is the log paths: an `out` label in meadow, the path, a `│` divider, an `err` label in butter, the path, size on disk in ink-3.

**Bleats (rows 41–46).** An 11-cell `␣BLEATS␣` chip on butter, then the provenance line the current implementation already prints, in ink-3: `catcher · out then err, end to end · re-read with each listing · 464 earlier lines not shown`. Each log line is a 6-cell stream cell (`␣out␣␣` meadow / `␣err␣␣` butter) then a 154-cell line: timestamp in ink-3, level word (WARN butter, ERROR bark, others default), message in ink-2.

**Status bar (row 47).** Paper-2 ground across 160. Keys in butter bold, labels in default: `q quit`, `j/k select`, `↵ open`, `e edit`, `/ filter`, `x stop`, `R restart`, `L reload`, `s settings`, `h help`; right-aligned `█ control enabled` in meadow, or `█ read-only` in bark.

## 1d — Sheep/dog pane

**Purpose.** Everything about one sheep: two histories on a shared time axis, process facts, config, env, and a tall feed.

| Rows | Content |
|---|---|
| 0 | title band, meadow: `shep lookout ▘▘ catcher` / `sheep 10 of 10 ▖ esc back to the flock` |
| 1 | identity band: 12-cell `␣CATCHER␣` chip on paper-2 in ink, then facts, then `!2 pending` in butter |
| 2 | `██ CPU` label (12 cells, meadow) + `%   six minutes, 5s samples · peak 34.0% at 00:34:10 · mean 9.4% · now 0.0%` |
| 3–10 | CPU chart: 8-cell axis label gutter (`40 ─`, `│`, `30 ─`, …), 140-cell chart body in meadow, 12-cell right margin |
| 11 | `██ MEM` label (sky) + `rss  same window · ceiling 52M, four sawtooth drops are the gc · now 48.3M` |
| 12–16 | MEM chart: same 8-cell gutter (`64M ─`, `52M ╌` in butter for the ceiling line, `48M ─`, `32M ─`, `0 ─`), 140-cell body in sky |
| 17 | shared x-axis: `6m ago … 5m … 4m … 3m … 2m … 1m … now`, `now` ending on column 160 |
| 18 | hairline rule |
| 19 | two column headers: left 76 cells `██ CONFIG & ENV   e edit  tab next group  2 pending`; `│`; right 83 cells `BLEATS` chip + `out then err · 464 earlier · [level≥warn] · / narrow` |
| 20–45 | left: config grouped by the `group` tag, a blank row before each group rule; right: 20+ log lines |
| 46 | blank |
| 47 | status bar: `esc flock`, `e edit`, `g secrets`, `l full log`, `/ filter`, `w window 6m`, `x stop`, `R restart`, `L reload` |

**Charts.** Half-block area charts, not line charts. For a column of value `v` against ceiling `max` over `rows` rows, the column's height in half-steps is `h = round(v / max * rows * 2)`; for row `r` counted from the top, `s = h - (rows - 1 - r) * 2`, and the cell is `█` when `s ≥ 2`, `▄` when `s == 1`, blank otherwise. The CPU chart is 8 rows (16 steps); memory is 5 rows. The ceiling row of the memory chart is drawn `╌` in butter and labelled `ceiling` in the right margin.

Both charts are 140 cells wide over the same window, so a memory step and a CPU spike align vertically. That alignment is the reason this direction was picked over side-by-side charts: keep it.

**Config listing.** Groups in the order `process, restart, logging, inputs, readiness, shutdown, watch`, each introduced by a group label + `─` rule in the line colour, with a blank row above it. Field rows are `name` in ink-3, value in ink-2, and a right-aligned ink-3 note where one helps (`fork`, `16 tries`, `100ms backoff`, `empty`, `fd 3 open`). A field with a pending edit is prefixed `!` and rendered butter, with `awaits respawn` on the right. `(unset)` and `(default)` are ink-3.

## 1e — Editing pane

**Purpose.** Change one sheep's config with the meaning and the consequence of each field on screen.

| Rows | Content |
|---|---|
| 0 | title band, **butter** ground, ink-dark text: `shep lookout ▘▘ editing catcher  (sheep config)` / `2 edits pending ▖ esc to close` |
| 1 | provenance: flockfile path, `overlay kv`, `sheep is online, pid 71578`; right column notes `nothing is written until you leave this pane` |
| 2 | group tabs: the active group as a paper-2 chip in ink, the rest in ink-3, then `tab next group   1…8 jump` |
| 3 | hairline rule, both columns |
| 4 | left header `FIELD … VALUE … LANDS`; right `FOCUSED` chip on butter + field name + type |
| 5–13 | field rows (left, 88 cells) and the explanation panel (right, 72 cells) |
| 14 | blank |
| 15 | `pending edits` group rule; right `VALIDATION` heading |
| 16–17 | the two pending edits, `old → new` in butter; right, validation bullets |
| 18–23 | `env` group rule and its keys; right, `NEIGHBOURS` |
| 24–44 | remaining fields / blank |
| 45 | legend: `▲ respawn needed by 2 edits · ● takes effect at once · = read-only, set it in the Flockfile · ! changed by you` |
| 46 | hairline rule |
| 47 | status bar: `esc close`, `j/k select`, `↵ change`, `space cycle`, `d back to default`, `u undo`, `tab group`, `h help` |

**Left column (88 cells).** Row layout: 1-cell gutter, 2-cell marker (`=` read-only, `!` edited, `~` complex/unset, else blank), field name, value, and a right-aligned impact tag. The selected row gets a paper-2 ground and a butter `▌` in column 1, and its name renders in ink.

Impact tags, from each field's own semantics:

- `● now` in meadow — applies on write
- `▲ respawn` in butter — needs the process to stop and start
- `□ next start` in ink-3 — only read at spawn (e.g. `autostart`)
- `read-only here` in ink-3 — `name`, `instances`; set in the Flockfile

**Right column (72 cells).** For the focused field only, and it does not move as the selection moves: the field's `blurb` verbatim from the `schemars` annotation, wrapped at ~66 columns; then label-value lines in ink-3/ink-2 for `now`, `default`, `example`; then the impact sentence (`▲ respawn  the sheep must stop and start again` / `pick the timing when you close the pane`); then `VALIDATION` bullets — meadow `█` for accepted forms, bark `█` for what is refused; then `NEIGHBOURS`, the fields this one interacts with.

**Env.** Keys set in the Flockfile show their value. Keys from the store show a butter block run (`████████████`), the word `sealed`, and `edit in g`. Sealed values never render as characters in this pane. A `+ add a key` row closes the list.

## 1g — Close dialog

Drawn over 1e, which stays legible: dim the whole pane to a single muted ink (`#2A3A31`) — including the title band, which drops to a desaturated green — so the dialog reads as a question about what you just did, not as a new screen.

The box is 86 cells wide, starting at column 38, rows 19–30. Border in butter using `▛▀▜ ▌▐ ▙▄▟`; interior ground paper-2; interior width exactly 84 cells.

```
 TWO EDITS NEED A RESPAWN    catcher is online, pid 71578
   max_memory and err_file only take hold when the process starts again.
   Everything else you changed is already live.

   R   restart now       stop, then start. About 1.6s of downtime.
   L   reload            one lamb at a time. No downtime, slower.
   c   continue          leave it running. The two edits wait.

   esc  keep editing       ·      this prompt expires in 7s ███████░░░
```

The heading is a butter chip; option keys are butter bold; option labels ink; consequences ink-3. Behaviour follows the apply-menu carve-out already documented in `docs/lookout/README.md`: because the dialog names its keys on screen, `R`, `L` and `c` act on the press rather than arming a confirm, and the prompt expires on the same ten seconds every other prompt gets. The countdown is redundant in words and glyphs (`7s` + a 10-cell gauge). `q` and Ctrl-C still quit. Status bar reduces to one ink-3 line: `the dialog owns the keyboard until it is answered or it expires`.

## 1h — Secrets

**Purpose.** Live editing of the KV store's key-values, which is where env secrets come from.

Butter title band (this is an editing mode). Row 1 states the store's own terms in ink-3: `store ~/.shep-play/kv.db · sealed at rest · never printed to a log, never carried in a bleat · the daemon reads it at spawn`. Row 2 is scope tabs — `flock-wide`, then one per sheep — because a key set on a sheep beats the flock-wide key of the same name, and the tab row is where that is said.

Table columns: 2 gutter, 26 `KEY`, 32 `VALUE`, 14 `SCOPE`, 16 `LAST SET`, 26 `READ BY`, 44 `LANDS`.

- A sealed value renders as a butter block run whose length is the value's length, never its characters.
- A revealed value renders in ink-2 with `visible for 6s` and a 10-cell countdown gauge in the `LANDS` column.
- A sheep-scoped key renders as `␣↳ catcher.SENTRY_DSN` under its flock-wide row, with `overrides the row above` in ink-3.
- `lookout.allow_control` appears here as an ordinary key with `● now` — it is this pane's own gate.
- A `+ new key` row carries an inline input chip (`NEW_KEY_█`) and the naming rule in ink-3.

Below a hairline: a `FOCUSED` panel (left 88) with the reveal's terms — `A reveal is written to the daemon's audit log with your uid and the time. The value leaves the screen on its own after ten seconds.` — plus `set` / `used` / `length`; and a `WHO READS IT` panel (right 72) listing each reader with a meadow `█` if it currently holds the value and ink-3 `░` if it will read it at next start.

Action row: `v reveal for 10s`, `↵ set a value`, `D delete the key`, `y copy to clipboard`, then `a reveal, a set and a delete are each recorded`.

## 1i — Bleats, full screen

Meadow title band with the line count. Row 1 names both files, their sizes, the 64 KiB window, and — in butter — `831K below the window, unread`, because bytes below the window were never read and the count of lines in them is unknown.

**Filter row (row 2).** Three *axes*, each a droppable chip on paper-2:

1. stream — `stream err` (out / err / both)
2. minimum level — `level ≥ warn`
3. text or regex match — `match /pool|slow/█`, the active input carrying the cursor

They compose with AND. The row states that: `all three must hold · 211 of 12,904 lines survive them · esc drops the newest chip`. `esc` removes the newest chip rather than clearing every filter.

**Log rows.** 8-cell line number in ink-3, 6-cell stream cell, 143-cell line (full ISO timestamp in ink-3, level word, message with matches highlighted in reverse butter), then a 3-cell **density gutter**.

**Density gutter.** The rightmost 3 cells of every row, for the whole frame height: one cell per screenful of the entire file — `░` in the line colour for a screenful of stdout only, `█` in bark where that screenful contains any stderr, `█` in meadow for the screenful you are in. Every row must carry the gutter, including empty ones, or it visibly jumps sides. The frame's last content row explains it: `density gutter  one cell per screenful of the whole file · █ holds stderr · █ is where you are`.

Status bar: `esc back`, `j/k line`, `ctrl-d/u page`, `G end`, `/ search`, `n/N match`, `f follow`, `w wrap`, `o out/err/both`; right-aligned `█ following`.

## 1j — Fold view

The same flock gathered by fold, reached with `F`. Meadow band, host strip, then rows grouped under fold headers: `edge ×4`, `asdfa ×1`, `no fold ×3`, and `dogs ×2` (dogs are never in a fold; the header says so).

Columns: 2 gutter, 24 `FOLD / NAME`, 12 `STATUS`, 22 `SHARE OF FLOCK MEM`, 10 `MEM`, 9 `CPU`, 10 `UPTIME`, 8 `RST`, 63 `NOTES`.

The share bar is a 20-cell gauge of that fold's part of total flock memory, with the percentage stated in the notes column (`71% of flock memory · shortest uptime shown`). A fold header sums restarts, CPU and memory and takes the **shortest** of its members' uptimes, matching the existing group-row rollup rule. Header rows render their numbers in ink; member rows in ink-2.

Row 45 states the consequence of the selection: `selected: fold edge · x stops all 4 · R restarts all 4 · L reloads all 4 · each one still arms a confirm first`. Fold-wide actions are the reason this view exists.

Status bar adds `z collapse fold` and `F flat list`.

## 1k — Keymap overlay

Drawn over 1a, dimmed the same way as 1g. Box 128 cells wide starting at column 17, rows 8–26, butter border, paper-2 interior of exactly 126 cells.

Four columns grouped by what the key does, not alphabetically: `MOVING`, `LOOKING`, `CHANGING` (meadow headings) and `DOING` (bark heading). The destructive three sit alone under `DOING` with the gate's state printed beside them — `each one arms, ↵ confirms, 10s to answer`, and `█ control enabled` in meadow.

Two ink-3 lines close the box: `colour is decoration only: every coloured cell says the same thing in words. NO_COLOR loses nothing but the colour.` and `h or ? closes this`.

Bottom-right of the interior carries the one decoration in the whole TUI that holds no information — a sheep, drawn in blocks, centred in a 38-cell cell:

```
  ▟█████▙
 ▟███████▙
 ▜██▀ ▀██▛
   ▀▘ ▀▘
```

Allowed exactly once, here.

## 1l — The shepherd has died

The frozen state after the retry ladder is spent.

- Row 0: bark band, ink-dark text: `THE SHEPHERD HAS DIED ▖ these values are frozen as of 00:41:33` / `nothing here is live ▖ q to quit`.
- Row 1: ink-3: `the dashboard stays up so you can read what it had · it will not exit on its own`.
- Rows 2–12: the host strip and the whole table in **one muted ink** (`#5C6B62`), including the status words — no cell may look live. The uptime column becomes `FROZEN AT` and stops advancing.
- Rows 30–34: a `THE LINK` chip on bark, then the ladder: `█ 250ms  █ 500ms  █ 1s  █ 2s  █ 4s` in bark with the failure in ink-3 (`connection refused on ~/.shep-play/shep.sock`), `since 00:41:33, four minutes ago`, `the sheep themselves may well still be running; lookout cannot see them to say`, and `r dials again now  ·  or start the shepherd from another shell: shep muster`.
- Row 47: `q quit`, `r retry the link`, `j/k still moves`, then `every other key is refused while the link is down`; right-aligned `█ frozen` in bark.

---

## Interactions & behavior

**Navigation.** `j`/`k` move the selection by a row, `g`/`G` jump to the ends, `↵` opens the selected sheep or dog (1a → 1d), `esc` goes back one level, `J`/`K` step to the next sheep without leaving 1d. `e` opens the editing pane from either 1a or 1d. `g` opens secrets, `l` opens the full-screen feed, `s` opens the existing daemon settings screen, `F` toggles the fold view, `h` or `?` toggles the keymap.

**Actions.** Unchanged from the shipped behaviour: `x`, `R`, `L` arm a confirm, `↵` confirms, any other key cancels, `q`/Ctrl-C still quit with a prompt up, an unanswered prompt expires after ten seconds, and `--read-only` / `lookout.allow_control = "false"` refuses outright with a literal sentence. The one exception is 1g's apply menu, which names its keys and so acts on the press.

**Filters.** `/` opens the name filter in 1a (existing behaviour: type to narrow, `↵` applies, `esc` cancels the edit, `esc` clears an applied filter, title carries `2 of 6 in the flock`). In 1i, `/` adds a match chip; the three filter axes AND together; `esc` drops the newest chip.

**Editing.** Nothing is written until the pane closes. `↵` opens a field's editor, `space` cycles an enum or bool, `d` restores the default, `u` undoes the last edit, `tab` moves to the next group. On close, if any pending edit is `▲ respawn`, show 1g; if every pending edit is `● now`, write and close with no dialog.

**Refresh.** The two-second poll and `r` are unchanged. The lambs line still comes from a separate `Request::Describe` on selection change and on `r`, and still carries its own age stamp. The bleats feed still re-reads the log files rather than subscribing to the bus, and still says so on its header line.

**Responsive behavior.** The existing degradation rules stand and extend to the new columns.

- Columns drop least-diagnostic-first at the widths already in `docs/lookout/README.md`; the new columns join that ladder before the old ones: the 27-cell right margin collapses first, then `MEM/CEIL` gauge (keeping `RSS`), then `CPU 60s` sparkline (keeping `%`), then `FOLD`, then the existing order.
- Panes drop before columns on short terminals: detail below 24 rows, feed below 18, host strip below 14, per the shipped rule.
- The 1d charts need 140 columns for the shared axis; below that, drop the memory chart first and keep CPU, and below 100 columns drop both and fall back to the sparkline pair from 1a.
- The 1g and 1k overlays need 90 and 132 columns; below that, draw them full-width with no border box rather than clipping.
- Below 31 columns or 6 rows, refuse as today.

## State

New state beyond what `app::App` already carries:

- `history: HashMap<SheepId, (VecDeque<f32>, VecDeque<u64>)>` — CPU and RSS samples, ring buffers sized to the widest chart (140) plus the sparkline (10). Populated from the existing two-second poll; the six-minute window at 5s samples is 72 points, so a 140-point buffer covers twelve minutes and lets `w` widen the window without re-collecting.
- `view: Pane` — which pane owns the body (`Flock`, `Folds`, `Sheep`, `Edit`, `Secrets`, `Bleats`, `Settings`), plus an overlay slot for `Help` and `ApplyMenu`.
- `edits: BTreeMap<FieldId, (Value, Impact)>` — the pending change set, its `Impact` deciding whether 1g appears on close.
- `filters: Vec<Filter>` in the feed — one per axis, ANDed, newest last so `esc` can pop.
- `reveal: Option<(Key, Instant)>` in secrets — the ten-second reveal window.
- `frozen: Option<FrozenAt>` — already implied by the shipped frozen banner; the redesign needs the timestamp for the `FROZEN AT` column.

## Design tokens

**Colour** — see the table above; go through `theme::Palette`.

**Geometry.** Design target 160×48 cells. Column widths per pane are listed with each screen and always sum to 160. Fixed spacing conventions: 1 blank row above every section band and every group rule; a hairline `─` rule of exactly 160 cells between major regions; 2 cells between a label and its value inside a band; the landing table keeps a 27-cell right margin.

**Gauges.** 10 cells for a row-level gauge, 20 for a fold share bar, 8 for the flock CPU strip. Sparklines are 10 cells. Charts are 140×8 (CPU) and 140×5 (memory) with an 8-cell axis gutter.

**Type.** A terminal has one face. The HTML reference uses JetBrains Mono at 11.5px/16px as a stand-in for the operator's terminal font; nothing in the design depends on it beyond fixed advance width and coverage of the glyph table above.

## Assets

None. Every visual is a character. The sheep in 1k is eight block glyphs; there are no images, icons or fonts to ship.

## Files

In this bundle:

- `screenshots/` — one PNG per frame, 2× the design grid. Caveat: the capture renders block glyphs as solid fills and `░` as a dot texture, so a sparkline reads as an area shape rather than as eight discrete steps. Trust the HTML and the glyph table above for exact codepoints; use the PNGs for layout and proportion.
- `Lookout Frames v2.dc.html` — the nine picked frames, each annotated. The implementation reference.
- `Lookout Frame Gallery (round 1, all options).dc.html` — twelve frames including the three unpicked alternatives. Context only.
- `support.js` — the runtime both HTML files load. Needed only to open them in a browser.

In the repo, the code these frames replace or extend:

| Path | What it draws today |
|---|---|
| `crates/shep-cli/src/lookout/view/flock.rs` | the flock table → 1a's table, 1j's fold grouping |
| `crates/shep-cli/src/lookout/view/host.rs` | the host strip → 1a row 2 |
| `crates/shep-cli/src/lookout/view/detail.rs` | the sheep detail band → 1a rows 37–38, and the facts band in 1d |
| `crates/shep-cli/src/lookout/view/bleats.rs` | the feed → 1a rows 41–46, and 1i |
| `crates/shep-cli/src/lookout/view/status.rs` | the status bar → every frame's row 47 |
| `crates/shep-cli/src/lookout/view/settings.rs` | the daemon settings screen → the pattern to follow for 1e, 1h |
| `crates/shep-cli/src/lookout/view/pane.rs`, `scroll.rs` | pane sizing and scrolling → the row allocations above |
| `crates/shep-cli/src/lookout/theme.rs` | the four semantic roles → add `sky`, keep the `NO_COLOR` and 16-colour tiers |
| `crates/shep-cli/src/lookout/field.rs`, `input.rs`, `app.rs` | field editing, keys, state → 1e, 1g, and the new state above |
| `crates/shep-cli/src/lookout/frames.rs` | `Scene::ALL`, the snapshot scene list → add a scene per new pane |
| `crates/shep-core/src/config/app.rs` | `AppConfig` and its `group`/`blurb`/`example`/`suggest` annotations → the source of every label, description and enum option in 1e |
| `docs/lookout/README.md`, `docs/lookout/frames.txt` | the frame gallery to regenerate once the panes land |

## Where to start

1. `theme.rs` — add the `sky` role and the gauge-remainder colour, with tests at both tiers, matching the existing `the_anstyle_binding_agrees_with_this_ones_colours` pattern.
2. Small shared widgets, each unit-testable against a string: `gauge(value, ceiling, cells)`, `sparkline(&[f32], cells)`, `chart(&[f32], ceiling, cols, rows)`, `band(label, role)`, `impact_tag(Impact)`. These five carry most of the redesign.
3. 1a, since every other pane borrows its bands, gutter, rules and status bar.
4. 1d, then 1e + 1g as a pair (the dialog is meaningless without the pending-edit set).
5. 1i, 1j, 1k, 1l.
6. 1h last: it needs the store's reveal-audit path, which is the only piece here that touches something outside `lookout`.

Add a `Scene` to `frames.rs` for each pane as you go, so `cargo test -p shep --lib --all-features -- --ignored write_the_gallery` keeps `docs/lookout/frames.txt` honest — the frames in this bundle are mockups, and the generated gallery is what should replace them as the reference.
