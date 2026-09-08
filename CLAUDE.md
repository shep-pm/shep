# shep — CLAUDE.md

Clean-room Rust process manager (daemon + CLI + client lib), inspired by pm2's
*feature list only*. License: MIT OR Apache-2.0. Sheep/sheepdog branding
throughout. Published at `github.com/shep-pm/shep`; the local checkout
directory is still named `pm2-rs`, which is expected and not a rename to make.

## ⚠️ Clean-room rule (non-negotiable)

**Never open, read, or port source from `~/GitHub/pm2` during
implementation.** That repo was read once, by a dedicated trace phase, to
produce our behavior specs — implementation works from the specs alone:

- [docs/systematic-refactor/refactor-workspace/map.md](docs/systematic-refactor/refactor-workspace/map.md) — the spec for the pm2-DERIVED module set, and only that. It is
  accurate and drift-annotated through roughly Phase 10, with a partial Phase
  15 pass, and it stops there. It has no mention of lookout, whistle, `shep
  stock`, `shep signal` or shep-cli-redirect, and it still calls the TUI
  `tui.rs` and the MCP server `mcp.rs`, which are the names those two shipped
  under before Phases 12 and 13 renamed them. For anything after the pm2
  cutover, the design lives in docs/brainstorming/specs/ and the reasoning in
  [docs/decisions.md](docs/decisions.md). This line said "THE spec: every module's behavior" until an audit
  on 2026-08-29 counted the gaps, which is a bad claim to leave in the file
  every session reads first.
- [docs/systematic-refactor/refactor-workspace/](docs/systematic-refactor/refactor-workspace/) — goals.md (must-haves, constraints, open questions), assessment.md (keep/toss verdicts), trace.md + trace/ (flow inventories, known-bug list — bugs are documented so we do NOT reproduce them)

"Compat"/"contract" language in those docs means fidelity to the spec, not to
pm2's artifacts. `~/GitHub/rand` is the style reference — read freely.

## Commands

MSRV 1.88, edition 2024. A no-op rebuild is 0.35s, so a slow run is never
compilation. It is test execution, and almost all of it is one class of test.

**The numbers behind every choice here, and the reasoning that stops each one
being "simplified" into something slower or less honest, are in
[docs/testing.md](docs/testing.md). Read it before changing a command.**

### The inner loop, while iterating

```bash
cargo test -p shep-daemon --lib --all-features -- --skip ::slow::
```

The skipped tests wait on real FSEvents or real elapsed time. Run the
unfiltered lib suite when touching `watch/`, `extras.rs` or the sampler.

shep-scoped work needs both halves, since Phase 15 made it a library with thin
bins over it:

```bash
cargo test -p shep --lib --bins --all-features -- --skip ::slow::
```

### The task gate, once, when the task is otherwise done

```bash
cargo fmt --all --check
```
```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```
```bash
cargo test --workspace --all-features
```
```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
```

**Bare `cargo test --workspace`, deliberately, not `--lib --bins`.** The global
rule preferring `--lib --bins` was measured on a project where doctests
dominated and does not transfer: this workspace's cost is the integration tier,
which `--lib --bins` skips rather than speeds up.

One cargo command at a time (the workspace shares one target-dir build lock),
each from its own command with `$?` captured directly, never through a pipe.

**If the task changed anything an operator types or sees**, the gate has a
fifth step in `web/`. See the docs trigger below.

### Per phase, not per task

```bash
cargo check -p shep-daemon --all-targets --all-features --target x86_64-unknown-linux-gnu
```
```bash
cargo check --workspace --all-targets --all-features --target x86_64-pc-windows-gnu
```

Linux reaches `notify.rs`'s abstract-namespace branch, which a macOS build never
compiles. Windows needs `brew install mingw-w64` for `ring`'s build script.
`cargo check`, not clippy: 51 dead-code warnings fall out of `cfg(unix)` code
that is not dead anywhere we ship.

**The local gate does not cover Linux or Windows. Read the CI result before
calling a branch green.**

### On a dependency change

```bash
cargo deny check
```

### At a merge

The four above, plus `cargo test --workspace --all-features -- --test-threads=1`
and both `benches/` gates. The serial run has caught a real regression.

## Subagent dispatch

- **Writing plans:** Opus, extra thinking. Plans carry the design work; a thin
  plan spends its cost later, in review loops.
- **Implementing a written plan:** Sonnet, high thinking. The design decisions
  are already made.
