# Releasing shep

**0.1.0 published on 2026-08-26.** All five crates are on crates.io and
`cargo install shep` works. Much of what follows was written before that and
describes the first release specifically; it is kept because the reasoning
about publish order and version choice still holds.

**Releasing is now one act: merge the release pull request.** release-plz
opens it, and merging it tags the commit, creates the GitHub release, and
uploads. There is no tag to push by hand and no local `cargo publish`.

That covers crates.io. Shipping to Homebrew, apt and the Windows package
managers is a separate question: [distribution.md](distribution.md). The
Homebrew formula builds from the crates.io tarball and needs nothing else,
but apt and both Windows channels wait on a release carrying a binary.

## Publish order

Four of the five crates form a chain, so they go up in dependency order. Read
off the manifests:

| Crate | Depends on |
|---|---|
| `shep-core` | nothing in the workspace |
| `shep-client` | `shep-core` |
| `shep-daemon` | `shep-core` |
| `shep` | `shep-core`, `shep-daemon`, `shep-client` |
| `shep-cli` | nothing — the redirect placeholder, no code, no deps |

So: **shep-core, then shep-client and shep-daemon in either order, then
shep.** `shep-client` and `shep-daemon` do not know about each other.
`shep-cli` has no workspace dependency in either direction, so it can go up
whenever — first, last, or wherever `cargo publish --workspace` schedules it.

You do not have to drive that by hand. `cargo publish --workspace` computes
the order itself and, unlike five separate `-p` runs, resolves the
inter-member dependencies against the local workspace instead of demanding
they already be on the index. That is the one-command form, and it is what
the sequence below uses.

## Version: `0.1.0`, and what the alpha train was for

A crates.io version is permanent. Yanking hides a version from resolution but
never frees the number, so a version can be spent exactly once and never
corrected. That is why the first publish went out as `0.1.0` rather
than `0.1.0`.

That reasoning was half right, and it is worth keeping the half that held. A
workspace's first publish is where packaging faults surface: a readme path
that does not resolve, a docs.rs build that fails on a unix-only dependency,
an inter-crate version requirement that does not match what actually went up.
Those are unfixable in place. On an alpha train the fix is `-alpha.2` and
costs nothing; on `0.1.0` it is `0.1.1` plus a yank, on four crates, in
public. Treating the first publish as a rehearsal is sound and it is what
this project did.

The other half did not hold. The argument was that a pre-release cannot be
picked up by accident, since semver excludes it from ordinary matching. True,
and close to worthless here, because `0.x` already carries that meaning: the
semver spec says major version zero is for initial development and anything
may change at any time. The suffix stacked a second instability signal on a
version that already said it.

What it charged for that, measured on 2026-08-26 rather than guessed:

```
$ cargo install shep
error: could not find `shep` in registry `crates-io` with version `*`
```

`cargo install` resolves `*` when given no version, and `*` never matches a
pre-release. Note the asymmetry, because it decides how much this matters:
`cargo add shep-client` copes fine, writing the exact `"0.1.0"`
requirement itself. Only `install` breaks. For a library that would be a
footnote. shep is a CLI whose primary distribution channel is
`cargo install`, so it was the headline command in the README, in
getting-started, and on the landing page, each needing a version flag and a
paragraph explaining cargo's resolution rules.

So `0.1.0` it is, and the alpha stays on the registry unyanked. It works when
named explicitly, and yanking is for releases that are broken or unsafe
rather than superseded. Publishing `0.1.0` needs no yank in any case:
`0.1.0` sorts above `0.1.0`, so it is an ordinary forward bump.

Keep the rehearsal habit for future risky publishes. Drop the assumption that
a pre-release suffix is free, because on a binary crate it is not.

### The version bump touches two places

`[workspace.package] version` is one of them. The other is
`[workspace.dependencies]`, where `shep-core`, `shep-daemon` and `shep-client`
each carry a literal `version = "0.1.0"` beside their `path`. Cargo strips the
path at publish time and substitutes that literal, and there is no
`version.workspace = true` shorthand inside a dependency entry, so the two
have to be edited together.

Getting this wrong is a silent failure with a loud symptom. If the package
version and the dependency literals disagree, the published `shep-client`
asks for a `shep-core` that the version actually uploaded does not satisfy.
The pre-release train made this especially easy to hit, since `^0.1.0`
excludes every `0.1.0-alpha.*` by semver, but any mismatch does it.
`shep-core` publishes fine and `shep-client` fails to resolve, at the point
where three of four crates are already permanent.

