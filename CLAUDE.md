# shep

Rust process manager (daemon, CLI, client library) inspired by pm2. MIT OR Apache-2.0. Published at `github.com/shep-pm/shep`.

## Commands

MSRV 1.88, edition 2024, toolchain pinned in `rust-toolchain.toml`. Why each
command is shaped the way it is: [docs/testing.md](docs/testing.md). Read it
before changing one.

Inner loop:

```bash
cargo test -p shep-daemon --lib --all-features -- --skip ::slow::
```
```bash
cargo test -p shep --lib --bins --all-features -- --skip ::slow::
```

`::slow::` tests wait on real FSEvents or wall-clock time. Run unfiltered when
touching `watch/`, `extras/` or the sampler (`limits/`).

Task gate, once, when the task is otherwise done:

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

Per phase. The local gate never compiles Linux- or Windows-only code, so read
CI before calling a branch green:

```bash
cargo check -p shep-daemon --all-targets --all-features --target x86_64-unknown-linux-gnu
```
```bash
cargo check --workspace --all-targets --all-features --target x86_64-pc-windows-gnu
```

- On a dependency change: `cargo deny check`.
- At a merge: the task gate, plus
  `cargo test --workspace --all-features -- --test-threads=1` and
  `cargo bench --manifest-path benches/Cargo.toml -- --test` on stable and 1.88.

Gotchas:

- One cargo command at a time (shared target-dir lock). Capture `$?` directly, never through a pipe.
- `-p shep` is the CLI crate in `crates/shep-cli`. `-p shep-cli` selects the empty redirect placeholder, runs zero tests and exits 0.
- Bare `cargo test --workspace`, not `--lib --bins`: the cost here is the integration tier, which `--lib --bins` skips rather than speeds up.
- Cross-target checks use `cargo check`, not clippy (`cfg(unix)` code reads as dead). Windows needs `brew install mingw-w64`.
- To count broken doc links, drop `-D warnings`: deny stops at the first crate.
- Doc links inside `#[cfg(test)]` modules are checked by nothing. Read them by hand when a split moves a test file.

## Architecture

Workspace crates under `crates/`. Each Cargo.toml `description` states its role.

| Directory | Package | Role |
| --- | --- | --- |
| shep-core | shep-core | Types, Flockfile parsing, wire protocol, `$SHEP_HOME` layout (`paths.rs`), OS transport (`transport.rs`) |
| shep-daemon | shep-daemon | Supervision engine: spawning, restart policy, log plane, watch and cron, RPC server |
| shep-client | shep-client | Async client: typed requests, event subscriptions, starting a shepherd |
| shep-cli | **shep** | Library with thin bins: `shep`, plus container-entrypoint aliases `shep-runtime` and `shep-dev` |
| shep-macros | shep-macros | `#[dog_config]` attribute, re-exported as `shep_client::dogs::dog_config` |
| shep-channel | shep-channel | Client an app links to speak the shepherd channel |
| shep-cli-redirect | shep-cli | Empty placeholder holding the `shep-cli` name on crates.io |

Also at the top level:

- `web/`: the Astro docs site. Published, part of the public surface.
- `benches/`: its own workspace. `versus-pm2/` runs by hand at a release.
- `docs/`: `history.md` (what shipped and why), `decisions.md`, `specs/deferred.md` (built versus deferred), `terminology.md`, `testing.md`, `releasing.md`.

Facts the code does not make obvious:

