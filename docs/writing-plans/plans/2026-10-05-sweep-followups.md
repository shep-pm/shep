# Plan: lamb sweep follow-ups (#690 items 2-4, #697)

Spec: `docs/brainstorming/specs/2026-10-05-sweep-followups-design.md`.
Branch: `fix/sweep-followups-690`, off `main` (#691 merged). Four tasks,
independent in code but run in order by one implementer, one commit each
(more if a task splits cleanly). Line references are pointers, not
promises; the compiler is the authority.

## Ground rules

- rust-house-style (IR-1..IR-48) and `docs/rust-house-style-addendum.md`
  before writing Rust. TDD: a failing test first.
- One cargo shape for the inner loop:
  `cargo test -p shep-daemon --lib --all-features -- --skip ::slow::`.
  Gate each commit with
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  and `cargo fmt --all --check`.
- Linux cross-check, own target dir:
  `CARGO_TARGET_DIR=target/linux-check cargo check -p shep-daemon --all-targets --all-features --target x86_64-unknown-linux-gnu`.
- Commits: `type(scope): summary`, types feat fix perf refactor docs test ci
  chore style, scope `shep-daemon` or `docs`. No `!`: nothing public changes.
- Never write a test or mutation that signals a pid the test did not spawn
  itself. Kill guards are tested as pure functions.

## Task 1: `ProcInstant` (spec section 1)

- `proc_table.rs`: `ProcInstant(u64)` with `now()`, and a start reading in
  the same unit per pid. Linux reads `/proc/<pid>/stat` field 22; a pure
  `parse_start_ticks(&str) -> Option<u64>` is tested everywhere
  (`cfg(any(test, target_os = "linux"))`), including a `comm` of `a) (b`.
- `sweep/mod.rs` `LambSnapshot` stores `ProcInstant`; `sweep/os.rs`
  survivors compare in that unit; earliest is zero on Linux, the boot check
  elsewhere.
- The three stamping sites in `limits/stats.rs` and `limits/mod.rs` use
  `ProcInstant::now()`. Tests that build snapshots from literals keep
  working through a constructor.
- Linux-only real-table test: a fresh child's start is within
  `[now - 1s worth of ticks, now]`.
- Docs: sweep module doc's "Pid reuse" section, `docs/decisions.md`
  "Sweeping a sheep's lambs" residual sentence, assumption 4 of
  `2026-10-05-sweep-lambs-design.md`, and any `web/` page stating the
  residual (grep `web/src/pages/docs` for "second").

## Task 2: leader check (spec section 2)

- `supervisor/sheep.rs` `sweep_after_exit` takes the root pid and checks it
  as specified. `LambSnapshot` gains whatever accessor the latest stamp
  needs.
- Engine test on `ScriptedSweep`: the root reported alive after the wait,
  no lamb signalled; and the ordinary case still sweeps.

## Task 3: boot tests (spec section 3)

- `boot/mod.rs`: `boot_with_sweep`, `boot` delegates.
- A `#[cfg(test)]` helper; every `boot(` call under `src/boot/` uses it.
  Confirm with `grep -rn "[^_]boot(" crates/shep-daemon/src/boot`.

## Task 4: #697 (spec section 4)

- `sweep/mod.rs` `sweep_lambs`: re-check the walk's roots; drop the look's
  born pids when any root fails.
- `ScriptedSweep` test: a root that stops surviving between `survivors`
  and the re-check, its born pid never signalled; the passing case still
  signals born pids.

## Finish

Full task gate from `CLAUDE.md`, plus the Linux and Windows cross-checks,
each with its own `CARGO_TARGET_DIR`. Unfiltered daemon tests once, since
this touches `limits/`.
