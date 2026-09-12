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

- `frames.txt`, thirty-six scenes rendered through the flattened `NO_COLOR`
  palette, the one an operator with `$NO_COLOR` set or a 16-colour terminal
  actually gets. Open it in any editor.
- `frames.ansi`, the same thirty-six scenes rendered through the coloured
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
  stops polling and re-dialling and leaves the last known values on screen.
  The title band turns bark and carries `THE SHEPHERD HAS DIED ▖ these
  values are frozen as of <time>`, and the table, the host strip and the
  section bands all go to one muted ink, so no cell can be read as current.
  The `UPTIME` header becomes `FROZEN` over a duration that stopped
  advancing when the link did: a frozen dashboard whose clock kept counting
  would be lying about a specific sheep by name. The detail band and the
  bleats feed give their rows to the link panel, which names the ladder it
  climbed, quotes the last dial's own error, counts how long ago that was,
  and says what is left to try. lookout never exits on its own — the
  operator quits with `q`. `r` is refused with the rest of the keymap:
  `run_link` has already returned by then, so nothing survives to answer a
  redial, and the panel says `shep muster` instead.
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
  prints `errored` under `--bark`, the frozen band prints `THE SHEPHERD HAS
  DIED` under `--bark`. Nothing here is colour-only, so `NO_COLOR` and a
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
  running, which is what the pane shows most of the time. Below 33 columns
  or 6 rows the pane refuses outright rather than draw overlapping garbage,
  with a two-line message short enough to survive the narrowest terminal it
  is warning about. The 33 is the table's own 31-column floor plus the
  2-column gutter the selection marker needs; the table draws inside a
  two-column border, so seeing
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
  shepherd has frozen. Frozen, the two section bands go muted with the rest
  of the table and are told apart by their own words, which is what
  `NO_COLOR` already asks of them. The selected row and the status bar are the only two
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
  flock CPU and memory, then the machine's own uptime, so the two host
  readings survive a narrow terminal together, and the summary is what
  falls off next.
- **The detail pane's two log-path lines became one.** `out` and `err` now
  share a single row with a divider between them and the pair's combined
  size on disk after it, and the pane gained a `cfg !N pending` cell for a
  sheep still carrying an unapplied config change.

## What 1i settled

- **`b` opens the bleats feed full screen on the selected sheep, and pins
  it.** There is no table on screen to change the selection with, so the
  pane describes that one sheep for as long as it stays open. It reads the
  same log files the corner feed does, not the bus, just polled faster
  while the pane has focus.
- **Three filter axes, and they compose with AND.** `o` cycles the stream
  (out, err, or both), `m` cycles a minimum level, and `/` opens a box for
  a text or regex match, with matches highlighted. A line with no
  detectable level always shows, whatever the minimum is set to: most app
  output carries no level at all, and hiding it would make the pane worse
  at the job it was opened for. The filter row states the composition and
  a survivor count scoped to the current window.
- **`esc` drops the newest filter chip before it closes the pane.** One
  axis at a time, newest first, and only once none are left does `esc`
  close the pane and return to the table.
- **`j`/`k` scroll a line, `ctrl-d`/`ctrl-u` a page, `G` jumps to the end
  and resumes following, `f` toggles following, `w` wraps long lines
  instead of truncating them, and `n`/`N` step between matches.**

## What 1j settled

- **`F` gathers the flock by fold instead of by name.** A fold is
  `AppConfig::fold`, a project-level grouping the sheep in it are none the
  wiser about. Each fold gets a header row; sheep with no fold sit under a
  `no fold` header instead, and dogs, which are never in a fold, keep their
  own `Dogs` band underneath. `F` again goes back to the flat table.
- **A fold header's numbers are its members summed, with one exception.**
  Restarts, CPU and memory are a sum across the fold; uptime is the
  *shortest* of the members', so the header reads as time since the fold was
  last disturbed rather than the age of its longest-lived sheep. A `SHARE`
  gauge and a `NOTES` percentage both show that fold's share of the whole
  flock's memory.
- **`z` collapses the fold under the cursor**, hiding its members and
  leaving the header behind with its rollup intact. Pressed again it opens
  the fold back up. It does nothing anywhere else, including on the `no
  fold` or `Dogs` bands.
- **An action on a fold reaches every sheep in it.** `x`, `R` and `L` on a
  fold header arm the same confirm the flat table's group header does, and
  the prompt names the count: `restart all 4 sheep in fold edge? enter
  confirms, any other key cancels`.
- **The `no fold` header is not selectable.** The wire has no way to name
  "everything with no fold" in one selector, so there is nothing an action
  there could send.

## What 1e settled

- **No edit reaches the shepherd until the config pane closes.** An edit,
  a cycle, an array change: each files into a change set the pane carries,
  and `esc` writes the whole set in one pass when it closes the pane. `r`
  is not an edit: it re-reads the pane's config from the shepherd while
  the pane is still open, same as any other request.
- **`u` undoes the newest unsent edit**, one at a time. It files nothing
  itself, so it never needs the control gate.
- **`tab` walks the pane's eight groups; `1` through `8` jump straight to
  one.** Both reset the cursor to the group's first field. A dog's pane has
  no groups to move between, since its schema declares none.
- **Env keys moved into the main field list**, under whichever group is on
  screen, and out of their own sub-screen. No key ever shows its value,
  before or after the move: the shepherd never sends one.
- **Read-only refuses the first keystroke that would file an edit**, not
  the close. The pane stays open and unchanged; nothing is left to refuse
  when `esc` writes.
- **A right-hand explanation panel describes the focused field**: its help
  text, its current value, its default, an example, what it accepts and
  refuses, and which other fields it interacts with. Below 90 columns
  there's no room for it and the cost column carries every row's cost
  alone; from 90 the panel draws and the cost column gives way to it; from
  160 both draw together.
- **Dogs take the same batched write and the same panel** a sheep's pane
  does: edits file into one change set, `u` undoes them, and `esc` sends
  the whole `dogs.toml` section in one request, however many fields
  changed.