The five lines to change in the root `Cargo.toml`:

```toml
[workspace.package]
version = "0.1.0"

[workspace.dependencies]
shep-core = { path = "crates/shep-core", version = "0.1.0" }
shep-daemon = { path = "crates/shep-daemon", version = "0.1.0" }
shep-client = { path = "crates/shep-client", version = "0.1.0" }
```

If this drifts again on the next bump, `cargo-release` mechanises it.

## Tag: `v0.1.0` was manual; release-plz's default is one tag per package

`v0.1.0` was tagged by hand, in the sequence below, before release-plz owned
any of this. The reasoning that follows describes the target shape for that
manual tag, and it does not describe what release-plz, as configured, does.

`release-plz.toml` sets no `git_tag_name` or `git_tag_enable`, so each
release-enabled package gets release-plz's own default for a multi-package
workspace: `{{ package }}-v{{ version }}`. `shep-core`, `shep-client`,
`shep-daemon` and `shep` share a `version_group`, so the version number
agrees across all four, but the tags do not collapse into one:
`shep-core-v0.1.1`, `shep-client-v0.1.1`, `shep-daemon-v0.1.1`,
`shep-v0.1.1`. `shep-cli` carries `release = false` and gets neither a tag
nor a release.

A single shared tag is still possible: release-plz's own single-tag recipe
disables `git_tag_enable` workspace-wide and re-enables it, named
`v{{ version }}`, on one representative package. It is not configured here,
so it is not what the next release does. Whether to add that configuration
is the maintainer's call, not this fix's to make for her.

All five crates share a single workspace version, and the four with code are
released together. `shep-cli`, the redirect placeholder, carries
`release = false` and is versioned along with the rest without ever being
published by release-plz. So one annotated tag on the release commit would be
the honest shape, if configured. Per-package tags are what a workspace whose members version
independently would want; adopting that scheme here, as the default already
does, produces four tags that can only ever hold the same number.

Keep the `v` prefix, in either shape. It is what GitHub's release UI and most
changelog tooling expect.

## Commits can go missing from the bump, and the guard writes them back

**release-plz does not read one `git log`.** It walks each package's history a
commit at a time, checking each one out and asking the commit it is standing
on for its own next ancestor:

```bash
git log --format=%H -n 2 -- crates/shep-daemon
```

That walk is sound on a line of history and unsound on a diamond. Two pull
requests cut from the same base and merged one after the other make one: the
walk enters at the second merge, follows whichever leg it steps onto first,
and never crosses to the other. Every commit on the far leg is invisible to
the version bump and to the changelog alike.

