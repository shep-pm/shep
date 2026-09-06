# Design: boot ordering with dependency trees

Status: designed 2026-09-06, not yet implemented.

Four questions from the maintainer, and this spec answers each of them by
name:

1. What sheep need to boot before other sheep?
2. What dogs need to boot before specific sheep?
3. Should all dogs boot first?
4. How long to wait between stages?

The answer to 3 is no, and the argument is hers: `shep-log-rotate` has to be
running before a sheep starts writing logs, and a metrics dog must not answer
for a flock that is not up yet. Both are true in the same flock, so dogs need
per-dog positioning rather than one global side.

This pass adds one Flockfile field, one `shep.toml` key, and one wire bump. It
does not touch a lookout pane.

## The problem

shep starts a flock all at once. `restorable()` splits the muster roll into
members and `to_start`, and `muster` hands the whole of `to_start` to
`start_restored` in one batch. Order within that batch is roll order, which is
insertion order, which means nothing.

For most flocks that is fine, because `autorestart` covers it: an app that
starts before its database crashes, backs off, and comes up on the second or
third try. What it costs is a boot that logs a burst of failures every time,
and a restart budget spent on a problem shep could have avoided. An app with
`max_restarts = 16` and a database that takes twenty seconds can exhaust the
budget and land `errored` on a machine where nothing is actually broken.

## What already exists

Established from the code, not assumed.

- **`do_start` is synchronous** (`supervisor.rs:2745`). It is a plain `fn` on
  the actor, reached from `handle_command`, which is itself driven by the
  actor's own message loop. Awaiting readiness inside it would block the loop
  that delivers `Msg::ReadyResult`, so the wait could never end. Any ordering
  that waits has to live outside the actor.
- **Readiness already exists and is already bounded.** `ReadinessSource::of`
  answers `Channel` for `wait_ready`, `Probe` for `readiness_probe`, and
  `Heuristic` for neither. `await_ready` is bounded by `listen_timeout` in
  every arm.
- **An app with no readiness signal is `Online` at spawn.** `spawn_fresh`
  computes `gated = !matches!(source, ReadinessSource::Heuristic)`
  (`supervisor.rs:3056`), so the ungated arm inserts the entry as `Online`
  with no readiness task at all. `probes/ready.rs`'s module doc says the
  `Heuristic` arm is *"reachable only from reload's `AwaitReady` state, not
  from `start`"*.
- **A readiness timeout is not a failure.** `handle_ready_result` marks a
  sheep `Online` anyway and warns, because *"treating a readiness timeout as a
  spawn failure would turn a slow-starting app into a restart loop"*.
- **Dogs are sheep entries.** `do_start_dog` calls `do_start` with a
  `DogSource`, so a dog occupies the same registry, emits the same bus events,
  and can be a graph node with no new machinery.
- **Dogs start after the restore, for a stated reason.** `boot.rs:770`:
  *"dogs after the restore so a metrics dog does not answer for an empty
  flock"*. `spawn_enabled_dogs` walks `options.dogs` in order and never fails
  the boot.
- **A dog's `dogs.toml` section is the dog's own config.** `dog_section`
  returns the operator's bytes, rebased, and `Request::DogConfig` carries them
  to the dog. There is no room in it for a key that means something to shep.
- **`AppConfig` is `#[serde(deny_unknown_fields, default)]`** (`app.rs:73`).
  An older peer decoding a newer peer's `AppConfig` fails on the unknown key.
- **`SheepConfigView` carries the whole `AppConfig`** (`request.rs:1239`), so
  a new field reaches the lookout config pane without any pane work.
- **Shutdown is fully parallel.** `begin_shutdown` collects every id with a
  live `ctl` and claims all of them at once under `LadderCap::Stop`, which
  reads `kill_timeout`, 1600ms by default.
- **`start` and `start_restored` already split by audience.** `snapshot.rs`
  argues it: `start` refuses a whole batch over an app whose script is
  provably missing, *"which is right for an operator typing `shep start` and
  wrong at an unattended boot, where a binary missing after a rebuild would
  cost the machine its entire flock"*.
- **A daemon-reload successor never restores.** `boot.rs` gates the restore on
  `options.restore && !inherited_flock`, and a successor inherits the flock
  whole. There is nothing to order on that path.

## Decisions

### 1. One Flockfile field, and shep derives the stages

```toml
[[apps]]
name = "api"
script = "./api"
depends_on = ["db", "cache"]
```

`Vec<String>`, `#[serde(default)]`, the same shape as `stop_exit_codes` and
`watch_options`. An empty list means the same as an absent one. A name refers
to a sheep or a dog.

