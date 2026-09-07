# Lookout panes 1i and 1j: bleats full screen, and the fold view

**Status:** approved 2026-09-08. Not yet implemented.

Two panes from the redesign bundle, built on what 1a landed. They share no code
with each other and can be built in parallel; they share this document because
both borrow the same foundations and both were ruled on together.

`docs/lookout/design-files/rulings.md` is the authority on which of the bundle's
claims survive contact with shipped shep. Where a frame and a ruling disagree,
the ruling wins.

## 1i: bleats, full screen

The dashboard's bleats feed is four lines in a corner. This is the same log,
given the screen, with filters.

### Shape

A new `Body` variant and `Scene`, full screen over the dashboard. It opens on
whichever sheep was selected and pins it: there is no table on screen to change
the selection with, so the pane describes one sheep for as long as it is open.

### Where the lines come from

`tail.rs`'s existing bounded window, polled faster while the pane has focus.

**Not the bus, and the reason is worth recording because it looks wrong at first
glance.** A dedicated viewer for one sheep sounds like exactly the case where a
subscription beats a poll. It is not, because the bus cannot scope logs to one
sheep. The topics are `log.out` and `log.err`, and a subscription's globs match
the topic rather than the sheep, so subscribing means taking every sheep's
output and discarding almost all of it. `source.rs` already prices that:

> Subscribing would make lookout the bus's highest-volume subscriber, and
> `super::link::run_connected` answers a lag with an immediate `ListFlock`, so
> log traffic would turn into shepherd request load exactly when the shepherd is
> busiest.

Two alternatives were considered and rejected. A second connection dedicated to
the pane would stop a lag amplifying into the dashboard's `ListFlock`, but the
daemon still fans every sheep's lines at this client. Per-sheep topics
(`log.out.<id>`) would fix it properly and are newly cheap, since the
compatibility work merged in #173 makes a new topic additive and needs no
version bump. That is a protocol change to serve a view, and it is not this
pane's job. It stays available if the poll proves inadequate.

### The three filter axes

They compose with AND, and the filter row says so.

| Axis | How it decides |
|---|---|
| stream | out, err, or both. Exact: shep writes the two files separately, so a line's stream is known rather than inferred |
| level | a level word parsed from near the start of the line, case insensitive |
| match | text or regex, with matches highlighted |

**A line with no detectable level is always shown, whatever the minimum is, and
the filter row states that.** An app's stdout is arbitrary text, and most of it
carries no level at all. Hiding what could not be classified is how a log viewer
loses the thing it was opened to find, and a sheep printing bare lines would
vanish entirely the moment a minimum was set.

`esc` drops the newest chip rather than clearing every filter, so backing out is
one key at a time. With no chips left, `esc` closes the pane.

### What the ruling dropped, and what that leaves

`rulings.md` drops the whole-file line count, absolute line numbers, and the
density gutter. All three require reading the whole file to say anything, and
this pane reads a window. Counting lines means reading them.

That leaves the frame's 8-cell line-number column with nothing true to put in
it. A window-relative number counts from wherever the window happens to begin,
which moves as the window slides, so it would look meaningful and not be.
**Drop the column and give its 8 cells to the line text.**

The filter row still states a survivor count, scoped honestly to what was read:
`211 of 2,847 lines in the window`, not the frame's count against a whole file
nobody has counted.

## 1j: the fold view

### Shape

A grouping mode on the flock table, not a separate pane. `App` gains a grouping
state with two values, flat and by fold, and `F` toggles it.

The table already carries a two-level row model for multi-instance apps, where
`web ×3` is a group row with instance rows beneath it. A fold header is that
same shape with a different key, so selection, scrolling, the action confirms
and the detail pane below all keep working. The frame supports this reading: the
meadow band and host strip are unchanged, and `F` toggles back rather than
closing anything.

### Rows

One header per fold, plus `no fold` for sheep whose `AppConfig::fold` is `None`,
plus `dogs`, which the header names explicitly because a dog is never in a fold.

A header sums its members' restarts, CPU and memory, and takes the **shortest**
of their uptimes. That matches the existing group-row rollup rule rather than
inventing a second one.

Header rows render in ink, member rows in ink-2.

### Two levels of grouping, not three

A three-instance app inside a fold shows as one member row and keeps its own
`×3` rollup. Instances stay behind the app's row. Without this, `edge ×4` is
ambiguous about whether four counts apps or processes, and the answer would
change as somebody scaled an app.

### The share bar

A 20-cell gauge of that fold's share of total flock memory, with the percentage
stated in `NOTES`. It reuses 1a's `cell::gauge`.

### Actions

Selecting a fold header and pressing `x`, `R` or `L` acts on every member, each
still arming a confirm that names the count.

Nothing new is needed on the wire. `SelectorSpec::Fold` already means
`shep stop fold:api` runs today, so this is a view over a capability that ships.

### A fold header needs a detail branch

`detail.rs` already has one for `RowKey::Group`: rollup figures, no lamb line,
no log paths, because a group has no single process to walk or tail. A fold
header is the same situation one level up and reuses that shape rather than
inventing a fourth.

### Columns

2 gutter, 24 `FOLD / NAME`, 12 `STATUS`, 22 `SHARE OF FLOCK MEM`, 10 `MEM`,
9 `CPU`, 10 `UPTIME`, 8 `RST`, 63 `NOTES`.

Eight columns against the dashboard's fourteen, so fold view needs its own drop
ladder for narrow terminals. Same `columns_for` mechanism, separate table:
`SHARE OF FLOCK MEM` at 22 cells is the first thing that should go, and it does
not exist in flat view at all.

`z` collapses a fold. `F` returns to the flat list.

## What both panes reuse from 1a

- `cell::gauge`, `cell::sparkline`, `cell::rule`, `cell::band`
- `Palette::band`, `Palette::ground`, and the sky and meadow roles
- `fit` and `columns`, which measure display columns rather than `char`s
- the group-row rollup in `app.rs` and its detail branch in `detail.rs`

## Testing

Both panes are rendered output, so the tests that matter assert what a frame
actually draws.

- Every new scene joins the `frames.rs` gallery, so a rendering change shows up
  as a diff somebody reads.
- Width sweep: both panes land inside their own rows at every width the existing
  sweep covers. 1j's ladder gets the same treatment 1a's did, including the
  invariant that a chosen tier fits.
- 1i's level filter: a line with no level survives a minimum, which is the
  decision most likely to be quietly reversed by someone tidying up.
- 1i's `esc`: drops one chip, then closes. Not one or the other.
- 1j's rollup takes the shortest uptime, not the first or the longest.
- 1j's fold actions reach every member, and the confirm names the count.

## What this does not do

- No per-sheep log topics. Recorded above as the fix if polling proves
  inadequate, deliberately not built here.
- No whole-file anything in 1i: no line count, no absolute numbers, no density
  gutter. The pane reads a window and says so.
- No third grouping level in 1j.

## Decisions

1. **1i pins the sheep it opened on.** Full screen leaves no table to change a
   selection with.
2. **1i polls the file rather than subscribing.** The bus cannot scope to one
   sheep, so a subscription costs every sheep's output. This reverses a call
   made earlier in the design conversation on a premise that turned out false.
3. **An unclassifiable line survives the level filter.** The alternative hides
   exactly the output a bare-printing app produces.
4. **1i drops the line-number column** rather than filling it with a
   window-relative count that would look meaningful and not be.
5. **1j is a grouping mode, not a pane.** The table's existing two-level row
   model already fits, and `F` toggles rather than opens.
6. **1j groups two levels deep.** An app inside a fold stays one row with its
   own instance rollup.
