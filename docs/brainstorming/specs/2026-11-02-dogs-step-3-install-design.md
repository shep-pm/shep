# Dogs step 3: install, upgrade, receipts

Status: settled with the maintainer in a grill on 2026-11-02. Step 3 of
[Dogs as plugins](2026-10-12-dogs-as-plugins-design.md), tracked in #1075,
replacing #357. Spec 1's decisions stand except where this amends them.

## What stands from spec 1

- Every dog prompts, through the grant prompt `adopt` and `enable` already
  show. Install adds no prompt of its own.
- An index row approves one exact version.
- `shep install <dog>`, bare `shep install`, `shep upgrade <dog>` and
  `--dogs`. Bare `shep upgrade` waits for #1434.
- The CLI touches the network. The daemon never does.
- Confinement is not planned. Grants are the control.

## Sources

| Flag | Installs | Approved by |
|---|---|---|
| none | The prebuilt binary for this target, else a build of the index's version | The index row |
| `--cargo` | `cargo install --locked --version <row's version> <package>` | The index row |
| `--ref <ref>` | `cargo install --locked --git <row's repo>` at a branch, tag or commit | Nobody: the operator's choice |

- Only `cargo` rows install. Other kinds print their instructions, as
  `shep dogs --available` does.
- `--ref` takes the row's own repo only. A build from anywhere else is
  `shep adopt <path>`, and there is no `--binary`.
- No prebuilt for this target and no `cargo` on `PATH`: refuse, naming
  both and linking rustup. shep never installs a toolchain.

## The index row

- A `cargo` row must carry an exact `version`. A row without one is listed
  but not installable.
- Prebuilt binaries are a sha256 per target triple. The URL is built, not
  stored: `<repo>/releases/download/v<version>/<package>-<version>-<target>`,
  with `.exe` on Windows. The asset is the bare binary, no archive, so no
  archive crate is added.
- The target must match the triple shep was built for, exactly.
- A version bump is a reviewed PR to `web/public/dogs.json`. A first-party
  dog's release opens it through a reusable workflow in this repo, which
  builds every target, uploads the binaries and writes their hashes.
- The CLI caches the index at `$SHEP_HOME/cache/dogs.json` for 24 hours,
  and `--refresh` forces a fetch. A failed fetch falls back to the cache
  with one line, and refuses only with no cache at all.

## Integrity

| Path | Check |
|---|---|
| Prebuilt | The download's sha256 must equal the row's, checked before the file is written into place |
| `--cargo` | crates.io's `trustpub_data.repository` naming a repo other than the row's refuses. `null` (a token publish) is one line in the prompt. A response without the field, which crates.io marks unstable, skips the check |
| `--ref` | One line in the prompt: built from `<ref>` at `<sha>`, which the index has not reviewed |

- An asset download follows exactly one redirect, from `github.com` to
  GitHub's asset host, over HTTPS. The index fetch still refuses every
  redirect.
- cargo verifies a crate's checksum itself. Neither crates.io nor GitHub
  gives shep a signature to check, and no cargo-vet query works outside a
  project, so neither is in this step.

## On disk

- `$SHEP_HOME/dogs/<name>/<version>/` for an index install, and
  `ref-<sha>/` for a ref, through `cargo install --root` or the download.
  `--locked` always. A ref's commit is read from the `.crates2.json`
  cargo writes into the root, so shep needs no `git`.
- `$SHEP_HOME/dogs/<name>/receipt.json`: the index URL and the row as
  approved, the package, the version or the ref and its commit, the full
  installer command line, the binary's sha256, the manifest as installed,
  and the time.
- The receipt's manifest is a record, compared at upgrade and read by the
  columns. Lookout still asks the binary for its schema fresh.
- A lock file per dog name. A second install or upgrade of the same dog
  waits, then refuses naming the lock.
- `shep delete` on an installed dog removes its directory and receipt. An
  adopted dog's file is never removed.

## Flow

- `shep install <dog>`: fetch, then the existing grant prompt, then
  `enable`. A no at the prompt leaves it installed, adopted and disabled.
  `--no-enable` skips the last step.
- Missing dependencies from the manifest install first, each with its own
  prompt. One missing from the index, a range the index's version does not
  meet, or a cycle refuses. A no on a dependency stops the install.
- Bare `shep install` lists what `shep dogs --available` lists, with a
  `shep install <name>` line for each installable dog. Browsing waits for
  step 5.

## Upgrade

- `shep upgrade <dog>` follows the receipt: an index install moves to the
  index's version, a ref install rebuilds its ref at the newest commit.
- `--ref <other>` switches ref, `--release` returns to the index's
  version, `--rollback` puts back the version the last upgrade replaced.
- It installs into a new directory, points the adopted path there, writes
  a new receipt and restarts the dog. The previous directory is kept for
  `--rollback`.
- A permission the new manifest declares waits for review, as `upgrade.rs`
  already does, while the dog keeps its old grants.
- The cargo path prints `https://diff.rs/<crate>/<old>/<new>`.

## Columns

- SOURCE: `built-in`, `installed` (shep-pm), `installed, community`, or
  `adopted` (a path, no receipt). A fact, never a gate.
- VERSION: what the manifest reports, with `→ <new>` when the cached
  index holds a newer version. Never from the network.

## Decisions this reverses

Each gets a `docs/decisions.md` entry when it lands:

- #357's "print, never execute": shep now runs the installer.
- Rule 7 of the dog config spec ("asked fresh, never stored") narrows: the
  receipt stores the manifest as a record of what was approved, and
  anything that acts on a schema still asks the binary.
- `docs/specs/deferred.md`'s "`shep install` does not exist" moves out of
  deferred.

Prebuilt downloads move here from step 6, so #1434 keeps only the
on-ramp and shep upgrading itself.