The alternatives were a hand-written stage list in `shep.toml` and a numeric
`boot_priority`. Both were rejected for the same reason: they put the order
somewhere the app's own repository does not own. A Flockfile is a project
template committed alongside the app, so a requirement the app has belongs in
it. The cost is that cycles become possible and an unresolved name has to mean
something, and decision 5 says what.

`boot_priority` was rejected separately for saying when without saying why.
Nothing can validate it, a typo of 200 for 20 is silently a different boot,
and everyone who has used one has run out of gaps.

### 2. Three grammar refusals, decided in `normalize`

- **`name:slot` as a target is refused**, naming the app-level form. A
  dependency on one instance of a load-balanced app is not a claim about
  availability, and per-instance edges produce a graph nobody can read.
- **A self-edge is refused.** It is a one-node cycle and it is visible in a
  single `AppConfig`, so it never needs a graph to catch.
- **Duplicates dedupe** silently. There is nothing to tell the operator.

A dependency on a multi-instance app waits for every instance to reach
`Online`. Partial availability is partial service, and the instances spawn
concurrently, so waiting for all of them costs nothing over waiting for one.

### 3. A stage advances on readiness, and `listen_timeout` is the fallback

This is the answer to question 4, and it invents no new timeout.

An app that something later depends on is armed with
`ReadinessSource::Heuristic` instead of going straight to `Online`. It sits
`Starting` for its own `listen_timeout`, 3000ms by default, then flips
`Online` and the stage advances. The wait is honest in `shep flock`, because
shep really is holding the next stage on it.

```
db has no probe. listen_timeout = 3s.

  t=0.00  spawn db          db  starting
  t=3.00  deadline elapses  db  online
  t=3.00  spawn api
```

The operator cuts it to near zero by configuring a real signal, which is the
whole point:

```toml
[[apps]]
name = "db"
readiness_probe = { kind = "tcp", target = "127.0.0.1:5432" }
```

**Correction, 2026-09-06.** "Near zero" holds only if the probe's `interval`
is cut too, and the example above does not cut it. It defaults to 10 seconds,
and `await_ready` probes first and sleeps after, so an app that binds in 50ms
fails the poll at t=0 and has nothing left to try before `listen_timeout`
elapses at 3s. Measured on a real flock: 3.05s at the default interval against
0.14s at `interval = "100ms"`. As written, the two lines above buy an operator
no speedup at all.

Two rejected alternatives. Treating `Online` as ready outright would make
`depends_on` order the spawns and nothing else, so an operator who wrote it
expecting a wait would get no wait, no warning, and no sign anything was wrong
until the dependent crash-looped. A new `boot_delay` field would be a second
timeout concept next to `listen_timeout`, and a sleep is a guess that holds
until the machine is slow.

The cost is real and belongs in the docs: a three-stage flock of unprobed apps
costs six seconds more at boot. Only apps something depends on pay it.

The gating is per app, not per stage. `Command::Start` grows a
`gate: BTreeSet<String>` naming the apps in this batch that a later stage
depends on. `shep start db` on its own is untouched, because nothing later is
waiting on it.

### 4. Dogs stay last by default, and two things can pull one earlier

The default is unchanged, so no existing install moves. A dog runs in a final
stage, after every sheep, for the reason `boot.rs:770` gives.

Two levers promote one, and they compose under a single rule: **a dog runs at
the earliest stage anything asks for.**

```toml
# $SHEP_HOME/shep.toml
[daemon]
enabled_dogs    = ["metrics", "bark"]
adopted_dogs    = ["log-rotate"]
boot_first_dogs = ["log-rotate"]
```

```
  stage 0  log-rotate
  stage 1  db, cache
  stage 2  api
  stage 3  web, worker
  stage 4  metrics, bark
```

The second lever is a sheep naming a dog in its own `depends_on`, which puts
that dog in the stage before it. A sidecar dog an app genuinely needs lands
where the app needs it rather than in stage 0 ahead of everything.

**Correction, 2026-09-06.** That second lever is not what shipped. `boot`
spawns dogs in exactly two groups, the promoted ones before the restore and
everything else after every stage, so a plan position for a dog is never
honoured. A sheep naming a dog gets a warning rather than an earlier dog, and
`[daemon] boot_first_dogs` is the only lever that moves one. Putting each dog
at its own stage boundary is a design change and was deferred to its own task.

`boot_first_dogs` lives in `shep.toml` rather than in `dogs.toml` because a
dog's section there is passed through to the dog itself, so a shep key in it
would reach a program that does not know the key.

One limit, stated rather than hidden: promoting `log-rotate` to stage 0 fixes
the boot window only. A `shep start web` typed later, while `log-rotate`
happens to be down, is a supervision problem and boot ordering does not solve
it.

### 5. A bad graph refuses at the keyboard and carries on at boot

The split follows the `start` versus `start_restored` precedent quoted above.

