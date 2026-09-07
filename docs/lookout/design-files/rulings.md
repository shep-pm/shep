# Rulings on these frames

The frames next to this file are concepts. Reproduce the layout and the
information design. Do not treat the copy, the sample data, or the behaviour
claims as spec: several are wrong about shipped shep, and the ones that would
become real bugs are listed below.

## What is being built

1a, 1d, 1e, 1g, 1j, 1k and 1l go ahead as drawn. 1i changes scope. 1h waits.

## 1h waits for the store

The secrets pane describes a vault shep does not have. `crates/shep-core/src/kv.rs`
is `kv.json` today: a flat `BTreeMap<String, String>`, `0600` and file-locked,
unencrypted, with no per-sheep scoping, no timestamps, no set-by or read-by
record, and no audit log anywhere in the repo. `shep set`/`get`/`unset` never
touch the daemon (`docs/decisions.md:1089`), and nothing reads the store into a
sheep's env at spawn.

That functionality is being built separately and merges first. Build the pane
against what lands, not against the frame.

## 1i reads the window, not the file

Dropped: the whole-file line count, the absolute line numbers, and a density
gutter spanning the whole file. The frame claims 12,904 lines and 831K unread in
the same breath, and counting lines means reading them. `tail.rs:79` already says so.

The pane starts blank or on a tail and fills as lines arrive. The three filter
axes are the point of it and they stay.

Open, not scoped: a toggle between the live feed and a file read, so the same
filters work against a log on disk instead of a hand-written grep.

## Do not copy these into code

- `L reload` does not mean no downtime. `ReloadMode` (`crates/shep-daemon/src/supervisor.rs:1658`)
  takes `Overlap` for an app with no probe, and an app without `SO_REUSEPORT`
  takes `EADDRINUSE` on an overlapping reload. 1g promises otherwise about an
  app whose own frame shows `reuse_port false`.
- An instance is not a lamb. `docs/terminology.md:20` says it in those words.
  Four places in the bundle call `web ×3`'s instances lambs.
- `▌` is East-Asian Ambiguous width. `crates/shep-cli/src/lookout/view/flock.rs:46`
  already rejected `▸` for that, with a test. The rest of the glyph vocabulary
  needs the same check.
- The wire never returns an env value. `SheepConfigView` clears env before the
  struct is built (`crates/shep-core/src/protocol/request.rs:1249`), so 1e cannot
  show a Flockfile key's plaintext.
- EXIT, CFG and SMIT are real columns (`crates/shep-cli/src/lookout/view/flock.rs:103`)
  and 1a drops all three without saying so. CFG marks every other sheep carrying
  pending config, so losing it costs more than the column.
- `d`, `J` and `K` are already bound (`crates/shep-cli/src/lookout/input.rs:59`),
  and `map_key` dispatches on mode rather than pane.
- There are eight config groups, not seven, and `cron` is missing from the
  design's list (`crates/shep-core/src/config/scaffold.rs:85`).

## Cheaper than the frames suggest

- The host strip already reads load, cores and host memory
  (`crates/shep-cli/src/lookout/source.rs:217`). Only the gauges are new.
- Fold actions already work on the wire: `SelectorSpec::Fold`
  (`crates/shep-core/src/selector.rs:81`) means `shep stop fold:api` runs today.
  1j needs a view, not a protocol change.
- The impact tags map onto `ApplyGroup` (`crates/shep-core/src/config/apply.rs:18`),
  already shown in the pane's COST column.
- The rollup math exists (`crates/shep-cli/src/lookout/app.rs:3876`), and so does
  1g's press-to-act carve-out (`docs/lookout/README.md:54`).

## The charts need a decision first

Nothing keeps history. The daemon holds one CPU baseline per pid
(`crates/shep-daemon/src/limits/stats.rs:53`) and lookout replaces the flock map
on every poll. That poll is 2s (`crates/shep-cli/src/lookout/link.rs:29`), not the
5s the frames assume, and `cpu_percent` is an average over a window that resets
every 15s (`crates/shep-daemon/src/limits/mod.rs:33`) rather than a reading at an
instant. A client-side ring buffer is fine. The axis labels have to state what
the samples really are.

## Two numbers to ignore

The 1a column table says the columns sum to 133 with a 27-cell margin. They sum
to 127, and the HTML uses 33. The HTML is right, and it is the reference the
handoff itself points at.