Measured 2026-09-20 against release-plz 0.3.160.
[535](https://github.com/shep-pm/shep/pull/535) and
[536](https://github.com/shep-pm/shep/pull/536) were both cut from `12ddcdc2`
and merged in turn. `ed89a01b`, `refactor(daemon)!: carry the real error in
BootError::Adopt`, sat on 535's leg, and the release pull request came out as
0.8.5 with no changelog line for it. cargo-semver-checks passed, having no
lint for a changed variant payload under `#[non_exhaustive]`, so nothing else
objected either. The same commits rebased into a line produced 0.9.0 with the
entry present, which is what pins the cause on the topology rather than on
`release-plz-changelog.toml`.

Measured again 2026-10-13, the same shape at a larger scale.
[#807](https://github.com/shep-pm/shep/pull/807),
[#803](https://github.com/shep-pm/shep/pull/803) and
[#812](https://github.com/shep-pm/shep/pull/812) were cut from one base and
merged in turn, and release pull request
[#809](https://github.com/shep-pm/shep/pull/809) came out at 0.13.1 with
`feat(daemon)!: list every known dog at rest` missing from the bump and none
of the three in its changelogs. Run against that branch as it stands, the
script reports 0.14.0 and a line for each of the three.

`git log --full-history` prunes neither leg, and
`scripts/fix-release-bump.py` reads that range. Whatever it finds that the
release pull request does not already account for, it writes onto the release
branch and pushes:

- the version, in `[workspace.package]`, in the `[workspace.dependencies]`
  literals and in `Cargo.lock`, raised only when the whole range implies a
  larger bump than the one release-plz took, and measured by release-plz's own
  rule: a break moves the minor from 0.x and the major from 1.0, a `feat` moves
  the minor from 1.0, or pre-1.0 where `features_always_increment_minor` is
  true for a package the feature reached, and a pre-1.0 `feat` is a patch. The
  option is worked out per package, a package's own value over the `[workspace]`
  one, and one shared version takes the largest bump any package it reached asks
  for
- the changelog line each commit owes each package, written under the heading
  release-plz already wrote for that release, grouped and filtered by
  `release-plz-changelog.toml` the way release-plz would have written it, a
  breaking one marked `**(BREAKING)**`
- the dog index, `web/public/dogs.json`: its top-level `shep` is set to the
  version the branch ends up carrying, right after `version`, in the same commit
  and push. release-plz never touches that file, so on every release branch it
  names the previous shep until the guard moves it. It goes in the guard because
  a commit status only attaches to the sha it names, and a second push after the
  guard would leave the new head without its `release bump` status. A branch
  whose version and changelogs are right but whose index is stale is rewritten
  too. A branch with no readable index is refused
- the pull request title, with only the number in it moved
- the `release bump` status, on the head it just wrote

It writes to the release branch and nowhere else, so nothing publishes: the
release still waits for a human to merge it. release-plz rewrites the branch on
every push to main, so the guard runs again each time and corrects it again,
and a branch it already corrected is left alone.

The bump rule is release-plz's and not a stricter one. Measured across this
repository's own 74 tag transitions, every minor bump carries a breaking commit
and every range with a `feat` and no break came out a patch. Replayed against
three releases this repository shipped, `0.12.0` to `0.12.1`, `0.12.3` to
`0.12.4` and `0.12.4` to `0.12.5`, each a patch that shipped `### Added`
entries, the script reports the number the release already carries and moves
nothing. Replayed against `0.6.1` to `0.6.2`, where a break did ship as a
patch, it reports 0.7.0. `release-plz.toml` sets
`features_always_increment_minor` nowhere, so every package sits on
release-plz's default.

It refuses rather than push when it cannot read the branch or its dog index, and when the version
it worked out is not one it can actually write: after the rewrite it re-reads
`[workspace.package]`, the `[workspace.dependencies]` literals and `Cargo.lock`
with a TOML parser and refuses if any of them still names the old version. A
release pull request titled one version and built at another is the mistake this
guard exists to catch, so it is the one outcome the guard will not produce.

`release-plz-pr.yml` runs it as the middle of that step's three guards. The
other two still fail for a human: a release pull request that bumps no version,
and one whose new changelog section is empty, are both shapes a person has to
decide which case they are.

`scripts/test_fix_release_bump.py` builds a throwaway repository with a diamond
in its history, a release branch cut the way release-plz cuts one, and a bare
remote standing in for github.com, then asserts the branch comes back at 0.14.0
with a changelog entry for every commit the walk missed, and that the same
fixture with the break taken out leaves 0.13.1 where it is. `manifests.yml` runs
it.

What the guard does not do: it does not fix the walk. The next release pull
request is built from the same under-read range and corrected again.

### Auditing what already shipped

`scripts/audit-changelog-gaps.py` is the read-only companion for releases
that already merged. For every crate with a `CHANGELOG.md` and every
released version, it reads the commits between the previous release tag and
this one that touched the crate's directory, keeps the ones
`release-plz-changelog.toml` would have given a changelog line (`feat`,
`fix`, `perf`, a non-skipped `refactor`, anything breaking), and reports the
lines that version's section does not carry. Run it from a full-history
checkout with `python3 scripts/audit-changelog-gaps.py`: it prints four
counts per crate (released sections, empty sections, empty sections that
owed an entry, missing lines), then one line per gap, and writes nothing.
It reads tags by the config's `tag_pattern` the way git-cliff does,
unanchored, so every per-package tag matches and a tag on any crate dates
the shared version for all of them; `shep-v0.9.0` is the `shep` package's
own tag, not a stray one, and `pre-rebase-backup` matches nothing. A
pre-release tag bounds the next range but is never itself audited, since no
changelog section carries one. `scripts/test_audit_changelog_gaps.py` builds
a throwaway two-crate repository for it, and `manifests.yml` runs those
tests.

The structural fix is a history with no diamonds in it. Requiring branches to
be up to date before merging would deliver that, at the cost of re-running
sixteen required checks on every open pull request each time anything merges.
Whether that trade is worth it is the maintainer's call.

## The sequence

**Every release after the first is one merge.** release-plz opens the pull
request, you merge it, and the resulting push tags, releases and uploads.
Nothing below is something you run.

What follows is how 0.1.0 was done by hand, kept because the checks in it are
the ones release-plz now performs for you, and because knowing what they were
is what lets you tell whether it did them. Run it from a clean `main` with
everything committed, one cargo command at a time, since the workspace shares
one target-dir build lock.

```bash
# 1. Bump both places, per the section above, then reconcile the lockfile.
$EDITOR Cargo.toml
cargo check --workspace --all-features

# 2. Move each crate's [Unreleased] section to [0.1.0] with today's
#    date, in all four real crates' CHANGELOG.md files. shep-cli (the
#    redirect) ships no code and keeps no CHANGELOG — nothing to log.
$EDITOR crates/*/CHANGELOG.md

# 3. The task gate, one command at a time, $? read directly and never
#    through a pipe. fmt and clippy run on 1.93 like CI, not on `stable`:
#    docs/testing.md#why-linting-runs-on-193 (install it once there).
cargo +1.93 fmt --all -- --check
cargo +1.93 clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features

# 4. Rehearse the whole publish. Resolves inter-member deps locally, so it
#    exercises all five rather than stopping at the first unpublished one.
cargo publish --workspace --dry-run

# 5. Commit the release, push it, and let CI run the gate on it.
git add -A
git commit -m "chore(release): 0.1.0"
git push origin main

# 6. Once that is green, tag the pushed commit and push the tag.
git tag -a v0.1.0 -m "shep 0.1.0"
git push origin v0.1.0
```

**Merging the release pull request is the irreversible step.** There is no
tag to push and no local `cargo publish` in the sequence.

Two workflow files, one job each. `release-plz-pr.yml` maintains a pull
request that bumps `[workspace.package]` and writes the changelogs from the
conventional commits since the last release, on every push to `main`. Merging
it is a hand decision, so several merges to `main` stack into one release.
`release-plz-release.yml` runs on the push that merge makes and does nothing
unless a manifest names a version that is not on crates.io. It tags, creates
the GitHub release, and uploads in dependency order.

**So a release needs one human step: merging the release pull request.**
It stopped merging itself on 2026-09-19 (`e7a7dffb`), so a release waits for
someone to decide the stacked changes are worth a version. The workflow turns
red on a release pull request that bumps no version, and the `crates-io`
environment will hold the publish if a required reviewer is ever configured
there.

**One check is yours, deliberately.** `benches/versus-pm2/versus-pm2.sh
--check` compares shep's performance with the committed baseline, and it
needs macOS and a quiet machine, so no workflow runs it. Run it before
merging a release pull request. An exit of 1 names the metric that
regressed; it does not block the merge by itself. Whether a regression ships
is the maintainer's call, and the check exists so that call is made before
the release rather than found afterwards on a docs page, which is how #291
and #292 were found. [testing.md](testing.md) has what each exit code means
and when to record a new baseline.

The token lives in that workflow's `crates-io` environment, so a laptop never
needs one. That environment is also where a required reviewer goes if this
ever wants a human gate between merging and spending a version number.

**This used to be split**, with a hand-written `release.yml` publishing behind
a `v*` tag, and the split was wrong. `release-plz.toml` records why at length.
The short version: merging a pull request titled "chore: release vX.Y.Z" is a
more deliberate act than typing a `git push` of a tag, and the split's only
visible effect was that merging the release PR did nothing and surprised the
person who merged it.

**The gate moved out of the workflow and into a branch ruleset.** Until
2026-08-27 the publish job triggered on `test.yml`'s `workflow_run` rather
than on `push`, so that it could require a green run against the exact commit
it was about to publish. `main` now carries a ruleset requiring `lint`,
`docs`, `typos`, the `test` matrix, `features`, `musl` and `minimal-versions`,
which means an untested commit cannot reach `main` for the publish job to find.
The gate had nothing left to catch, so it went, and the workflow matches
zendriver-rs's shape.

Read that as a dependency, not a simplification: delete the ruleset and
nothing checks anything before an upload. `slow`, `coverage`, `bench`,
`privileged` and `windows-gnu` are deliberately not required. `slow` is the
serial timing tier and has been the whole of CI's red for four consecutive
runs; requiring a job that fails on a contended runner would block merges
without telling anyone anything.

**The release pull request is squashed, not merged.** release-plz 0.3.160,
which the pinned action runs, substitutes the pull request's head commit for
the commit it was handed when that commit is a merge commit, so `cargo publish`
would package a tree that never landed on `main`. A squash leaves nothing for
that lookup to find. The old workflow carried a step that refused merge
commits outright; squashing removes the case instead of detecting it.

If CI is unavailable and the publish has to happen from a laptop, the token
goes in `CARGO_REGISTRY_TOKEN` and the command is `cargo publish --workspace`,
after the same rehearsal. Ordering is cargo's problem, not yours. To go crate
by crate and watch each land instead:

```bash
cargo publish -p shep-cli
cargo publish -p shep-core
cargo publish -p shep-client
cargo publish -p shep-daemon
cargo publish -p shep
```

`shep-cli` has no dependency ordering to respect — it is listed first only so
it is out of the way early, not because it must go before anything else. Each
of the last three real crates waits for the previous one to appear in the
index. Recent cargo polls for that on its own; if a run fails with `no
matching package named 'shep-core' found` immediately after `shep-core` went
up, wait a minute and rerun the one that failed.

Afterwards, the install line becomes:

```bash
cargo install shep --version 0.1.0
```

`shep` carries three `[[bin]]` targets since Phase 15's library
extraction, not one — this single command installs all three binaries:
`shep`, plus the two container-entrypoint aliases `shep-runtime` and
`shep-dev`.

The README's status block, badges, and "Try it" section already carry this
command as of 2026-08-16, ahead of the actual publish, per the maintainer's call that
the docs should describe the install rather than wait for it. Nothing to
edit there at publish time; just confirm the badges resolve once the crates
are up.

## `shep-log-rotate` publishes after this, not alongside it

`github.com/shep-pm/shep-log-rotate` is the first external dog and it
depends on `shep-client`. Until `shep-client` is on the index, that crate
cannot be published at all, so the order across the two repositories is:
everything here first, then the dog.

Its manifest is already prepared for the swap. `shep-client` is named there
with **both** a version and a git URL, so cargo builds it from git today while
`cargo package` still finds the version requirement it demands. Once
`shep-client` is up, the git and branch keys come out of that one line and
nothing else changes.

Two things worth knowing before doing that:

- **Its lockfile pins a shep commit, not a version.** Whoever swaps the line
  should rebuild and re-run the dog's own suite against the published crate
  rather than assuming the git build proved it. The published tarball excludes
  files a git checkout has, and that difference is exactly what a git
  dependency cannot test.
- **The version is `0.1.0`**, matching this workspace, so the number
  does not promise more than an alpha.

## What is a blocker and what is not

The point of this section is that the decision in the morning is informed. A
`0.1.0` promises a working thing on macOS, Linux and Windows that is not
finished. It does not promise a stable API or a complete v1.0 surface.

### Blockers

**Nothing outstanding, and this is no longer a prediction.** `0.1.0-alpha.1`
published on 2026-08-26: all five crates went up, in cargo's own order,
within four seconds. The list below is what held, not what somebody hoped
would hold.

- The task gate is green: `cargo +1.93 fmt --all -- --check`, `cargo +1.93 clippy
  --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace
  --all-features` at 1716 passed / 0 failed.
- Every crate has `description`, `readme`, `keywords` and `categories`, and
  every category is a real crates.io slug. A category the registry does not
  recognise is rejected at upload, after earlier crates in the chain have
  already gone up permanently.
- The version bump reaches both `[workspace.package]` and the three
  `[workspace.dependencies]` literals. See the section above for why this one
  is a blocker and not a nicety.
- `cargo publish --workspace --dry-run` is clean at the version you are about
  to publish, not at the version you dry-ran last week.

### Not blockers

**CI runs automatically now.** `push` to `main` and `pull_request` trigger the
full workflow as of 2026-08-16; before that it was `workflow_dispatch` only,
kept off while the repository was private and Actions billed macOS at 10x.
The repository is public and standard runners are free, so that arithmetic is
history rather than a live constraint. The
weekly `schedule` row stays off for the same billing reason, since a full
19-job run against an unchanged tree spends the expensive part of the file to
learn nothing. The gates it runs are the same ones that run locally, and they
are green.

**Windows runs.** It was zero when this section was written, and the entry
survives because what it says about a `1.0.0` still holds: an alpha is
allowed an unsupported platform and a `1.0.0` is not. The day-to-day tier is
built now and `windows-latest` runs the suite in CI, so this is no longer the
thing standing between an alpha and a release. Three refusals are permanent
and deliberate: no graceful `stop` outside the shepherd channel, no
`shep startup`, and no `user`/`group`. The smaller gaps that are not
permanent, among them most `shep signal` names and the unix file modes, are
in [specs/deferred.md](specs/deferred.md) rather than repeated here.

**The v1.0 build queue is empty.** `shep serve`, `shep dev` and `shep
runtime` shipped in Phase 15, lookout's search/filter, its action keys, and
lambs in its detail pane shipped in Phase 16, and OTLP export on the metrics
dog shipped 2026-10-14 with the channel samples that were its missing half.
Nothing [specs/deferred.md](specs/deferred.md) names as v1.0 scope is unbuilt
now. The version is an alpha for what the rest of this file names — three
init scripts no host has ever booted, the permanent Windows refusals, the
v1.1+ cuts the spec argues for — and not for a verb or a surface that does
not exist. `.js`
Flockfiles, the schemars schema, the CLI-flag config layer, and openrc/BSD
`rc.d` units shipped in Phase 14, see the two paragraphs below for what that
means for this release, specifically.

**The three new init scripts are rendered and pinned by exact-string tests,
and have not been executed on FreeBSD, OpenBSD, or an openrc host.** No such
host exists in this project. Nothing claims support for those platforms
until somebody reports back from one; `shep startup` will happily render a
script for an init system nobody here has ever booted it under.

**`crates/shep-core/assets/flockfile.schema.json` is a committed, generated
artefact that ships in shep-core's tarball**, because
`crates/shep-core/src/config/flockfile.rs` `include_str!`s it — that is why
it lives inside the package directory rather than at the repository root.
Regenerate it with `cargo run -p shep -- schema >
crates/shep-core/assets/flockfile.schema.json` before a release if
`AppConfig` changed, though the drift test in `flockfile.rs` will have told
you already: `cargo test -p shep-core` fails first.

**Docs.rs has never built these crates.** It cannot until they are published,
so this is unknowable in advance rather than skippable. `RUSTDOCFLAGS="-D
warnings" cargo doc --workspace --no-deps --all-features` locally is the best
available proxy and it passes. Check the docs.rs build status for each crate
after publishing; a failure there is fixable in the next alpha.

**The LICENSE files ship inside the `.crate` archives.** They did not until
Phase 17. `LICENSE-MIT` and `LICENSE-APACHE` live at the repository root, and
cargo only reaches outside a package directory for `readme` and
`license-file`, so every archive went out without them. Each crate directory
now carries a symlink to both, which cargo follows and packages as real files.

The `license = "MIT OR Apache-2.0"` field is still what crates.io renders and
what tooling reads. The files matter for the people the licences are actually
addressed to, who get a tarball rather than a web page.

## The `shep-cli` package was renamed to `shep`

Decided and done 2026-08-15. Checked that day: `shep`, `shep-core`,
`shep-daemon`, `shep-client` and `shep-cli` were all free on crates.io.

The binary was already called `shep` while the crate that built it was named
`shep-cli`, so users would have installed it as `cargo install shep-cli` and
then run `shep`. Renamed the package (not the directory — the checkout still
keeps `crates/shep-cli/`, since Cargo takes the published name from the
manifest, not the path) to `shep` so the install command and the binary name
match: `cargo install shep`, then run `shep`.

That leaves `shep-cli` unclaimed and sitting one character off `shep`'s own
namespace — an obvious squat target once the other three crates are visible
under a `shep-*` naming convention. `shep-cli` is published as a real
redirect crate (see `crates/shep-cli-redirect/`) for exactly that reason:
nothing was ever published under the old name, so the redirect is purely
defensive, not a migration path anyone needs to follow.

## What the rehearsal found

`cargo package --list` was read for all four crates. Every file in every
archive is source, tests, fixtures, insta snapshots, the changelog or the
readme. Nothing needed an `exclude`.

The 800K of design PNGs under `docs/shep-design/`, the `benches/` tree, the
`web/` directory and the pm2 migration fixtures at the repository root are all
outside the four package directories, and cargo packages only what lives under
the crate it is building. They were never in danger of shipping.

The largest archive is `shep-daemon` at 1.9 MiB uncompressed, 520 KiB
compressed, which is `supervisor.rs` and `boot.rs` being large source files.
That is well inside the 10 MiB registry limit.

## Releasing a dog

A dog's own release builds its prebuilt binaries and proposes its index row
through `.github/workflows/dog-release.yml`, a reusable workflow in this
repository. The calling job is in
[Writing a dog](../web/src/pages/docs/writing-a-dog.astro). What to know on
this side:

- **The index row is a human's merge.** The workflow opens a branch
  `dog-release/<package>-<version>` on this repository and a pull request
  that changes `source.version`, `source.sha256` and `source.permissions` of
  that one row, through `scripts/dog-index-update.py`. It hashes the assets
  the release serves, not what each leg built. A first row for a new dog is a
  pull request by hand.
- **The permissions come off the built binary.** The
  `x86_64-unknown-linux-gnu` leg runs the staged binary with
  `--shep-manifest` and keeps the answer as `manifest.json`, uploaded as its
  own artifact `manifest-x86_64-unknown-linux-gnu`. A dog declares one
  manifest for every target, so one leg is enough, and it is the read-only
  `build` job that runs the binary. `index-pr` downloads the artifact with
  `gh run download`, which is why that job holds `actions: read` too, and
  hands the path to the script as a sixth argument. The script copies each
  permission's `name` and `required`, in the manifest's order, and nothing
  else the manifest carries.
- **Reading the manifest is bounded.** The probe runs the staged binary under
  `timeout -k 5 60`. A dog that ignores `--shep-manifest` and starts its loop
  never returns, and a probe with no bound would hold `build` to its 60-minute
  job timeout, cancelling `upload` and `index-pr` with it. A dog that runs on
  past 60s is killed and treated as a dog that answered nothing.
- **A manifest is optional.** A binary that does not answer
  `--shep-manifest`, runs past that bound, or answers something other than one
  JSON document with a permissions list uploads no manifest artifact and says
  so in the step summary; the release goes on. `jq -s` is what reads the answer
  so a binary printing two documents fails here rather than in `index-pr`,
  after the release is out. A row written with no manifest carries no
  `permissions`, and a list an earlier version left in the row goes out with
  the version it described. The manifest artifact is named beside the
  `dist-*` pattern `upload` downloads, never inside it, so it never becomes
  an asset on the release.
- **A manifest that exists has to download.** `index-pr` asks the run which
  artifacts it has before fetching, because `gh run download` fails the same
  way whether the artifact was never uploaded or the download broke. An absent
  artifact is the dog's answer and the row is written without permissions; a
  listed one that will not download fails the job, so a broken run never writes
  a row missing permissions it did read.
- **Building and uploading are separate jobs.** `build` holds a read-only
  token, since it runs the dog's build scripts and one can prepend to
  `GITHUB_PATH` to swap a tool a later step runs. `upload` runs none of the
  dog's code: it fetches the built files with `gh run download`, which is why
  the caller grants `actions: read`, and attaches them. `index-pr` hashes
  only the names `upload` reports, so a re-run with fewer targets leaves
  nothing from an older run in the index.
- **The token is the dog repository's secret.** `index-token` has to push a
  branch and open a pull request here; `GITHUB_TOKEN` cannot cross
  repositories. A pull request opened with it fires no checks, so use a PAT
  or an App token, as `release-plz-release.yml` does.
- **The targets are `release-artifacts.yml`'s, copied.** A matrix cannot
  cross workflow files, so `scripts/test_dog_release_targets.py` fails
  `manifests` until a target added to one is added to the other.
- **Dry run:** Actions, "Dog release", Run workflow, with the dog's package,
  an existing tag and its `owner/name`. The dog's repository has to be
  public, since the checkout uses this repository's token. It builds every
  target and prints each hash in the log and the job summary. It uploads
  and opens nothing, and refuses a real run from here since this
  repository's token cannot write to the dog's.
- **The row has to match the release.** The script refuses a row whose
  `repo` is not the repository the release was built from, and one whose
  `source.kind` is not `cargo`. It hashes only the files named
  `<package>-<version>-<triple>`, plus `.exe`, so a sidecar on the release is
  skipped.
