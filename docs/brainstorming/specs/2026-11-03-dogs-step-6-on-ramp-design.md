# Dogs step 6: the on-ramp and shep upgrading itself

Status: settled with the maintainer in a grill on 2026-11-03. Step 6 of
[Dogs as plugins](2026-10-12-dogs-as-plugins-design.md), tracked in #1075
and #1434. Spec 1's decisions stand except where this amends them.

## What stands from spec 1

- Decision 6: log rotation stays a dog, and shep offers it when logs grow.
- Decision 7: shep owns a fixed list of needs and writes the hint text.
  The index maps a dog to a need.
- Decision 9: the CLI refreshes its cached index after 24 hours. The
  daemon never touches the network.
- Decision 10: the docs reorder below.

## The welcome

`shep welcome`, and the first run that prints it, gains a fixed dogs block
after the quick start. It reverses "no dogs in the welcome" and gets a
`docs/decisions.md` entry.

```text
Getting started
  shep start server.js    start it and keep it alive
  shep flock              see what's running
  shep bleats server      follow its output
  shep startup            bring it back after a reboot

Dogs add what shep leaves out
  shep install log-rotate rotate logs as they grow
  shep install discord    hear when a sheep crashes
  shep install deploy     ship from a git branch

  shep welcome            show this again
```

- Fixed text, never read from the index: a first run may have no cache.
- The existing tests hold it to 80 columns and no em dash. A new one pins
  that each named dog is in `web/public/dogs.json` under that `adopt_as`.
- The comment in `welcome.rs` explaining why dogs were left out goes.

## Hints

| Hint | Shows when | Offers | Ends when |
|---|---|---|---|
| `logs` | A sheep's log file passes 100 MiB | Index dogs mapped to `logs` | One of them is enabled |
| `alerts` | A sheep has restarted 3 times since the shepherd started | Index dogs mapped to `alerts`, and the built-in `bark` | One of them is enabled |
| `upgrade` | The cached index names a newer shep than this CLI | `shep upgrade` | This CLI is that version |

- One line, under `shep flock` and in lookout's status bar. One hint at a
  time, in the table's order.
- Human output on a terminal only. Never in `--format json`, never with
  `--quiet`, never on a pipe. No JSON shape changes.
- Hints read the index cache and never fetch. No cache, no dog hints.
- shep writes every hint's text. The thresholds are constants in one
  place, pinned by tests.
- An index row gains `needs`: a list of need names. A name shep does not
  know is ignored, so the index can add a need before shep shows it.
- The index gains a top-level `shep`: the latest released version. The
  release PR writes it, so it lands with the version it names.

## `shep hints`

```text
$ shep hints
HINT     STATE  FOR IT
logs     on     log-rotate (not installed)
alerts   off    discord (installed), bark (built-in)
upgrade  on

$ shep hints off logs
$ shep hints off          # every hint
$ shep hints on alerts
```

- State lives in `$SHEP_HOME/hints.json`, written only by the CLI. Off is
  remembered, on is the default.
- A new verb, so it is wired into the help groups and the docs `VERBS`
  list in `cli/help.rs`.

## `shep upgrade`

Bare `shep upgrade` upgrades shep, then every installed dog the cached
index has a newer version of. One confirm lists all of it first:

```text
$ shep upgrade
shep        0.24.0 → 0.25.0   brew upgrade shep
log-rotate  0.2.4  → 0.2.5
deploy      0.6.6  → 0.7.0
Upgrade these? [Y/n]
```

`shep upgrade <dog>` and `--dogs` stay as step 3 built them.

### Finding the channel

Read from the running binary's resolved path:

| Channel | Path | Runs |
|---|---|---|
| Homebrew | Under `brew --prefix`'s `Cellar/shep` | `brew upgrade shep` |
| cargo | In `$CARGO_HOME/bin` | `cargo install --locked shep` |
| Scoop | Under `scoop/apps/shep` | `scoop update shep` |
| winget | Under WinGet's `Packages` | `winget upgrade shep-pm.shep` |
| Chocolatey | Under `chocolatey/lib/shep` | `choco upgrade shep` |
| `.deb` | `/usr/bin/shep`, listed in dpkg's `shep.list` | Nothing: prints the steps |
| Archive | Anything else | Nothing: prints the download URL and SHA256SUMS steps |

- On Windows shep runs no manager. The running shepherd holds `shep.exe`,
  and with no `execve` a reload stops and starts the flock. It prints the
  channel's command and `shep daemon reload`, says the flock restarts, and
  goes on to the dogs. Revisit once each manager's handling of a locked
  exe is measured.
- The manager runs in the foreground with the terminal, so its own prompts
  and `sudo` work.
- Afterwards shep reads the new binary's `--version`. Not newer means the
  channel has not caught up with the release yet: say so, and stop before
  the dogs.
- `version_guard.rs`'s `UPGRADE_COMMAND` becomes the detected channel's
  command, so the skew warning names the right one.

### The order

1. shep itself.
2. The shepherd reload: asks "Reload the shepherd onto 0.25.0 now?" by
   default. `--reload` skips the question. With no terminal and no
   `--reload`, it skips the reload and prints `shep daemon reload`.
3. The dogs, through step 3's upgrade. A newly declared permission still
   waits for review.

A dog's upgrade restarts it under whichever shepherd is running, so a dog
that needs the new shep version gets it only when the reload ran.

## Docs reorder

Decision 10, as settled:

| Page | From | To |
|---|---|---|
| Dogs, Dog grants, Community dogs | Part 4, Dogs | Part 2, Day to day |
| Writing a dog | Part 4, Dogs | Part 5, Machine surfaces |

Part 4 goes away and the parts renumber. `docs-nav.ts` holds the order.
`upgrading.astro` leads with `shep upgrade` and keeps the manual steps
below it. `getting-started.astro` matches the welcome.
