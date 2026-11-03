# Dogs step 5b: the catalogue in lookout

Status: settled with the maintainer in a grill on 2026-11-03. The first
half of step 5 of [Dogs as plugins](2026-10-12-dogs-as-plugins-design.md),
tracked in #1075 and #566. Lands before [panes](2026-11-03-dogs-step-5a-panes-design.md),
since it needs only [step 3](2026-11-02-dogs-step-3-install-design.md).
Spec 1's decisions stand except where this amends them.

## What stands

- The catalogue installs. #566's browse-only marketplace is superseded.
- Install, upgrade and delete behave as step 3 says. The catalogue is a
  second front door to the same code, not a second implementation.
- Every dog prompts with what it declares. The CLI touches the network,
  the daemon never does.

## One view, three ways in

| Way in | Where |
|---|---|
| A `browse dogs` row | The last row of the settings screen's dogs section |
| The `i` key | From the flock view. The ticket checks `i` is not part of a shared keymap row first |
| `shep lookout catalogue` | Opens lookout straight on the catalogue |
| A greyed row | Uninstalled index dogs listed greyed under the installed ones in the dogs list. Selecting one opens its catalogue entry |

- The list reads the index cache at `$SHEP_HOME/cache/dogs.json`. Opening
  the catalogue with a cache older than 24 hours fetches in the
  background and redraws, as `--refresh` would. The greyed rows in the
  dogs list read the cache only and never fetch.
- No cache and no network: the catalogue says so in one line and lists
  nothing. The dogs list shows no greyed rows.

## What a row shows

- The list: name, category, VERSION as step 3 defines it (`→ <new>` when
  the index is ahead), and the state: not installed, installed, enabled.
- The entry: description, repo, license, kind, and the permissions the
  index lists for this version, each marked required or optional.
- A row whose kind shep cannot install shows its instructions, as
  `shep dogs --available` prints them, and no install action.

## The index carries permissions

- A `cargo` row gains `permissions`: each permission's name and whether it
  is required, as the dog's manifest declares them at that version.
- The reusable dog release workflow (#1447) writes it, reading the
  manifest off the binary it just built for the runner's own target.
- Install still prompts from the real manifest. A permission the binary
  declares that the row did not list is flagged in the prompt, one line
  each. A row without `permissions` shows "not listed" in the entry and
  installs as today.
- The field is additive. `dogs.schema.json` gains it and the index keeps
  `version: 1`.

## Install from lookout

- The grant prompt opens as a modal with the same text the CLI prints,
  built by the same function. Yes, no and the per-permission choices are
  the CLI's.
- On yes, the download or build runs in the background. Progress sits in
  the status bar: the dog's name, the step (downloading, building with
  cargo, enabling) and the time spent. Navigation keeps working.
- A failure opens a modal with the installer's output, capped the way
  `shep trigger`'s replies are.
- Dependencies install first, each with its own modal, in the order the
  CLI asks them. A no on any one stops the install, as in the CLI.
- Quitting lookout during an install asks first. A cancelled install
  leaves nothing behind, since step 3 writes the receipt last and the
  per-dog lock is released on drop.
- One install at a time from lookout. A second asks to wait for the first.

This needs step 3's install and upgrade split from their terminal I/O: the
prompt answer passed in, progress reported through a sink. The CLI keeps
its own printing on top of the same core.

## Upgrade and delete

- A row showing `→ <new>` has an upgrade action. It runs step 3's
  `shep upgrade <dog>`, so a newly declared permission waits for review
  and the dog keeps its old grants.
- An installed dog has a delete action behind the existing confirm. An
  adopted dog with no receipt keeps its file, as `shep delete` does.
- Enable and disable stay in the dogs list.

## Docs

`lookout.astro` gains the catalogue and its key, `community-dogs.astro` says
the index carries permissions, and `writing-a-dog.astro` says the release
workflow writes them. The CLI reference is regenerated for
`shep lookout catalogue`.
