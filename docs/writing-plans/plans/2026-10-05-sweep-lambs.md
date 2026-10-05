# Plan: sweep a sheep's lambs on every exit (#688)

Spec: `docs/brainstorming/specs/2026-10-05-sweep-lambs-design.md`.
Branch: `feat/sweep-lambs-688`. Four tasks, strictly sequential: each builds
on the seam the previous one lands. Line references below were read on
2026-10-05 and are pointers, not promises; the compiler is the authority.

## Ground rules for every task

- Read the rust-house-style rules (IR-1..IR-48) and
  `docs/rust-house-style-addendum.md` before writing Rust. TDD: a failing test
  first, then the code.
- One cargo shape for the inner loop:
  `cargo test -p shep-daemon --lib --all-features -- --skip ::slow::`.
  Gate each commit with
  `cargo clippy -p shep-daemon --all-targets --all-features -- -D warnings`
  and `cargo fmt --all --check`. Nothing else, and never `-p shep-cli`.
- Conventional commits, `type(scope): summary`, scope `shep-daemon` (or
  `docs`). No `!`: nothing public changes.
- Crate-private everything new. `RunningProcess`, `MemorySampler`,
  `ProcessRss` and `ProcessIdentity` are public API and must not change.

## Task 1: the `LambSweep` seam, the sweep driver and the fake

New crate-private module `crates/shep-daemon/src/sweep.rs` (or `sweep/` if it
grows past ~500 lines with tests, IR-48).

- `LambSnapshot`: pids (deduplicated) and `taken_at_secs` (wall-clock seconds
  since the epoch). A `merge` that unions pids and keeps the **later** (corrected in Task 2: per pid, see the spec)
  second, since a survivor must have started no later than the snapshot that
  saw it.
- Trait `LambSweep: Send + Sync`, dyn-compatible, synchronous:
  - `snapshot(root_pid) -> LambSnapshot`: a fresh table walk, ppid
    descendants of `root_pid`, excluding it.
  - `last_snapshot(root_pid) -> Option<LambSnapshot>`: what the last poll
    tick recorded.
  - `survivors(&LambSnapshot) -> Vec<u32>`: alive, not a zombie, OS start
    time `<= taken_at_secs`, never pid `<= 1`, never `std::process::id()`. An
    unknown start time is not a survivor.
  - `signal(pid, LambSignal)` where `LambSignal` is `Term` or `Kill`; a
    failure is returned, never panicked.
- `async fn sweep_lambs(sweep: &dyn LambSweep, snapshot: &LambSnapshot, grace: Duration)`:
  survivors → `Term` each → poll every 100 ms (`tokio::time`, so tests run on
  a paused clock) until none survive or `grace` elapses → survivors again →
  `Kill` each. An empty snapshot returns without reading anything. One
  `tracing::info!` when it signalled anything, a `warn!` naming the pids that
  needed `Kill`, and a `warn!` per failed delivery. Returns a small report
  (termed, killed) for tests.
- Periodic recording: `StatsState` gains a per-root map of `LambSnapshot`,
  replaced wholesale each tick from the tick's `TreeIndex` (use
  `TreeIndex::descendants_of`) for every watched root, the same way
  `record_baseline` replaces baselines. Call it from the poll loop in
  `limits/mod.rs` next to `record_baseline`, with the wall-clock second read
  **before** the table was sampled.
- Real implementation: a struct over `Arc<StatsState>`. `snapshot` walks via
  the stats sampler's `sample()` and `TreeIndex`; `last_snapshot` reads the
  map; `survivors` does one targeted sysinfo refresh of the snapshot's pids
  (status and start time; move `start_epoch_secs`'s refresh shape out of
  `handover/uptime.rs` into a shared helper rather than duplicating it);
  `signal` is `nix::sys::signal::kill` with a positive pid on unix. On
  Windows `survivors` returns nothing and `signal` is unreachable in practice;
  keep it compiling (`cfg`), document why the job object makes it moot.