- The CLI starts the daemon by re-executing itself with a hidden `daemon` subcommand (`crates/shep-cli/src/launch.rs`).
- Three version constants answer three questions. `PROTOCOL_VERSION` is what this build speaks and `MIN_SUPPORTED` is the oldest peer it accepts (both in `shep_core::protocol`). `SCHEMA_VERSION` governs the JSON output envelope (`crates/shep-cli/src/output/mod.rs`) and moves only on a rename, removal or retype. Only raising `MIN_SUPPORTED` refuses anyone.
- A Flockfile is a project template that shep never writes. Operator tuning lives in `$SHEP_HOME/overrides.json`: a Flockfile load spends an override, a lookout pane sets one.
- A dog's config lives in `$SHEP_HOME/dogs.toml`.
- A reload overlaps old and new only with `reuse_port` or a readiness probe. shep never binds an app's listening socket.
- Windows is built and shipped, so `cfg(unix)` is a design choice. The OS transport lives only in `shep_core::transport`.
- Tests in `crates/shep-cli/src/cli/help.rs` pin every visible verb to one help group and to the docs generator's `VERBS` list. A new verb fails one of them until it is wired into both.

## Docs site: hard trigger

A change to anything an operator types, sees or configures (verb, flag, alias,
`shep.toml` key, Flockfile field, exit code, JSON shape, default) is not done
until `web/` says so.

1. Regenerate the CLI reference from the real binary, then read `git diff`:
   ```bash
   cargo build --release
   ./web/scripts/generate-cli-reference.sh
   ```
2. Grep the hand-written `web/src/pages/docs/*.astro` pages for what changed.
3. Run the site gate from `web/`, in this order (CI's `docs site` job):
   ```bash
   cd web && npm ci
   ```
   ```bash
   cd web && npx astro check
   ```
   ```bash
   cd web && npm run build
   ```

- `npm ci` first. Without it `npx` prompts to fetch `@astrojs/check`, and a cancelled prompt exits 0.
- `npm run build`, never bare `astro build`. The script adds `verify-*` checks (prose word budgets, the pagefind index) that fail on pages that build clean.
- `astro check` is the only step that typechecks component props.

## Code style: hard trigger

Invoke the `shep-idiomatic-rust` skill before writing or reviewing any Rust
here. It fronts [docs/idiomatic-rust.md](docs/idiomatic-rust.md): rules
IR-1..IR-47, cited as `IR-<n>`, with evidence in
[docs/idiomatic-rust/lenses/](docs/idiomatic-rust/lenses/).

Most common drift: panicking constructors, `std::error::Error` instead of
`core::error::Error`, missing `# Errors` sections, `# Panics` without
`#[track_caller]`, input grammars wider than the spec.

- Every new public item needs docs and a deliberate `Debug` decision: redacted for anything carrying env or secrets, with an exact-string test (IR-41).
- `#![forbid(unsafe_code)]` is live in core, client, cli and macros. Unsafe lives only in shep-daemon's `sys.rs` and `sys_windows.rs` and shep-channel's `endpoint.rs`, each with its own `allow(unsafe_code)` and a `// SAFETY:` comment per block (IR-22/23).
- A dev-dependency on another workspace crate names a path only: no `workspace = true`, no version. A versioned one deadlocks publishing. `scripts/check-dev-deps.py` enforces it in CI.
- Open design decisions are listed at the bottom of map.md and in goals.md's open questions. Those are the maintainer's calls.

## Terminology

[docs/terminology.md](docs/terminology.md) is the lexicon.

- **sheep**: one managed process, singular only. The plural is **flock**, never "sheeps" or bare "sheep".
- **shepherd**: the daemon, and only the daemon.
- **dogs**: plugin processes the shepherd supervises (metrics, bark).
- **lambs**: a sheep's child processes.
- Also: fold, Flockfile, bleats, bark (webhooks), whistle (MCP), muster, lookout (TUI).
- Straight verbs (`start`, `stop`, `list`) stay first-class aliases. Destructive ops and error text stay plain.

## Commits and subagents

- Conventional commit subjects: `type(scope): summary`. Types are `feat` `fix` `perf` `refactor` `docs` `test` `ci` `chore` `style`. Put `!` on the commit that breaks something, in the crate it breaks. release-plz silently drops a subject it cannot parse. `.githooks/commit-msg` and `.github/workflows/commits.yml` enforce it; [CONTRIBUTING.md](CONTRIBUTING.md) has the detail.
- Every subagent brief states the commit rule in its own text.
