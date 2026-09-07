# `shep lookout` — frames

`shep lookout` (alias `dash`) is a terminal dashboard over the shepherd. It
now draws all four panes spec §9 names: the flock table (the spine, and the
only pane Phase 12a shipped), a host-usage strip above it, and a sheep
detail pane plus a bleats feed underneath a selected row — both added in
Phase 12b. Plan 1a repainted the whole thing afterward: reverse-video bands,
a load and memory gauge on the strip, and a CPU sparkline and a memory gauge
in the table. See "What 1a settled" below.

This directory is not documentation of a shipped design. It is the thing
The maintainer asked for: *"let's start with flock table first. I need to see the
panels before I can make a full decision."* A TUI cannot be screenshotted
the way a web page can, so these rendered frames are how she looked at each
phase before deciding what came next.

## Reading the frames

- `frames.txt`, thirty-five scenes rendered through the flattened `NO_COLOR`
  palette, the one an operator with `$NO_COLOR` set or a 16-colour terminal
  actually gets. Open it in any editor.
- `frames.ansi`, the same thirty-five scenes rendered through the coloured
  palette the pinned snapshot tests use. Read it with `less -R` so the
  escape codes render instead of printing literally.

The two files are deliberately different pictures of the same dashboard, not
one file with the colour stripped from the other: `frames.txt` still shows
the section bands and the selection marker, since reverse video and the `>`
gutter both survive `NO_COLOR`, but the meadow/sky/bark roles are gone.

Both files are generated, not hand-written, and both come from the same
scene list the pinned snapshot tests read (`Scene::ALL` in
`crates/shep-cli/src/lookout/frames.rs`) — so they cannot drift from what
the test suite checks. Regenerate them with:

```bash
cargo test -p shep --lib --all-features -- --ignored write_the_gallery
```

## What 12a settled

- **Daemon death: bounded retry, then freeze, never exit.** The link task
  re-dials the shepherd 5 times, at 250/500/1000/2000/4000 ms — about 7.75 s
  of waiting — before it gives up. Once the ladder is exhausted, lookout
  shows the frozen banner (`the shepherd has died: these values are frozen
  as of <time>`), stops polling and re-dialling, and leaves the last known
  values on screen. The uptime column stops advancing with it: a frozen
  dashboard whose clock kept counting would be lying about a specific sheep
  by name. lookout never exits on its own — the operator quits with `q`.
  A shepherd that was **never** running is a different case: that connect
  attempt happens before raw mode is entered, and a failure there is the
  ordinary `daemon_unreachable` refusal every other verb gives, not eight
  seconds of a full-screen dashboard cycling "reconnecting" for a shepherd
  that was never there.
- **Actions are on by default, and it says so.** `--read-only` (or
  `lookout.allow_control = "false"` in the KV store) closes the gate before
  any action key does anything. Three action keys exist — `x` (stop), `R`
  (restart) and `L` (reload) — and none of them acts on the keypress that
  pressed it: an action key arms a confirm, Enter confirms it, any other key
  cancels it, `q` and Ctrl-C still quit even with a prompt up, and an armed
  prompt nobody answers expires after ten seconds. Read-only refuses
  outright, with a literal sentence (`read-only: from --read-only or
  lookout.allow_control`). The status bar always says which state is in
  force. The apply menu a parked pane offers on close is the one exception
  to the arm-then-confirm rule: it names its keys on screen, so `L` and `R`
  send on the press, and it expires on the same ten seconds.
  This is a fat-finger catch, not a security boundary: lookout runs as the
  operator's own process, under the operator's own uid, so the shepherd has
  no way to refuse a keypress it cannot tell apart from `shep stop`.
- **Colour is always redundant with text.** Every coloured cell says the
  same thing in words that the colour is repeating — the STATUS column
  prints `errored` under `--bark`, the banner prints `the shepherd has
  died` under `--bark`. Nothing here is colour-only, so `NO_COLOR` and a
  16-colour terminal both lose decoration, never information.
- **Narrow terminals drop columns in a fixed order**, least diagnostic
  first, one at a time, at the width in brackets: `MEM/CEIL` (134), `CPU 20s`
  (122), `CFG` (116), `SMIT` (101), `FOLD` (89), `EXIT` (78), `RESTARTS`
  (68), `PID` (59), `MEM` (49), `CPU` (41), then `UPTIME`, leaving
  `ID NAME STATUS` as the floor. `MEM/CEIL` and `CPU 20s` go first, ahead of
  even `CFG`, because both restate a number a plainer column already
  carries: the gauge repeats `MEM`, the sparkline repeats `CPU`. `SMIT`
  still goes next for being much the widest of what is left, and `EXIT`
  early after it because it renders `-` for every sheep that is still
  running, which is what the pane shows most of the time. Below 31 columns
  or 6 rows the pane refuses outright rather than draw overlapping garbage,
  with a two-line message short enough to survive the narrowest terminal it
  is warning about. The table draws inside a two-column border, so seeing
  `MEM/CEIL` at all takes a terminal at least 148 columns wide, and
  `CPU 20s` at least 136.

