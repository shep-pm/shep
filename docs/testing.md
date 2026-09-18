# shep — how the test suites behave

Moved out of `CLAUDE.md` on 2026-09-07, for the reason `docs/history.md`
records: that file is injected into every subagent, and these measurements
are worth keeping without being worth re-sending on every dispatch.

`CLAUDE.md` carries the commands themselves. This carries the numbers behind
them, and the reasoning that stops each one being "simplified" back into a
slower or less honest form.

## The measurements behind the commands

MSRV 1.88, edition 2024. The build cache works — a no-op rebuild is **0.35s**.
Slow runs are never compilation; they are test execution, and almost all of it
is one class of test.

### The inner loop — use this while iterating, including for every mutation

```bash
cargo test -p shep-daemon --lib --all-features -- --skip ::slow::
```

**~3.2s, 766 of 785 lib tests as of 2026-09-03**. The exact counts drift
every time a task adds one, so treat them as a shape, not a checksum. Three
briefs have now shipped a stale figure, this file carried "437 of 454"
for long enough to be wrong by fifty, then "619 of 638" while the real number
climbed by ninety-two, then "711 of 730" while the config-overrides branch
added fifty-five. The 19 tests this skips live in a nested `mod slow`
inside each file's `mod tests` — `extras.rs` has 9, `watch/source.rs` 7, and
`watch/mod.rs`, `limits/sample.rs` and `handover/mod.rs` one each — and wait on real macOS
FSEvents or real elapsed time; they are the reason the unfiltered lib run
costs ~25s instead. A mutation in `supervisor.rs` does not need them — but a
change to `watch/source.rs`'s watcher plumbing, or to timing-sensitive
behavior in `extras.rs` or the sampler, does, so run the unfiltered lib suite
when touching either.

CI runs that tier as its own serial `slow` job and skips it everywhere else,
because a contended runner cannot hold a wall clock still: the debouncer
tests were the whole of CI's red for four runs. `boot.rs`'s
`two_concurrent_boots_on_a_stale_socket_exactly_one_wins` rides along in that
job for the same reason without being in a `mod slow` — it is fast, but it
races two threads and needs the machine quiet. Add a timing- or
contention-sensitive test and it needs the same treatment; the workflow's
skip list names both groups explicitly.

**The skip list is not the first answer to a CI-only failure, and twice on
2026-09-04 it was the wrong one.**
`a_reopen_that_cannot_open_a_path_again_exits_internal` renamed a log file
after `poll_flock` said `online`, which means the daemon spawned the child
and NOT that the pump has opened the file, so the rename failed `ENOENT`;
the sibling ninety lines above it
already waits for the first line through `bleats` and calls that wait a
precondition. `a_flock_of_every_carried_kind_survives_a_daemon_reload` was
not a test problem at all: the log pump dropped a line the child wrote just
before its sheep task let go, 39 times in 64 at the seam, and quarantining
it would have hidden that. Both stay in the ordinary tier. Before reaching
for the skip list, check the failure against the `slow` tier's own
criterion, which the workflow states: a test belongs there when it asserts a
duration, a batch or a count that a contended runner cannot hold still.
"Waits twenty seconds for something that should take milliseconds" is not
that, and an EMPTY artifact where a partial one was expected is a defect
rather than a slow machine. See `docs/decisions.md`, "CI flakes, and the log
line a stop could lose".

From Phase 15 on, `shep` is a library with three thin `[[bin]]` targets
over it (`shep`, `shep-runtime`, `shep-dev`) rather than one bare binary — the
two container-entrypoint aliases spec §3 asks for cannot share a module tree
without a library underneath them. A **shep-scoped** run therefore needs
both halves: `cargo test -p shep --lib --bins --all-features`. `--bins`
alone now runs almost nothing, since every unit test in the crate lives in the
library.

