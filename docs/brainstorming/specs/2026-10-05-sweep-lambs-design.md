# Sweeping a sheep's lambs on every exit (#688)

Approved 2026-10-05 (delegate mode).

## Problem

A stop reaches the sheep's own process group and nothing else
(`crates/shep-daemon/src/kill.rs`). Three holes:

1. A lamb in a group or session of its own (`setsid`, `setpgid`, a sandbox
   runtime) never gets the stop signal or `kill_tree`.
2. With `shutdown_with_message` the first rung sends no signal at all.
3. Once the leader exits, `proc.wait()` resolves and `kill_tree` is skipped,
   so any lamb that outlives the sheep, in-group or not, keeps running,
   reparented to init.

A crash has the same shape as 3: nothing touches the lambs at all.

## Design

### One choke point

Every way a sheep ends passes through `run_sheep`
(`crates/shep-daemon/src/supervisor/sheep.rs`): stop, restart, reload drain,
scale-down and shepherd shutdown arrive as `SheepCtl::Kill` and run
`kill_process`; a crash or a clean exit arrives on the `proc.wait()` arm. The
sweep runs in that task, after the leader is reaped and **before**
`Msg::Exited` is sent, so a stop reply, an autorestart or a respawn only
happens once the lambs are gone.

### What is swept: a snapshot

A snapshot is a set of pids plus the wall-clock second it was taken. No start
times are stored.

- **A ladder** (`SheepCtl::Kill`): before the first rung, a fresh walk of the
  process table (`TreeIndex::descendants_of(root)`), merged with the last
  periodic snapshot. The periodic half catches a lamb orphaned off the ppid
  tree since the last tick.
- **A natural exit**: the leader is gone and the ppid tree with it, so the
  sweep uses the snapshot the `MEMORY_POLL_INTERVAL` tick recorded. That tick
  already walks the table for every watched sheep; recording costs a map
  write per sheep and no extra walk.

### The sweep (unix)

1. Survivors: each snapshot pid that is alive, not a zombie, and whose OS
   start time is at or before the snapshot's second. A pid cannot be recycled
   while its process lives, so a process alive at both readings that started
   no later than the snapshot is the one the snapshot saw. Pids `<= 1` and the
   shepherd's own pid are never survivors.
2. `SIGTERM` every survivor, then poll every 100 ms for up to the app's
   `kill_timeout`, returning early once none survive.
3. Re-check survivors the same way and `SIGKILL` what is left.

The sheep shuts its own lambs down first: an out-of-group lamb gets nothing
until the leader has exited.

### Seam

A crate-private `LambSweep` trait: take a fresh snapshot, read the last
periodic one, list survivors, signal one pid. The real implementation sits on
`StatsState` (its sampler for the walk, its tick for the periodic snapshot)
and a targeted sysinfo refresh for liveness, sharing `start_epoch_secs` with
`handover/uptime.rs`. A fake in `fake/` drives the engine tests on a paused
clock. `kill.rs` stays portable. `RunningProcess`, `MemorySampler` and
`ProcessRss` are unchanged, so nothing public breaks.

### Windows

No-op. The job object's `kill_tree` already reaches the whole tree on a stop.
The natural-exit hole stays open there and is documented with the other
Windows gaps.

## Assumptions (approved)

1. A crash is swept too, not only a stop. A one-shot sheep that backgrounds a
   daemon and exits 0 now has that child killed, as under systemd's default.
   No opt-out key.
2. Out-of-group lambs are left to the sheep until the leader exits.
3. The sweep's grace is `kill_timeout` on every path, including a reload
   drain. Worst case a stop takes 2 × `kill_timeout`, only when lambs outlive
   the sheep and ignore `SIGTERM`.
4. One-second residual on pid reuse: a pid recycled within the snapshot's own
   second passes the check. Documented, no pidfds.
5. No new bus event: a `tracing` info line per sweep that signalled anything,
   a warn when one needed `SIGKILL`.
6. A nested shepherd started from inside a sheep is a lamb and is swept if it
   is still in the ppid tree.
7. Not covered: a lamb that double-forks away before a snapshot, and one born
   and orphaned between two ticks before a crash.