## What 12b settled

- **A selected sheep, and the table marks it.** `j`/`k` move the selection
  by a row now, not just the viewport; `g`/`G` jump to its ends. A `>`
  gutter to the left of ID marks the selected row, and the offset re-clamps
  whenever a snapshot replaces the flock map or the selected sheep drops out
  of it. The two panes below the table both describe whichever sheep is
  selected.
- **A name filter, narrowing the table in place.** `/` opens a box in the
  status bar; typing narrows the table to sheep whose name contains the
  query, `Enter` applies it and closes the box, `Esc` cancels the edit, and
  `Ctrl-C` still quits from inside the box. Once a filter is applied, `Esc`
  clears it rather than quitting — the one carve-out to "every other key
  cancels" a filter needs, so an operator does not have to reach for `/`
  and backspace to get back to the whole flock. The title carries a second
  number, `2 of 6 in the flock`, for as long as a filter is narrowing what
  the table shows.
- **The bleats feed reads log files, not the bus.** It re-reads the
  selected sheep's `out`/`err` log files from disk on every refresh, rather
  than subscribing to the `log.*` bus topic. A busy flock costs one bounded
  64 KiB read per file per refresh; subscribing would make the dashboard the
  highest-volume subscriber on the bus for a pane most refreshes don't even
  draw. What the feed cannot show, it says: lines the
  reader saw and discarded count exactly, and bytes below its window report
  as bytes, because nothing counted the lines in those and guessing would be
  worse than saying so.
- **The detail pane reads what the table already has, with one exception.**
  Every line but the last comes from the same `ProcessInfo` the table's own
  rows are built from: the untruncated name, both log paths, and whichever
  columns the current width tier has dropped. The lamb line is the
  exception — `ProcessInfo::lambs` is `None` on the `ListFlock` reply the
  table is built from, so the pane fetches it separately with a
  `Request::Describe` on selection change and on `r`, never on the
  two-second poll, and it carries its own age stamp because of that.
- **Short terminals drop panes before they drop columns.** A plain 80×24
  gets all three: host strip, detail pane, feed. Below 24 rows the detail
  pane goes first, below 18 the feed goes with it, and below 14 the host
  strip goes too and only the flock table remains — the same shape 12a
  shipped alone. The order is least-diagnostic-first, the same principle
  the column drop already used: the detail pane only restates what the
  selected row already shows, so it is the cheapest thing to lose, while
  the feed is the only pane carrying information no other pane has. The
  `no_detail` scene in `frames.txt` is the 120×20 case — feed present,
  detail gone.

`shep lookout` ships complete as of Phase 16: the filter, lambs in the detail
pane, and the three action keys behind the gate are all built. Plan 1a then
redrew the landing pane on top of that shipped surface. See
[docs/specs/deferred.md](../specs/deferred.md) for the workspace's remaining
debt.

## What 1a settled

- **The flock table grew two columns.** `CPU 20s` is a ten-cell sparkline of
  the sheep's own CPU history, scaled to one ceiling shared by every row:
  the busiest sample any sheep has posted in the retained window, floored at
  2%, so rows read against each other instead of each filling its own
  column. `MEM/CEIL` is a ten-cell gauge of RSS against the sheep's
  `max_memory`, when it has one. Fourteen columns total, with `NAME` capped
  at 32 (`NAME_MAX`): past that width the table ends and the row stays
  empty, rather than `NAME` swallowing the rest of a wide terminal. Both new
  columns are read restatements of `CPU` and `MEM`, which is why the drop
  ladder above sheds them first.
- **The title, the two section bands, and the selected row all paint now.**
  The title and the `FLOCK`/`DOGS` bands are reverse video: meadow for the
  flock band, sky for dogs, and the title turns bark when the link to the
  shepherd has frozen. The selected row and the status bar are the only two
  rows that ever paint a background; everywhere else the operator's own
  terminal background shows through. Under `NO_COLOR` the roles disappear
  but the reverse video stays, so a band still names its section in plain
  text, and the selected row falls back to the `>` gutter it always had.
- **The host strip gained two gauges and a sparkline.** A ten-cell load
  gauge and a ten-cell memory gauge sit next to the numbers they used to
  print alone, and an eight-cell sparkline now rides beside `flock cpu`,
  scaled to its own window peak rather than the table's ceiling, since it
  plots a sum across the whole flock. The strip reads machine then flock,
  left to right: load, host memory, the `N errored · N parked` summary,
  then flock CPU and memory, so the two host readings survive a narrow
  terminal together, and the summary is what falls off next.
- **The detail pane's two log-path lines became one.** `out` and `err` now
  share a single row with a divider between them and the pair's combined
  size on disk after it, and the pane gained a `cfg !N pending` cell for a
  sheep still carrying an unapplied config change.