`shep` has a `mod slow` of its own as of 2026-08-28, seven tests as of
2026-08-31, in `commands/lifecycle.rs`. It needs a real node to start and exit inside a
budget, which is a claim about the machine's speed rather than about shep: at
200ms it failed on four CI runners at once while passing every local run. Add
`-- --skip ::slow::` to a shep-scoped run for the same reason the daemon one
carries it. CI already covers it: the `slow` job runs `--workspace`, chosen so
a `mod slow` outside shep-daemon could not end up skipped everywhere and run
nowhere.

### The task gate — run once, when the task is otherwise done

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
```

**If the task changed anything an operator types or sees**, the gate has a
fifth step in `web/` -- see the docs trigger below for what counts and why:

```bash
cargo build --release
```
```bash
./web/scripts/generate-cli-reference.sh
```
```bash
cd web && npx astro build
```

Each from its own command with `$?` captured directly, never through a pipe —
in zsh a pipeline's `$?` is the last command's and `${PIPESTATUS[0]}` is empty.
**One cargo command at a time**: the workspace shares one target-dir build
lock, so concurrent runs block rather than parallelise. (A separate worktree,
or `benches/`, has its own lock and may run alongside.)

### The two cross-checks — run once per phase, not per task

```bash
cargo check -p shep-daemon --all-targets --all-features --target x86_64-unknown-linux-gnu
cargo check --workspace --all-targets --all-features --target x86_64-pc-windows-gnu
```

One cargo command at a time, as everywhere else, and give them their own
`CARGO_TARGET_DIR` if you want the host cache left alone.

**Linux.** `notify.rs`'s abstract-namespace branch and its test are both
`#[cfg(target_os = "linux")]`, so a macOS `cargo test` compiles neither. That
branch is what a systemd `Type=notify` unit — the unit `shep startup` installs
— depends on for readiness reporting, and it went five phases without a
compiler ever reading it (platform audit #3). `--all-targets` is what reaches
the test. shep-daemon has no `ring` in its tree, so this needs no cross C
toolchain; `-p shep` would, and is not in this gate — a macOS host has no
`x86_64-linux-gnu-gcc` for `ring`'s build script to call, so `cargo check -p
shep --target x86_64-unknown-linux-gnu` fails outright here, gcc or no.

**shep carries its own `#[cfg(target_os = "linux")]` code now, and this
gate does not reach it.** Phase 15 added
`crates/shep-cli/tests/init.rs::a_reparented_orphan_is_reaped` and
`reap.rs::drain_reaps_a_real_reparented_orphan`, both Linux-only, in the one
crate this gate deliberately excludes. Local checks give no signal on either
— not this gate (excludes `-p shep` for the reason above), not a bare
macOS `cargo test` (never compiles a `target_os = "linux"` item at all). What
DOES cover them: `.github/workflows/test.yml`'s `test` job, whose
`ubuntu-latest`/`ubuntu-24.04-arm` legs run `cargo test --workspace --locked
--all-features` on real Linux. That workflow runs on every
push and pull request, and has since 2026-08-16, so those two tests DO get
executed on real Linux now. This paragraph previously said the workflow was
`workflow_dispatch`-only "while the repository is private"; both halves were
stale, and the staleness cost real time on 2026-08-19 when a Phase 17 task
was written to "turn CI on" that was already on. The repository is public and
standard runners are free.

Still don't assume the local gate covers Linux: it does not, and three
separate breakages on 2026-08-19 were visible only to CI. `--all-features`
hides a feature-matrix break, a macOS `cargo test` never compiles a
`target_os = "windows"` arm, and the windows-gnu cross-check is `cargo check`,
which does not run anything. Those are not gaps in the gate; they are what
the gate is. **Read the CI result before claiming a branch is green.**

**Windows.** Every plan through Phase 6 carried this one; Phases 7-9 dropped
it without saying so, and it never reached this file, which is why nothing
noticed for three phases. Restored in Phase 10 after being measured green
(`EXIT=0`, 8.42s, 2026-08-13). It needs a C toolchain for the target —
`brew install mingw-w64` — because `ring`'s build script runs `cc`; a host
without `x86_64-w64-mingw32-gcc` cannot run it, and that is presumably how it
came to be dropped.

`cargo check`, deliberately, not `clippy -- -D warnings`: shep-daemon's
`boot`/`sys`/`server`/`tokio_runner` are `cfg(unix)`-gated, so on Windows 51
dead-code warnings fall out of code that is not dead anywhere we ship. The
question this gate asks is whether the tree still compiles for a target nobody
has implemented yet. Silencing those warnings would mean `#[allow(dead_code)]`
on live code.

### Doctests are not the cost here — do not split them out

Measured 2026-08-12 on this machine: bare `cargo test --workspace
--all-features` **89.3s**; `--all-targets` (same minus doctests) **82.7s**;
the three crates' doctests run alone **30.9s**. They overlap rather than add,
so splitting them out of the task gate buys ~6.5s and costs a second command.

The global rule to prefer `--lib --bins` over bare `--workspace` was measured
on a project where doctests dominated. It does not transfer: this workspace's
cost is the integration tier (`cli_e2e` ~47s, `daemon_e2e` ~22s), which
`--lib --bins` would skip entirely rather than speed up. Keep the bare form.

**This holds for the LOCAL gate. CI splits them, because nextest cannot run
them at all.** As of 2026-09-04 every CI leg that used to run `cargo test`
runs `cargo nextest run` plus a separate `cargo test --doc`, so a flaky
integration test can be retried without retrying anything else. That split is
forced by the tool rather than chosen, and it is cheap: measured the same day
on this machine, warm, doctests alone are **6.8s**, not the 30.9s above, which
was a colder tree. `cargo nextest run` over the same skip set is **26.1s**
against `cargo test`'s **45.2s**, so a leg is faster even paying for the
second command.

Two numbers to keep straight when a count looks wrong. `cargo test --workspace
--all-features` over the skip set reports **2525**; nextest reports **2511**,
and the missing **14** are exactly the doctests it does not run. The two
filtersets in the workflow partition the suite with nothing dropped: 2511 in
the ordinary legs plus 30 in `slow` is 2541, which is every non-doctest test.
Nothing here changes the local gate: keep running bare `cargo test`.

**A test that passed only on retry is a warning on the run, not a green
leg.** Every CI profile in `.config/nextest.toml` writes junit, and
`.github/actions/nextest-report` runs after every nextest leg: each test
carrying a `flakyFailure` becomes a `::warning` annotation and a row in the
job summary, and the file is kept as an artifact for 14 days. Read the
annotation before merging. The retry exists so a contended runner does not
block a merge, not so a defect can hide behind one; `nextest.toml`'s own
comment records two 2026-09-04 failures a retry would have hidden.

### cargo-deny runs in CI; run it when a dependency changes

```bash
cargo deny check
```

The `deny` job runs it against `deny.toml` on every pull request that
touches Rust, either lockfile or the file itself: RustSec advisories, the
licence allowlist, duplicate versions (warned, not refused) and sources
(crates.io only). A licence not on the list fails the job until somebody
reads it and adds it, which is the point. `brew install cargo-deny` on the
host; it is not in the task gate because the advisory half needs the
network, and the gate is meant to run offline.

### The phase gate — run at a merge, not per task

The four above, plus `cargo test --workspace --all-features -- --test-threads=1`
and both `benches/` gates. The serial run is not ceremony: it was red on `main`
before Phase 5 and it caught a real regression in Phase 6.

### Measuring a mutation's blast radius

Use the inner loop. Escalate to `cargo test --workspace --all-features
--no-fail-fast` **only if the targeted run shows a radius above 1**, or if the
change crosses a crate boundary. Without `--no-fail-fast` cargo stops at the
first failing binary and a radius of 3 reads as 1.

Bounded waits on real children produce **false radii under load** — an earlier
task saw 9 failures that were all load artefacts. Confirm any radius above 1 by
re-running that suite in isolation with the mutation still applied.