| case | `shep start`, `shep add` | boot, `shep muster` |
|---|---|---|
| cycle | refuse, exit 4, name the cycle | warn, name it, run those nodes in a final unordered stage |
| unknown name | warn, edge satisfied | warn, edge satisfied |
| dependency has `autostart = false` | warn, edge satisfied | warn, edge satisfied |
| never ready | not reachable: `Online` at its own deadline | advance, warn |
| self-edge, `name:slot` | refused in `normalize` | refused on re-validation, entry lands in `rejected` |

An unknown name warns rather than refusing on both sides, because a dependency
on an app whose Flockfile lives in another repository is legitimate and
refusing it would make cross-repository dependencies unusable.

The cycle refusal names the actual cycle, `api -> db -> cache -> api`, which
means a DFS rather than reading off what Kahn's algorithm failed to emit.
"Some of these are in a cycle" is not something an operator can act on.

At boot nothing refuses, because a machine rebooting with nobody watching must
not be stranded by a typo. The worst case is a flock that is up and a log that
names every problem.

### 6. `autostart` wins over `depends_on`

`api` depends on `db`. `db` is registered, stopped, and sets
`autostart = false`. The daemon boots. `db` stays stopped, `api` starts in its
stage as though the edge were satisfied, and the boot warns naming both.

`depends_on` orders what is already being started. It does not decide what
gets started. Two fields, two jobs, one sentence each, and an explicit
statement on `db` is not overridden by a field in another app's file.

The cost is that `api` starts against a database that is not there and
crash-loops until somebody looks, with the warning as the only signal. The
alternative costs more: an operator reading `db`'s own config to find out why
it is running would not find the answer there.

### 7. Two new modules, because supervisor.rs is 19,965 lines

- **`shep_core::config::graph`** is pure. `AppConfig`s in, stages out, plus
  diagnostics. Kahn for the sort, DFS for naming a cycle, no I/O and no tokio.
  It lives in core because the Flockfile parser needs a document-local check
  and the daemon needs a full-flock one, and one algorithm beats two that
  disagree about what a cycle is. Names sort within a stage, so the plan is
  deterministic.
- **`shep_daemon::boot_order`** is the async driver. It holds a
  `SupervisorHandle` and a `Bus` subscription, starts a stage, waits, and
  advances.

The driver subscribes before it starts anything, the way `boot` already
subscribes `spawn_dog_watch` ahead of the supervisor so it *"cannot miss an
`Errored` a dog reaches during the restore step"*.

A stage advances when every member has reached a terminal answer: `Online`, or
`Exit`, or `Errored`. A member that dies resolves at once rather than holding
its stage for the full deadline. The driver's own bound is the longest
`listen_timeout` in the stage plus slack, and it is a backstop only, since
each member's readiness task already flips it at its own deadline.
`RELOAD_DEADLINE_SLACK` is the precedent for the slack and the reasoning is
the same: scheduling jitter, not a real wait.

### 8. The CLI computes its own deadline

A four-stage start costs nine seconds of heuristic waits, and shep-client's
`DEFAULT_DEADLINE` is five. Left alone, `shep start Flockfile.toml` would fail
with `DeadlineExceeded` while the daemon was doing exactly what it was asked.

The CLI holds every `AppConfig` it is sending, so it can compute the worst
case: the sum of every `listen_timeout` plus slack, which is every app in its
own stage. That is the move `shep logs -f` already makes with
`LOG_PLANE_DEADLINE`, and `action_timeout`'s own rustdoc argues for it.

`shep kill`'s deadline needs the same treatment once shutdown is staged. The
plan checks it rather than assuming.

### 9. Shutdown reverses, with dogs held out of it

Staged shutdown runs from `RunningDaemon::run`'s teardown, before the existing
`SupervisorHandle::shutdown`, which stays as the backstop so a driver bug
cannot leave a child alive. Each stage is bounded by its members' own kill
ladders under `LadderCap::Stop`.

```
today, every sheep at once:
  t=0.0  SIGTERM db, api, web, worker
  t=1.6  SIGKILL whatever is left

staged, reverse:
  t=0.0  SIGTERM web, worker
  t=1.6  SIGTERM api
  t=3.2  SIGTERM db
```

Worst case moves from 1.6s to 1.6s per stage. Five stages is eight seconds
against systemd's default `TimeoutStopSec` of 90s, so `unit.rs`'s template
needs no new key.

What it buys is a worker draining its queue against a database that is still
answering. Without it, both get SIGTERM in the same millisecond.

**Dogs are not in the reverse stages.** They all stop in the backstop, after
every sheep. Monitoring should outlive what it monitors, and strict reverse
would kill bark before the flock it is meant to report on. This is a
deliberate deviation from reversing the boot order exactly.

### 10. Restart and reload walk stages forward

