# Lamb sweep follow-ups (#690 items 2-4, #697)

Approved 2026-10-05 (delegate mode). Builds on
`2026-10-05-sweep-lambs-design.md`. Item 1 of #690, the stop deadline,
ships separately off `main`: it touches only `shep-client` and `shep-cli`.

## 1. Start times to the clock tick on Linux (#690 item 2)

A snapshot dates each pid by the second of the walk that saw it, and a
survivor must have started no later. sysinfo floors both `btime` and the
start ticks, so on Linux a pid reused up to about two seconds after the
walk still passes.

A pid is now dated on a **process clock** native to the platform, a
crate-private `ProcInstant` newtype in `proc_table.rs`:

- Linux: clock ticks since boot. `ProcInstant::now()` is
  `CLOCK_BOOTTIME` (`nix::time::clock_gettime`, safe) times
  `sysconf(CLK_TCK)`, floored. A pid's start is field 22 of
  `/proc/<pid>/stat`, ticks since boot on the same clock, parsed after the
  last `)` so a `comm` holding spaces or parens cannot shift fields.
  Residual: one tick (10 ms at the usual 100 Hz).
- Every other unix: epoch seconds through sysinfo, as today, with the
  before-boot refusal and its slack unchanged.

`LambSnapshot` stores a `ProcInstant` per pid instead of a bare second.
Every stamping site reads `ProcInstant::now()` before its walk:
`StatsState::lamb_snapshot_now`, `StatsState::lamb_walk_from`, and the poll
tick in `limits/mod.rs`. `PidReading.started_secs` stays, since handover's
uptime wants epoch seconds; the sweep reads a start in `ProcInstant` units.
On Linux the before-boot guard is moot (ticks since boot cannot precede
boot), so the earliest acceptable start there is zero.

If the kernel's start ticks and `CLOCK_BOOTTIME` ever disagreed, a reused
pid would be accepted, never a real lamb refused: `CLOCK_BOOTTIME` only runs
ahead of a clock that excludes suspend.

## 2. No sweep while the leader still lives (#690 item 3)

`run_sheep` sweeps once `proc.wait()` or `kill_process` returns. A wait that
returns while the leader still runs (an adopted reaper's closed `SIGCHLD`
stream, an unexpected errno, a ladder that gave up on a stuck leader) would
then sweep a live sheep's lambs, and `descendants` would follow what they
start. `ECHILD` is not the risk: there the leader really is gone.

`sweep_after_exit` checks the leader first, on both paths, with the sweep's
own predicate: `survivors` over a one-pid snapshot of the root, dated by the
latest stamp in the lamb snapshot. Alive, unreaped and no younger than that
stamp means the leader is still running: skip the sweep and `warn`. A
leader pid reused after the reap started later than the stamp, so it is not
mistaken for the leader. An empty lamb snapshot skips the check, since there
is nothing to sweep. `RunningProcess` and `ExitOutcome` do not change.

## 3. Boot tests never pair the real sweep with scripted pids (#690 item 4)

`boot()` builds `Extras::real`, and the boot tests drive it with
`ScriptedRunner`, whose pids start at 1000. Under a parallel `cargo test`,
pid 1000 can be another test's real `/bin/sh`, a descendant of the test
binary, so the ancestry guard passes it and a scripted sheep's exit sweeps
that child's descendants.

A crate-private `boot_with_sweep(runner, paths, options,
Option<Arc<dyn LambSweep>>)` replaces `extras.lamb_sweep` when given one;
`boot` delegates with `None`. A `#[cfg(test)]` helper passes
`idle_sweep()`, and every in-crate `boot(` call in `src/boot/` moves to it.
`tests/daemon_e2e` uses `TokioRunner`, so its real pairing stays.

## 4. A walk from a reused survivor (#697)

Each look in `sweep_lambs` reads survivors, then `descendants` walks from
them without the ancestry check, and what it finds gets `SIGTERM` that same
look. A survivor that exits between the two reads and has its pid reused
makes the walk start from a stranger.

After the walk, `sweep_lambs` re-checks the roots with `survivors`. A root
alive at both reads with the same start no later than its stamp was alive
throughout, and a live pid cannot be reused, so its children in the walk
are its own. If any root fails the re-check, that look's newly found pids
are dropped (not merged, not signalled) and the next look tries again.

## Assumptions (approved)

1. Linux `/proc/<pid>/stat` field 22 shares `CLOCK_BOOTTIME`'s base on any
   kernel from about 4.x. A Linux-only test checks a fresh child's start
   lands within a second before `ProcInstant::now()`.
2. macOS stays at about a second: microsecond starts need `proc_pidinfo`
   FFI, new `unsafe` outside the files allowed it.
3. The leader check runs on both exit paths, not only the adopted wait
   error.
4. Item 3 is fixed in test wiring, not with a method on the public
   `ProcessRunner` trait.
5. The #688 spec and the decision entry are amended in place where they
   state the residual.