- **Every brief says to use conventional commit subjects, and says it in the
  brief rather than trusting it to be known.** `type(scope): summary`, with a
  `!` on the commit that actually breaks something, in the crate that breaks.

  This is a release-correctness rule, not a style one. release-plz walks the
  INDIVIDUAL commits and `filter_unconventional = true` drops whatever does
  not parse, so an unreadable subject contributes nothing to its crate's
  changelog and nothing to the version bump. The `!` on a pull request title
  is read by nobody: release-plz ignores merge commits, which is the opposite
  of what a `merge_commit_title = PR_TITLE` setting suggests.

  Measured 2026-09-04, and it is the reason this bullet exists. Of the 31
  commits behind `shep-core` 0.2.1, 19 were unreadable, and the split was
  exact: every readable one was written in the main thread, every unreadable
  one by an implementer subagent. One of the 19 changed `BusEvent::topic`'s
  signature, so a source break went to crates.io as a patch with an empty
  changelog section. Nothing failed. `semver_check = true` did not catch it
  either, having no lint for a changed inherent-method return type.

  Nine types are accepted, and they are what `release-plz-changelog.toml`
  handles rather than the conventional-commits list. `feat`, `fix`, `perf`
  and `refactor` produce entries; `docs`, `test`, `ci`, `chore` and `style`
  are `skip = true` and drop when not breaking, except
  `chore: update Cargo.{toml,lock} dependencies`, which has its own parser
  ahead of that skip. `revert` and `build` are refused, because they match no
  parser and `filter_commits = true` discards them as silently as it discards
  a sentence.

  **A `!` is the second half of the rule, never a shortcut past the first.**
  `protect_breaking_commits = true` outranks a `skip = true`, so `docs!:` and
  `chore!:` are kept and arrive marked BREAKING. It does not outrank a miss,
  so `revert!:` and `build!:` vanish, and neither does it rescue a subject
  that never parsed: `Tell a running dog its config changed!` produces
  nothing, measured. The commit that changed `BusEvent::topic` needed to be
  `refactor(core)!:`, and no shorter fix would have saved 0.2.1. A conventional
  type gets the commit seen; the `!` then decides the bump. All measured with
  git-cliff 2.14.1 against the real config.

  `.github/workflows/commits.yml` gates it now and `.githooks/commit-msg`
  catches it earlier, so this bullet is the explanation rather than the
  enforcement. It still belongs here, because a brief that omits the rule
  produces a branch that fails CI at the end instead of a subagent that gets
  it right at the start.

## Architecture