Not reverse-stop then forward-start. The rolling version sounds more correct
and behaves worse: it puts the whole fold down at once in the middle, where
forward-only never does. Each stage's members restart or reload as they do
today, and ordering decides only when the next stage begins.

Reload keeps choosing `Serial` or `Overlap` per app through
`ReloadMode::of`. A stage completes when every member has emitted `Reloaded`
or `ReloadAbandoned`.

### 11. `depends_on` is `NextSpawn`

Measured against read sites rather than guessed from the name. It is read once
when a batch is ordered: boot, muster, a staged start, a staged restart or
reload. Nothing re-reads it while a sheep runs, so it is not `Live`. Nothing
is baked into the child, so it is not `NeedsRespawn`. The flock's shape is
unchanged, so it is not `Structural`.

`autostart` is the closest existing entry and carries the same shape, with the
comment *"Read once at muster or boot, by `restorable()`"*.

`GROUP_ORDER` in `scaffold.rs` puts it in `process`, beside `fold` and
`instances`, all three being about this app's place in the flock.

## Wire

`PROTOCOL_VERSION` moves from 4 to 5.

The protocol's own evolution rule says a new serde-defaulted field keeps the
version. That rule assumes the receiver tolerates unknown fields, and
`AppConfig` does not: it is `#[serde(deny_unknown_fields, default)]`, so a
daemon at protocol 4 fails to decode a `depends_on` a newer client sends.

This is the same class as the 2 to 3 move for `ResetDepth`. It regresses live
functionality for a daemon that has simply not restarted since an upgrade,
rather than only making a new feature unreachable. The bump turns a dead
client into a named `protocol_mismatch` refusal, exit 6, at the handshake.
Not `version_skew`, exit 12: `refuse_version_skew` runs only after
`connect_or_spawn` returns `Ok`, and a protocol refusal fails the handshake.

`SCHEMA_VERSION` stays 1. `ProcessInfo` gains `depends_on: Vec<String>`, which
is additive, and that envelope moves only on a rename, a removal, or a retype.

`crates/shep-core/assets/flockfile.schema.json` regenerates from the parser's
own document type through schemars.

## Surfacing

Deliberately small.

- `shep describe` lists `depends_on`, which answers "why did web start nine
  seconds in".
- The boot logs the stage plan once, on one line.
- No new `shep flock` column. That table already drops columns under pressure
  and this is not per-row status.
- `shep lookout` gets nothing. Its panes are being redesigned separately and
  `SheepConfigView` already carries the field, so it appears in the config
  pane on its own.

## Out of scope

- **Editing `depends_on` from lookout.** `Request::SetSheepField` does not
  take a list-valued field today. That is the follow-up, and no pane work
  happens here.
- **Ordering a single-target start.** `shep start api` starts `api`. Ordering
  applies when shep is starting more than one thing at once.
- **A required versus optional distinction.** systemd's `Requires=` against
  `After=`. One field, one meaning, until somebody hits the case.
- **Ordering across a daemon handover.** A successor inherits the flock and
  never restores, so there is nothing to order.

## Testing

Fast tier throughout, with `listen_timeout` at 50ms in fixtures. An
event-order assertion against the fake runner is not a duration assertion, so
almost nothing here earns a `mod slow`.

shep-core:

- edges respected, and stable ordering within a stage
- a cycle named exactly, not merely detected
- self-edge, `name:slot`, unknown name, duplicate entries
- a proptest over random DAGs asserting every edge holds. proptest is already
  a workspace dependency; shep-core needs it as a dev-dependency.

shep-daemon:

- stage 2 does not start before stage 1 is online
- a member that exits mid-stage does not hold its stage
- a cyclic muster roll still brings the flock up, cyclic nodes last
- a dependency with `autostart = false` warns and the dependent starts
- dogs land in the final stage by default
- `boot_first_dogs` puts a dog in stage 0
- a sheep's `depends_on` pulls a dog earlier
- shutdown observes reverse order
- dogs stop in the backstop, after every sheep

shep:

- a cycle refuses with exit 4 and names the cycle
- the computed deadline is the one actually sent

## Docs

Operator-visible, so `web/` is part of the task.

- New `web/src/pages/docs/boot-order.astro`, plus the nav. Big enough for its
  own page, the same call `folds.astro` made.
- Edits to `lifecycle.astro`, `first-flockfile.astro`, and `dogs.astro` for
  `boot_first_dogs`.
- `docs/dogs.md` for `boot_first_dogs`.
- `docs/decisions.md` for the protocol bump, gating `Heuristic` on a
  dependency, `autostart` winning, and dogs staying last with two promotions.
- `CLAUDE.md` quotes `boot.rs`'s order comment verbatim and that comment
  changes.
- `cargo build --release`, then `generate-cli-reference.sh`, then
  `astro build` and `astro check`.
