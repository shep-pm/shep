# shep versus pm2

A head-to-head measurement of the two process managers on one machine, run by
hand at a release and never in CI: it installs a third-party npm package and
times a wall clock, and a shared runner can hold neither still.

```sh
./benches/versus-pm2/versus-pm2.sh --check    # judge a run against baseline.json
./benches/versus-pm2/versus-pm2.sh --record   # replace baseline.json with a run
./benches/versus-pm2/versus-pm2.sh            # print a run's figures, judge nothing
```

`VERSUS_SCRATCH` picks where builds, the pm2 install and the raw samples go
(default `/tmp/shep-versus-pm2`); `SHEP_BIN` points at an already-built
release binary if you have one. `docs/testing.md` says when to run it.

## Before the first run

The harness builds nothing and installs nothing. It checks for all of this
before it touches a daemon, and names whatever is missing:

- macOS. It reads BSD `stat` and `sysctl`, and its socket root is under
  `/private/tmp`.
- A release build of shep at `$VERSUS_SCRATCH/wt-bench/target/release/shep`,
  or wherever `SHEP_BIN` says. A worktree keeps the build off your checkout,
  and with no `CARGO_TARGET_DIR` set the binary lands inside it, which is how
  the run finds the commit it records:

  ```sh
  git worktree add /tmp/shep-versus-pm2/wt-bench <rev>
  cargo build --release --manifest-path /tmp/shep-versus-pm2/wt-bench/Cargo.toml
  ```

- pm2 in the scratch directory. For `--check`, install the version
  `baseline.json` names: pm2 is the control, and a different pm2 is a
  different control.

  ```sh
  npm install --prefix /tmp/shep-versus-pm2/pm2-install pm2@7.0.4
  ```

- `python3`, `perl` and `node` on `PATH`.

## The baseline

`baseline.json` is the last run somebody decided to keep, reduced by
`compare.py` to one figure per metric per tool. `--check` runs the harness
and compares shep's figures with it; `--record` runs the harness and
replaces it, and the `git diff` of that file is the before and after. Every
run keeps its `metrics.jsonl`, so judging a finished run again costs a
second rather than another run:

```sh
python3 benches/versus-pm2/compare.py check /tmp/shep-versus-pm2/versus-pm2-raw/metrics.jsonl
```

What the three verdicts mean, and why pm2 is the control rather than the
denominator, is at the top of `compare.py`. Each metric's threshold and the
reason for it is in its `METRICS` table. The committed file was transcribed
from the docs page's 2026-09-14 table rather than recorded, since that run's
samples were never kept. Its `notes` say what that costs, and the first
`--record` replaces it.

## What it measures, and why each is fair

Both tools run the *same two shell scripts* the harness writes, under the same
`/bin/sh`, with logs going to each tool's own default file capture.

| Metric | Workload |
| --- | --- |
| Idle daemon CPU and RSS | ten `while true; do sleep 5; done` apps |
| Log-plane CPU per line | one unthrottled `echo` loop, 62-byte lines |
| Start latency | ten apps, cold daemon and warm |
| Footprint | shep's binary against the pm2 install tree |

The shepherd is what is sampled, never the children. On the pm2 side that
means its God Daemon.

## The order is A/B/A on purpose

shep, then pm2, then shep again. A laptop that changes power state mid-run
shows up as disagreement between the two shep rounds instead of hiding inside
a ratio. The first run caught exactly that: the battery flipped from
discharging to charging, and the two shep rounds still agreed within 2.6%, so
the numbers stood.

Read the ratios, not the absolute figures. The box is not required to be idle,
and the run that produced the committed results had a load average near five
from unrelated work.

## Clean room

This benchmark drives the **published npm package** as a black box:
`npm install pm2`, then its own binary and its own `pm2 ecosystem simple`
generator for the config format. Observing what a shipped program does is not
porting it. Nothing here reads pm2's source, and neither should you.

## Results, 2026-08-29

shep 0.1.12 (`d113586`) against pm2 7.0.4 on node v26.5.0, macOS.

The first run, kept as it was written. The current figures are
`baseline.json` and the docs page, and neither reads this section.

| Metric | shep | pm2 | Ratio |
| --- | --- | --- | --- |
| Idle daemon RSS, ten apps | 13.90 MiB | 71.16 MiB | 5.1x |
| Log-plane CPU per line | 2.10 us | 4.03 us | 1.9x |
| Start ten apps, cold | 0.158 s | 0.374 s | 2.4x |
| Start ten apps, warm | 0.056 s | 0.197 s | 3.5x |
| `shep` binary vs pm2 install | 14.23 MiB | 23.10 MiB | 1.6x |
| Idle daemon CPU | 0.045% | 0.020% | a tie at the noise floor |

Idle CPU is reported as the instrument read it. Both figures are hundredths
of one percent of one core; the difference is not a result.

That footprint row compares one binary against a whole install tree, which is
not a like-for-like. `m_footprint` reads `stat` on `$SHEP_BIN` alone, and the
three-binary split landed on 2026-08-15, before this run: an install put
`shep`, `shep-runtime` and `shep-dev` on disk, so the honest shep side of that
row is three times 14.23 MiB. The row said "Install footprint" until
2026-09-14. The harness's own printed label said "shep binary" the whole
time.

Neither side counts a runtime. shep's binaries are static and pm2's tree is
not: it needs Node, another 72.73 MiB installed on the machine these were
measured on. `m_footprint` does not measure that, deliberately, since most
people running pm2 have Node already and its size varies by platform and
packager. Read the row as "what the tool itself weighs", not "what it costs
to run".

The log-plane figure is worth its own sentence. shep cost 32.8 us per line
before the 2026-08-28 audit, so pm2 was ahead by 8x on this measure until the
day before these numbers were taken.

Verified while measuring, rather than assumed: neither tool prefixed or
timestamped a line, and the two out logs are byte-identical to the script's
own echo; each daemon grew exactly one file during the window; pm2 ran at its
defaults with only name, script and interpreter set.