Seven published workspace members, one distributed binary (`shep`):
shep-core, shep-daemon, shep-client, shep-macros (the `DogConfig` derive,
reached through shep-client's re-export), shep-cli (published as `shep`),
shep-channel (the client an app links to speak the shepherd channel), and
shep-cli-redirect, a placeholder holding the `shep-cli` name on crates.io.
Each crate's Cargo.toml `description` states its role.

**The docs site is `web/`** -- an Astro site, published, and part of the
public surface. See the docs rule below; it is not optional upkeep.

Daemonization = the binary re-execs itself with a hidden `daemon` subcommand.
Module-by-module design: map.md (see above).

## Docs — hard trigger

**The `web/` docs site is published and is part of the public surface. A
change to what an operator can type, see, or configure is not finished until
`web/` says so.** That means a new or removed verb, flag, alias, `shep.toml`
key, Flockfile field, exit code, JSON payload shape, or default value.

Two halves, and only one of them is automatic:

1. **Regenerate the CLI reference.** It is generated from the real binary's
   own `--help`, so it never needs writing by hand:

   ```bash
   cargo build --release
   ./web/scripts/generate-cli-reference.sh
   ```

   `git diff` afterwards is the check. A stale copy does not fail any build,
   which is precisely why it drifts.

2. **Read the prose pages.** `web/src/pages/docs/*.astro` are hand-written
   and no generator touches them. Grep for the thing you changed before
   assuming they are fine.

Then build the site, because it can fail on content the Rust gate never sees:

```bash
cd web && npx astro build
```
```bash
cd web && npx astro check
```

**Both, and `check` is the one that catches a wrong prop.** Astro does not
typecheck during a build, so a page passing a component a prop it does not
have builds clean and renders wrong. Measured 2026-08-20: `/docs/output`
shipped two `<Callout kind="note">` against a component whose prop is
`variant`, so `variant` was `undefined`, the rendered `div` lost its variant
class and the label badge rendered empty. `astro build` was green the whole
time. `astro check` reported both, at `ts(2322)`, the moment it was run.

**Why this is a hard trigger rather than a nicety.** On 2026-08-19 the
generated reference was two days stale (919 lines of drift), and regenerating
it surfaced a real regression nobody had noticed: the grouped verb listing
that replaced clap's own `Commands:` block had silently dropped every
`[aliases: ...]`, so `shep --help` named none of the six working aliases for
several phases. The same audit found a sample Flockfile in `from-pm2.astro`
carrying a `reuse_port = true` line that had become a parse refusal that
morning -- copy-pasteable, and broken. **Nothing in the Rust gate can catch
either.** `cargo test` does not read `web/`, and `web/` had no mention
anywhere in this file until now.

## Code style — hard trigger

**Invoke the `shep-idiomatic-rust` skill before writing or reviewing ANY Rust
in this repo.** It fronts [docs/idiomatic-rust.md](docs/idiomatic-rust.md) —
47 numbered rules (IR-1..IR-47) distilled from rand 0.10.2. Cite rules as
`IR-<n>` in reviews. Evidence with file:line citations:
[docs/idiomatic-rust/lenses/](docs/idiomatic-rust/lenses/).

Top drift risks (all observed in baseline testing): panicking constructors
outside shep, `std::error::Error` instead of `core::error::Error`, missing
`# Errors` doc sections, `# Panics` without `#[track_caller]`, widening input
grammars beyond spec.

## Terminology

[docs/terminology.md](docs/terminology.md) is the lexicon: flock, fold,
Flockfile, bleats, bark (webhooks), whistle (MCP), muster, lookout (TUI),
**dogs** (plugin processes — metrics, bark — supervised by the daemon; the
daemon itself is only ever "the shepherd"), **lambs** (child processes of a
sheep — process-tree members). `sheep` = ONE managed user process (singular
only); the plural is always **flock**, never bare "sheep"/"sheeps". Rules: straight verbs
(`start`/`stop`/`list`) stay
first-class aliases; destructive ops and error text stay plain — the theme
never costs clarity.

## Gotchas

- Every new public item needs docs and a deliberate Debug decision (redacted
  for anything carrying env/secrets, with an exact-string test — IR-41).
- `#![forbid(unsafe_code)]` is LIVE in core/client/cli, not planned. Unsafe
  lives in three files across two crates, each carrying its own
  `#![allow(unsafe_code)]` or `#[allow(unsafe_code)]` with per-block
  `// SAFETY:` (IR-22/23): shep-daemon's `sys.rs` (eight sites on unix) and
  `sys_windows.rs` (ten on Windows), and shep-channel's `endpoint.rs`
  (three sites, two on unix and one on Windows: probing the descriptor the
  shepherd names in `SHEP_CHANNEL_FD` under a `ManuallyDrop` that closes
  nothing, then taking it, sound because a process-global guard makes that
  reachable at most once per process, and `PeekNamedPipe` on Windows, which
  `PipeReader`'s own doc comment exists to justify). This line said
  "planned" and named only `sys.rs` for the whole of the Windows port, then
  said "exactly two files" after shep-channel added a third, then said "one
  site" in `endpoint.rs` after it had grown a second, then said "two sites,
  one per platform" after unix grew the probe.
- Open design decisions live at the bottom of map.md and in goals.md's open
  questions — check them before making architectural calls; if a decision is
  listed there, it is the maintainer's, not yours.
- **A dev-dependency on another workspace crate names only a path**, never
  `workspace = true` and never a version. `cargo publish` strips a path-only
  dev-dependency and keeps a versioned one, and a versioned one has to
  resolve on crates.io while the crate is being packaged. shep-macros'
  dev-dependency on shep-client carried the workspace version, shep-client
  depends on shep-macros so shep-macros publishes first, and every release
  from 2026-09-04 18:24 stopped there with `failed to select a version for
  the requirement shep-client = "^0.2.1"`: shep-core and shep-daemon reached
  crates.io at 0.2.1 and 0.2.2 while shep-macros, shep-client and shep
  stayed at 0.2.0 until #131 broke the cycle on 2026-09-05.
  `scripts/check-dev-deps.py` now refuses a versioned one on every pull
  request (`.github/workflows/manifests.yml`, no toolchain needed);
  `cargo publish --dry-run -p <crate>` reproduces the failure in a second;
  and `deny.toml` allows a path-only wildcard for exactly this shape. See
  `docs/decisions.md`, "CI and releases".

## Status

Phases 1 through 17 are merged, plus the pm2 cutover, the dogs subsystem,
`shep lookout`, `shep whistle`, config and packaging, the last three v1 verbs,
Windows, config overrides, boot ordering, and the lookout landing-pane redesign.

**[docs/history.md](docs/history.md) is the phase-by-phase account: what
shipped, when, and why each design call went the way it did.** Read it when you
need to know why something is the way it is. Several of its paragraphs exist
because this file once carried a claim that had quietly become false, so prefer
it over your own recollection.

What is not derivable from the code, and is worth knowing before you change
anything:

- **`PROTOCOL_VERSION`, `MIN_SUPPORTED` and `SCHEMA_VERSION` answer three
  different questions** and it is easy to move the wrong one. The protocol is
  what this build speaks; the floor is the oldest peer it still accepts; the
  envelope's schema governs JSON output and moves only on a rename, removal or
  retype. An additive `ProcessInfo` field moves none of them. Moving
  `PROTOCOL_VERSION` refuses nobody by itself: only `MIN_SUPPORTED` rising
  does, and it refuses every peer built below the new floor.
- **A Flockfile is a project template, never written by shep.** Operator tuning
  lives in `$SHEP_HOME/overrides.json`. There are three doors into that store
  and they mean different things: a Flockfile load spends an override, a lookout
  pane sets one.
- **A reload's overlap is conditional**, on `reuse_port` and on whether the app
  has a readiness probe. shep never binds an app's listening socket.
- **Windows is built and runs**, so `cfg(unix)` is a design decision rather than
  a shrug. The OS transport lives in one place, `shep_core::transport`.
- **A dog's config lives in `$SHEP_HOME/dogs.toml`**, migrated once at boot from
  the old `[dog.<name>]` sections in `shep.toml`.
- **Verb count is 41 generated and 42 listed**; the difference is `help`. Check
  which question is being asked before changing either.

What is built versus deferred: [docs/specs/deferred.md](docs/specs/deferred.md).
