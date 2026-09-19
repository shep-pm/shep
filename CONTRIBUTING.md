# Contributing to shep

Bug reports, fixes and features are all welcome. Issues labelled
[good first issue](https://github.com/shep-pm/shep/labels/good%20first%20issue)
are the ones with the smallest amount of context to load first.

This file is mostly about things you cannot guess from reading the code. The
gate is four commands, two of the test invocations have a trap in them, and a
commit subject that does not parse is dropped from the changelog without an
error anywhere.

## One rule before anything else

**Do not port source from pm2, or from any other process manager, including by
eye.** shep was written from behavior specs rather than from anyone's code, and
that is worth keeping true. Describing what pm2 does is fine, and the docs
compare the two in detail. Copying how it does it is not.

## Setting up

Rust 1.88 or newer, edition 2024. `rust-toolchain.toml` pins the channel, so
`rustup` fetches the right one on the first build.

Turn the repository's git hooks on. They catch a bad commit subject before CI
does, and run [typos](https://github.com/crate-ci/typos) over staged files if
you have it installed:

```bash
git config core.hooksPath .githooks
```

## While you work

Run the crate you are changing rather than the workspace. The skipped tests
wait on real elapsed time or real filesystem events, so they are worth an hour
of your afternoon and nothing else:

```bash
cargo test -p shep-daemon --lib --all-features -- --skip ::slow::
```

Run the unfiltered suite when you touch `watch/`, `extras.rs` or the sampler,
since those are the tests being skipped.

**The CLI's package name is `shep`, not `shep-cli`.** The directory is
`crates/shep-cli` and the package inside it is `shep`, so `cargo test -p
shep-cli` runs zero tests and exits 0, which reads exactly like a pass. The CLI
is a library with thin binaries over it, so it needs both halves:

```bash
cargo test -p shep --lib --bins --all-features -- --skip ::slow::
```

## The gate

Run these before you open a pull request. One at a time: the workspace shares
one build lock, so two at once is slower than either alone.

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
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --document-private-items
```

`--document-private-items` is most of that last command's value. Most of the
daemon and the CLI is `pub(crate)`, so without it the check reads a small
fraction of the doc comments this repository writes, and an intra-doc link in a
private module can point at nothing forever. It costs about half a second.

Bare `cargo test --workspace` is deliberate here, rather than `--lib --bins`.
This workspace's cost is its integration tests, which `--lib --bins` skips
rather than speeds up.

CI also builds for Linux and Windows, and neither is reachable from a macOS
gate. Read the CI result before calling a branch green.

## If you changed anything an operator types or sees

A new or removed verb, flag, alias, `shep.toml` key, Flockfile field, exit
code, JSON shape or default value is not finished until the docs site says so.
Nothing in the Rust gate can catch this: `cargo test` never reads `web/`.

The CLI reference pages are generated from the binary's own `--help`:

```bash
cargo build --release && ./web/scripts/generate-cli-reference.sh
```

`git diff` afterwards is the check. Then grep the hand-written pages under
`web/src/pages/docs/` for whatever you changed, because no generator touches
those.

Then run the site's own gate, from `web/`, in this order:

```bash
cd web && npm ci && npx astro check && npm run build
```

`npm ci` goes first or the command after it passes without checking anything: a
fresh checkout has no `@astrojs/check`, so `npx` offers to fetch one and a
declined prompt exits 0. `astro check` is the separate half that catches a
component handed a prop it does not have, since Astro does not typecheck during
a build. And `npm run build` rather than `astro build` alone, because the
`build` script wraps the Astro build in a set of verifiers that each fail on
content a clean build does not notice.

## Commits

Every commit subject is `type(scope): summary`, and this one matters more than
it looks. Releases are cut by release-plz, which walks individual commits and
silently drops any subject it cannot parse. An unreadable subject therefore
contributes nothing to its crate's changelog and nothing to its version bump,
with no error anywhere. A source break once shipped to crates.io as a patch
release with an empty changelog section exactly this way.

Nine types are accepted, and a hook and a CI job both check:

`feat` `fix` `perf` `refactor` `docs` `test` `ci` `chore` `style`

Add `!` after the type or scope for anything that breaks a caller, on the
commit that breaks it, in the crate that breaks. A `!` on a pull request title
is read by nobody, because release-plz ignores merge commits.

Bodies are welcome and can be long. Say why, not what.

One commit per thing. A fix that arrives alongside an unrelated cleanup is two
commits, so either can be reverted without the other.

## Opening a pull request

Fill in the template. The short summary at the top is for whoever reviews it,
and the `Precise details` block below is for the coding agents and scripts that
also read pull requests.

Say what you did not test. An untested platform named in the body costs nothing
and saves a reviewer guessing.

CI runs the same gate as above plus Linux and Windows. A red check is not a
reason to apologise; push a fix and carry on.

## Filing an issue instead

Use the forms. The one thing worth care in a code-quality issue is the site
list: name every location, not the first few you found. A list that stops early
means whoever picks it up fixes four of eleven and the rest survive. Grep for
the shape rather than the identifier, since a sibling written by the same hand
usually carries a different name.

## Terminology

The sheepdog theme is load-bearing in the code and the docs. A **flock** is
every managed process, a **sheep** is one of them, **bleats** are its logs, a
**bark** is a webhook alert, a **dog** is a plugin process the shepherd
supervises, and the daemon itself is only ever the **shepherd**. Plain verbs
like `start`, `stop` and `list` stay first-class, and error text stays plain.
The theme never costs clarity.

[docs/terminology.md](docs/terminology.md) has the rest.