- Fake in `crates/shep-daemon/src/fake/`: scripted snapshots and a scripted
  survivor list per call (so a test can say "two survive the first look, one
  the second"), recording every `signal` call in order.
- Tests (paused clock): empty snapshot signals nothing; all exit after `Term`
  → no `Kill`, returns before `grace`; one ignores `Term` → `Kill` after
  `grace`; a failed delivery is logged and the sweep still finishes; `merge`
  keeps the later second per pid; the poll tick records a snapshot for a watched
  root and drops one for an unwatched root. A real-table unix test that
  `survivors` refuses pid 1, our own pid, and a pid whose start time is after
  `taken_at_secs` (spawn a child, snapshot with `taken_at_secs` one hour in
  the past). That last one may live in a `mod slow` if it spawns.

Commit: `feat(shep-daemon): add a lamb sweep that outlives the process group`.

## Task 2: wire the sweep into every exit

`crates/shep-daemon/src/supervisor/sheep.rs`, `run_sheep`:

- `spawn_sheep_task` and `run_sheep` take an `Option<Arc<dyn LambSweep>>`.
  `None` means no sweep (an actor built without extras). Every call site
  passes the actor's: `actor_spawn.rs` (two), `actor_lifecycle.rs`,
  `actor_reload.rs`, and the direct `run_sheep` in
  `supervisor/tests/reload_bus.rs`. Build it once where `Extras` builds
  `StatsState` (`extras/mod.rs`) and hold it on `Extras`.
- `SheepCtl::Kill` arm: before `kill_process`, snapshot = fresh `snapshot(pid)`
  merged with `last_snapshot(pid)`. After `kill_process` returns,
  `sweep_lambs(.., app's kill_timeout)`, then send `Msg::Exited`.
- `proc.wait()` arm: `last_snapshot(pid)`, sweep with `kill_timeout`, then
  send `Msg::Exited`.
- The snapshot walk is blocking (~6 ms). Match how the codebase already calls
  `sample_now` from async code; if that is `spawn_blocking`, do the same.
- Update the comments this makes false: `kill.rs`'s module and
  `kill_process` docs ("A lamb outliving the sheep also skips this rung"),
  `tokio_runner/runner.rs`'s `signal_group` and `kill_tree` docs, the
  `limits/mod.rs` module doc and `StatsState::lambs_of`'s "Not the set of
  processes a stop kills" (still not exactly the set, say how it differs now).
- Engine tests with the fake runner and the fake sweep, paused clock:
  - a stop sweeps a snapshot that merges the fresh walk and the last tick;
  - `shutdown_with_message`: the sweep still sends `Term` to lambs;
  - a natural exit sweeps the last tick's snapshot;
  - `Msg::Exited` (or the stop reply) does not arrive before the sweep's
    `Kill` when a lamb ignores `Term` (advance the clock and assert ordering);
  - no sweep handle → behaviour exactly as before.

Commit: `feat(shep-daemon): sweep a sheep's lambs before reporting its exit`.

## Task 3: prove it on real processes

A unix `::slow::` test through the real `TokioRunner` and the real sweep (find
the nearest existing real-process supervisor or runner test and follow it):

- A sheep (`/bin/sh -c`) starts a lamb that calls `setsid` (perl or python
  `os.setsid()`, or `setsid` the binary where present; skip with a message
  when none exists) and traps and ignores `TERM`, writes its pid to a file,
  then sleeps.
- Stop the sheep with a short `kill_timeout`. Assert the lamb pid is gone
  (`kill(pid, 0)` is `ESRCH`, or a zombie reaped by init) within a bound.
- Second case: the sheep exits on its own with a `setsid` lamb still running
  and a sweep snapshot recorded; assert the lamb is gone after the exit is
  reported. If the poll interval makes this impractical, drive the tick
  function directly rather than waiting 15 s.
- Use a unique `$SHEP_HOME`/temp dir per test (IR-34) and kill any survivor in
  a drop guard so a red run leaves nothing behind.

Commit: `test(shep-daemon): a setsid lamb is gone after a stop and after a crash`.

## Task 4: docs

- `web/src/pages/docs/lifecycle.astro` (~line 205): what a stop reaches now,
  the sweep after the leader exits, that a crash sweeps too, the grace being
  `kill_timeout`, and the double-fork residual. Plus the Windows line.
- `docs/history.md` (~line 143 and ~349): the `setsid` hole is closed for a
  sheep's lambs on unix; probes keep it.
- `docs/decisions.md`: one entry with the assumptions from the spec.
- `docs/specs/deferred.md` / `web/src/pages/docs/not-built.astro`: remove or
  amend anything that lists this hole as unbuilt.
- Site gate from `web/`: `npm ci`, `npx astro check`, `npm run build`.

Commit: `docs: a stop and a crash now sweep a sheep's lambs`.

## Task gate (main thread, once)

The four commands in CLAUDE.md's task gate, the Linux and Windows
`cargo check` cross-targets, then a PR that closes #688.
