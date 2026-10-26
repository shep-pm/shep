# Deferred — what is not in v1.0, and why

The single list. Spec §2's "v1.1 committed" section names six items
deferred by design; everything else below is named as v1.0 scope in the
spec (§2, §3, §5, §6, §8, §9) but is not built as of the 2026-08-12
spec↔implementation audit (`feat/phase8-cutover` at `fc3679e`, 883 tests
passing locally, 1 ignored). A spec section is a plan, not a shipped-state
claim — drift between the two is what this file exists to stop hiding.
Linked from spec §2.

## Scope decision, 2026-08-12: everything below §2's six cuts ships in v1

The maintainer's call, after the five v1.1 audits came back: *"we should probably fix
everything in v1. We're not in a rush to release this to the public. We want
a hot looking app right off the bat if we have to compete with well
established apps like pm2 and other rust attempts."*

So this file now holds two different kinds of thing, and the section headings
say which is which. The six items under "Committed to v1.1+ by design" are
still deferred — they are scope cuts the spec argues for. Everything under
"Named as v1.0 in spec §2/§9, not yet built" is a **build queue**, in this
order:

1. **The audit debt** — what the five 2026-08-12 audits turned up. Real bugs
   first (`kill_signal` accepts a typo and then sends the wrong signal
   forever; an on-time `ActionReply` can be matched to the wrong request),
   then the wire and config asymmetries, then the tooling and doc staleness.
2. **The rest of the v1.0 surface** — serve, dev/runtime, `.js` Flockfile,
   schemars, the daemon-config flags layer, and openrc + BSD rc.d.
3. ~~**The Windows functional tier — last**~~ (the maintainer, 2026-08-12).
   **Superseded 2026-08-15: Windows left this queue for the v1.1+ section**
   below. The estimate that was "mostly guesswork" has since been made, and it
   is what changed the decision — see
   [windows-estimate.md](windows-estimate.md). The tier shipped on 2026-08-26,
   eleven days after that move, and its record is under "Not deferred" in
   [deferred-history.md](deferred-history.md).

**Dogs** (spec §8) was originally queued first and has since shipped, on
`feat/phase9-dogs`; see "Not deferred" below for what landed. **whistle**
(spec §8, §13) has since shipped too, on Phase 13; same section.

Ordering is not priority. Windows was last because its estimate was the
weakest; the estimate was made, the tier moved to v1.1+, and it shipped there
on 2026-08-26.

## Committed to v1.1+ by design (spec §2)

Five deliberate scope cuts, not oversights — spec §2 carries the reasoning.
There were six until 2026-08-26, when the Windows tier stopped being one
of them: it shipped, and its entry moved to "Not deferred" below.

- HTTP/SSE MCP transport (whistle ships stdio-only first). Tracked as https://github.com/shep-pm/shep/issues/343
- cgroup v2 enforcement (`enforce = "kernel"`) — `LimitEnforcer`'s polling
  impl is the v1.0 tier. Sized 2026-09-06, together with the CPU half this
  line never mentioned: see the subsection below. Tracked as https://github.com/shep-pm/shep/issues/344
- `@shep/io` npm shim (built on demand). Tracked as https://github.com/shep-pm/shep/issues/362, with Python
  alongside it at https://github.com/shep-pm/shep/issues/363
- vcs metadata (`vcs` feature, off by default). Tracked as https://github.com/shep-pm/shep/issues/345
- `shep web` JSON status endpoint. Resolved, 2026-08-13: the metrics dog
  does not cover this — it serves Prometheus exposition text for a
  scraper, and `shep web` was a hand-fetched JSON payload for a
  dashboard, an incompatible shape for an incompatible consumer. This
  stays its own deferred item rather than being folded into the dog.
  Tracked as https://github.com/shep-pm/shep/issues/346

### Per-sheep resource limits, and where `enforce = "kernel"`'s cost sits, sized 2026-09-06

Sized against the code, not built. The shape is two config fields carrying
docker's semantics, chosen deliberately so nobody has to learn a shep-specific
model:

- **`max_cpu_cores`**, matching `docker --cpus=N`. The sheep's process tree
  gets at most N cores' worth of CPU time per scheduling period, scheduled
  wherever the kernel likes. Not affinity, and not core pinning.
- **`max_memory` gaining an enforcement strategy**, matching
  `docker --memory=2g`. The kernel caps the tree instead of shep sampling it
  every `MEMORY_POLL_INTERVAL` and restarting it.

**Affinity is the obvious first guess and it is the wrong one.** Pinning a
sheep to a set of cores forces shep to choose *which* cores, and with several
sheep that choice becomes a placement policy. Either every sheep stacks onto
cores `0..N` and contends there while the rest of the machine idles, or shep
becomes a scheduler making placement calls without being able to see anything
else running on the host. A quota needs no placement decision at all: the
kernel keeps scheduling the tree wherever it likes, and stops scheduling it
once the slice is spent.

#### The cost is one thing, and it is neither field

Nothing in `crates/` creates, writes, or detects a cgroup. Grepping the
workspace for `cgroup` returns three comments and no code
(`crates/shep-daemon/src/runner.rs:900`,
`crates/shep-daemon/src/limits/mod.rs:54`, and
`crates/shep-cli/src/lookout/source.rs:353`). The work is the container
itself: a per-sheep cgroup created at spawn with the child moved
into it before it forks anything, v2 versus v1/hybrid detection, delegation
detection, and a clean refusal rather than a silent no-op when none of that is
available. Call it phase 0. Against it, each field is one file write:
`cpu.max` and `memory.max`.

Sizes below are relative to each other, not an estimate in days:

| Work | Size |
| --- | --- |
| Phase 0, the per-sheep cgroup | L |
| `max_cpu_cores`, writing `cpu.max` | S |
| `max_memory` with `enforce = "kernel"`, writing `memory.max` | M |

**Windows already does most of what phase 0 needs, which is the finding worth
recording.** The spawn path calls `command.spawn()`
(`crates/shep-daemon/src/tokio_runner.rs:705`), reads the pid, and immediately
creates a job object and assigns the child to it (`tokio_runner.rs:716-738`).
The unix arm of that same function has only `command.process_group(0)`
(`tokio_runner.rs:571-572`) and no per-sheep resource container.

**The analogy stops one step short, and the gap is a race.** Read the Windows
comment exactly: "the child exists and everything it spawns **from here**
inherits the job". From here, not from birth. `spawn()` returns a process that
is already running, so anything it forks between that return and the
containment call is outside. On Windows that costs little, because a job is
about reachability for `kill_tree` and a stray early grandchild is rare. On
Linux it costs correctness: writing a pid to `cgroup.procs` moves that thread
group and **not** its existing descendants, so an early fork stays in the
parent cgroup and its memory is never counted against `memory.max`. A shell
wrapper that execs a real program is the ordinary case, not a corner one.

So phase 0 cannot copy the Windows site verbatim. The child has to join the
cgroup before it can fork, which on unix means either `pre_exec`, where the
child writes its own pid to `cgroup.procs` after fork and before exec, or
`clone3`'s `CLONE_INTO_CGROUP`, which is atomic and needs Linux 5.7. `pre_exec`
is the portable one and runs in the narrow post-fork window where almost
nothing is safe to call, which is its own cost. Sizing phase 0 as L already
assumed a real spawn-path change; this is what that change is.

#### `LimitEnforcer` cannot be the seam, and the reason is timing

`arm()` fires at the transition to `Online`, not at the spawn, and
`went_online`'s own rustdoc says so: "Arming happens at the transition, not the
spawn: a liveness probe armed against an app that has not finished starting
fails its threshold and restarts the app before it ever comes up"
(`crates/shep-daemon/src/supervisor.rs:7565-7590`). For the polling enforcer
that costs nothing, because it sums a tree from outside and needs no
cooperation from the child. A cgroup only accounts for a process that was
already inside it before it forked, so arming seconds late means arming
against descendants that have already escaped.

So create the cgroup unconditionally for every sheep at spawn on Linux,
configured limit or not. Unconditionally means every sheep on a host that has
the machinery, never every sheep everywhere: macOS has no cgroups and refuses
both fields outright, so there is nothing to create there and a sheep starts
exactly as it does today. Windows already builds its job for every sheep with
no limit set, and `Job::create`'s own comment says why the zeroed limit block
is written out explicitly, that doing so "makes adding a limit later a one-line
edit at a site that already handles its own errors"
(`crates/shep-daemon/src/sys_windows.rs:87-127`).

`arm()` then reduces to writing a limit into a container that already exists,
which preserves `max_memory`'s current `ApplyGroup::Live` classification
(`crates/shep-core/src/config/apply.rs:48`): a config change still reaches a
running sheep through a re-arm, with no respawn.

#### The trait's own doc contains a claim this design breaks

`LimitEnforcer`'s rustdoc says the cgroup implementation "must replace the
polling one without the engine noticing"
(`crates/shep-daemon/src/limits/mod.rs:54-56`). That is false on one axis, and
the axis is the restart budget.

A polling breach does not merely skip `max_restarts`, it **resets** it.
`extra_restart` delegates to `begin_manual` rather than `respawn`, which
"keeps the kill ladder and the budget reset" (`supervisor.rs:1131-1136` and
`supervisor.rs:7092-7094`), and the module doc states the rule plainly: "its
restart does not count against `max_restarts`" (`limits/mod.rs:11-12`). A
cgroup OOM kill arrives instead as an ordinary `SIGKILL` crash through the
normal exit path, entirely outside the trait, and so it consumes the budget.
An app that leaks and today restarts indefinitely would instead burn its
`max_restarts` and get parked.

**That is the better case. The worse one is that nothing arrives at all.** The
cgroup OOM killer picks one victim by its own heuristic, and it need not be the
pid shep supervises: kill a lamb and the sheep stays up, over its ceiling, with
no exit to classify and no breach to report. Today's polling enforcer has no
such hole, because it sums the tree and restarts the root whatever died. So the
design needs `memory.oom.group=1`, which makes the kernel kill every process in
the cgroup together and turns an OOM into exactly the root-pid exit the engine
already understands. Setting it is one more file write in phase 0 and it is not
optional: without it, the whole feature silently degrades to "sometimes".

Closing that means teaching the exit path to decide whether an exit was this
sheep's own OOM, and that is harder than reading one number. `memory.events`
is hierarchical, so its `oom_kill` counts kills anywhere at or below the
cgroup, and an ancestor or a host-wide OOM can raise a sheep's counter without
the sheep's own `memory.max` ever being reached. `memory.events.local` is the
one that excludes descendants. Both are cumulative counters rather than events,
so a non-zero value proves nothing on its own and even a matching delta across
an exit is correlation: read the local counter before the spawn and again at
the exit, treat only a rise in that window as this sheep's OOM, and decide in
advance what an ambiguous exit counts as. Conservative is to charge it to the
budget, since the alternative is a crash loop that never parks.

None of that is a swap behind `LimitEnforcer`. It is new logic in the exit
path, and it is the kind of thing found late and expensively.

Separately, extending `arm(&self, id, root_pid, limit: MemSize)` is a
**breaking change**. The trait is public specifically so
`crates/shep-daemon/tests/external_impls.rs:28-36` can implement it from
outside the crate, and that test's `impl` block is the compile-time proof. CPU
should not go through the trait in any case, since docker's `--cpus` has no
breach event to report.

#### CPU never kills. Memory does

`cpu.max` takes `$MAX $PERIOD`, both in microseconds, and the CFS bandwidth
controller simply stops scheduling the cgroup's tasks until the next period
begins. Nothing dies, the tree runs slower. runc computes the same value the
same way: quota is cores times period.

Note which way that scales, because Windows scales the other way and doing
Linux first is how the wrong model gets carried across. On Linux more than one
core is a quota **above** the period: two cores at the default 100000us period
is `200000 100000`, and the number grows without a ceiling. On Windows the
ceiling is the whole machine, so more than one core is a **larger fraction** of
a fixed 10000. One conversion multiplies by the core count, the other divides by
the host's.

**Swap has to be named, because `memory.max` alone does not bound memory.**
`memory.max` caps memory and says nothing about swap, so a sheep at its ceiling
can keep going into `memory.swap.max`, which defaults to no limit. Docker does
not leave this implicit either: `--memory=2g` with no `--memory-swap` allows
2g of swap on top, for 4g of total footprint. Borrowing docker's flag without
its swap default would give shep a ceiling that is not one. The recommendation
is `memory.swap.max = 0`, so `max_memory` means what an operator reading the
name expects and what today's polling enforcer already means, since that sums
RSS and a swapped-out sheep looks under its limit either way. It diverges from
docker's default deliberately, and the entry says so rather than inheriting a
2x footprint by silence. Windows needs the same sentence for a different
reason: `JOB_OBJECT_LIMIT_JOB_MEMORY` bounds committed virtual memory, not
resident memory, so the same configured number does not mean the same thing on
the two platforms and the docs page has to say which.

`memory.max` is not the symmetric knob to `cpu.max`. Under Linux overcommit the
kernel does not politely refuse the allocation that crosses the line. It
reclaims, and when reclaim fails it invokes the cgroup-scoped OOM killer on a
process it picks by its own heuristic, which need not be the one that
allocated. Worth
stating explicitly, because "cap it instead of restarting it" sounds like a
graceful refusal and on Linux it is not. `memory.high` is the genuine
throttle-without-killing knob, applying reclaim pressure and stalling the
allocator rather than killing anything. It is a plausible later addition, not
what `max_memory` should mean.

#### Platforms, which decide the staging

**Linux** gets real enforcement for both fields, entirely gated on phase 0.
Delegation is the practical question of whether an ordinary user gets the
feature at all:

| shep runs as | cgroup write access |
| --- | --- |
| root | always works |
| a systemd system unit | needs `Delegate=yes`, which the shipped unit does not set |
| a user login session | whatever `user@.service` already delegated, which varies by systemd version |

The shipped unit is rendered at
`crates/shep-cli/src/commands/startup/unit.rs:57-71` and its `[Service]` block
carries no `Delegate=`. Adding one also means updating the exact-string test
at `unit.rs:393-407`, which pins the rendered unit verbatim.

**Writable is not the same question as enabled, and the refusal has to ask
both.** A directory shep can write does not mean `cpu.max` and `memory.max`
will exist in the cgroups it creates under it. In cgroup v2 a controller
appears in a child only when the parent lists it in `cgroup.subtree_control`,
which the parent can only do for controllers it has in its own
`cgroup.controllers`, and the top-down rule means enabling one can simply fail.
So phase 0's probe is three checks and not one: the subtree is writable, the
controller is present in `cgroup.controllers`, and it is enabled in
`cgroup.subtree_control` or can be. A limit accepted without all three is a
limit shep cannot enforce. Where any of them fails, refuse the configured limit
at load with a message naming which one and why, and never accept the field
while enforcing nothing.

**Windows** gets real enforcement for both, and memory is the cheap half. It
also matches the intent better than Linux does:
`JOBOBJECT_EXTENDED_LIMIT_INFORMATION` is already allocated, zeroed and
submitted in `Job::create`, and `JOB_OBJECT_LIMIT_JOB_MEMORY` makes the
over-limit commit **fail** rather than terminating the process. CPU needs a
second info class, `JobObjectCpuRateControlInformation`, with
`ENABLE | HARD_CAP` rather than the weight-based mode, which expresses a
relative priority and not a quota.

`CpuRate` is the conversion worth writing down, because it reads backwards and
a review on 2026-09-13 got it backwards. It is hundredths of a percent of the
**whole machine**, capped at 10000, not of one core: 10000 means every cycle
the host has. One core is therefore not a fixed number, and `max_cpu_cores`
cannot be scaled without knowing how many processors the host has. The
conversion is `cores * 10000 / processor_count`, which is what moby computes
for `--cpus` on Windows, so `max_cpu_cores = 2` on an eight-processor host
writes 2500 and not 20000. The cap is the tell: if the rate were per-core,
expressing two cores would need a value above 10000, and the API refuses one.

`CpuRate` is also an integer, and that bites at the small end rather than the
large one. `cores * 10000 / processor_count` floors, so a genuinely configured
fraction can land on zero on a big host: 0.05 cores on a 128-processor machine
computes 3, and 0.01 computes 0, which Windows rejects outright. Docker's
`--cpus` takes fractions, so `max_cpu_cores` has to as well. Clamp the result
into `1..=10000` rather than passing it through: a floor of 1 gives the
smallest limit the API can express, which is the honest answer to asking for
less than that, and the ceiling keeps a `max_cpu_cores` above the host's
processor count from being refused when it should simply mean "all of it".

**macOS** has no mechanism for either, and this has to be a documented
refusal. `RLIMIT_AS` is defeated by ordinary virtual-address reservations, so
a runtime that reserves a large arena up front trips it while using almost
none of it. `RLIMIT_RSS` is not enforced by XNU. Jetsam memory watermarks need
an entitlement Apple does not grant to third parties. The refusal belongs at
the call site with its reasoning attached, in the style shep already uses for
the Windows signal refusal at `tokio_runner.rs:215-278`.

**The macOS consequence is the largest cost here and the least obvious.**
macOS is shep's primary development platform, so none of the enforcement code
can ever be exercised in the ordinary local edit-and-test loop. Every change
needs a Linux host or the Windows box. That is a permanent iteration tax for
the life of the feature rather than a one-time port, and it should shape the
staging more than any individual API's difficulty.

#### Smaller decisions, recorded so they are not reopened

- **`enforce` is the right field name.** It is already published in this
  file's own v1.1 line above.
- **It should be a sibling enum**, following the `ProbeKind` precedent at
  `crates/shep-core/src/config/app.rs:18-25`. Not a bool, and not a variant
  nested inside `max_memory`: `MemSize` is a plain validated newtype
  (`crates/shep-core/src/values.rs:39`) and is not built to carry a strategy
  tag.
- **`max_cpu_cores` wants a validated newtype** of its own, following
  `MemSize` and `UpDuration` (`values.rs:39`, `values.rs:204`), rather than a
  bare `f64` that silently accepts zero, a negative, or a NaN. Fractions are
  in, because `docker --cpus` takes them, so the type's job is to refuse
  nonsense rather than to refuse non-integers. Refusing is not the whole
  contract either: the per-platform conversions round, and the Windows one can
  floor a valid fraction to a value the API rejects. The clamp belongs at the
  conversion, not in the type.
- **Both fields need a row in the `FIELDS` table** at
  `crates/shep-core/src/config/apply.rs:35`, which `is_classified` reads as
  the anti-drift gate.
- **`SCHEMA_VERSION` does not move.** The rule at
  `crates/shep-cli/src/output/mod.rs:58-59` is that it is "bumped only for a
  breaking change to any command's `data` shape. Additive fields do not bump
  it", and a new key in a rendered config view is additive.
- **`PROTOCOL_VERSION` is not as clear-cut.** Its own subsection below.

#### The `PROTOCOL_VERSION` answer is not automatic, and the maintainer owns it

Two documents disagree, and the disagreement is worth settling before either
field is written rather than during review.

The rule at `crates/shep-core/src/protocol/mod.rs:50-54` says "Additive
optional fields (new serde-defaulted `Option<T>` fields, new variants behind
`#[non_exhaustive]`) keep the version". `AppConfig` is `#[serde(default)]` and
carries no serde `deny_unknown_fields`, so by that rule neither field bumps
anything.

The tripwire test `a_new_app_config_field_forced_the_protocol_version_up`
(`protocol/mod.rs:70-80`) says the opposite for `AppConfig` specifically, and
names two precedents where it held: `depends_on` forced 5, `environment`
forced 8.

**That test's stated reason is stale.** It argues from `AppConfig` being
`deny_unknown_fields`, and the serde attribute has since moved to
`Flockfile::parse` (`crates/shep-core/src/config/app.rs:88-101`, which ends
"Do not restore the serde attribute here"). The old failure mode, an older
peer refusing the whole payload, is gone.

What replaced it is worse for this particular field. An older daemon handed
`max_cpu_cores` ignores the key and runs the sheep with no CPU limit at all,
silently. A silently dropped resource limit is a better argument for a bump
than the additive rule is against one. Decide it deliberately rather than
letting the additive rule settle it by default.

#### Recommended order

Phase 0, then memory, then CPU.

Memory second, because it finishes the v1.1 item already committed above. CPU
third, because it is nearly free once phase 0 exists. Starting with CPU
because it looks like the smaller of the two does not work: it needs the same
foundation, and doing it first buys a field whose only working platform is
Windows.

## Named as v1.0 in spec §2/§9, not yet built

Schedule rather than design is what leaves these open. Where a phase has
landed part of a spec section, the entry names the part still missing rather
than the whole section. See `docs/systematic-refactor/refactor-workspace/`
for what phase is next.

The queue is empty as of 2026-10-14. Its last item, OTLP export on the
metrics dog, shipped that day with the channel samples that were its missing
half. Its entry, and every other item this section used to hold, is now in
[deferred-history.md](deferred-history.md) under "Not deferred".

## Known debt, recorded rather than built

### What the reload-readiness fix does NOT cover -- open, 2026-08-28

Three residuals, recorded because each was a deliberate stopping point rather
than an oversight.

**The post-drain probe is exact only for a single-instance app.** An
overlapping reload of a probed app (`reuse_port = true`) asks its probe a
second time once the drainee is reaped, because `SO_REUSEPORT` means the kernel
may hand either instance a connection and even a late probe can be answered by
the one on its way out. With one process left, an answer proves that process
answered. With a CLUSTER, a reload replaces one instance at a time, so the
surviving old instances are still in the group and can still answer for a bad
replacement until the last swap. Closing it needs a per-instance identity in
the probe response — the app naming which process it is — which is exactly what
`wait_ready` already provides and is why the code points a clustered app at the
channel instead of growing a second mechanism.

Tracked as https://github.com/shep-pm/shep/issues/351.


**A replacement that fails its readiness check keeps its process and loses its
lifecycle extras.** It is left registered and `Starting` rather than killed,
because with the drainee gone, killing it would empty the instance slot
outright. But extras are armed at `went_online`, so an instance parked this way
has no liveness loop and nothing will restart it on its own; the operator has
to act on the `process.reload_abandoned` event. Arming a liveness loop against
a process that is not `Online` is a wider change than this fix wanted —
`handle_extra_restart` guards on that status for four separate callers.

Tracked as https://github.com/shep-pm/shep/issues/289.


**A deploy tool's patience is sized off `listen_timeout + graceful_timeout`,
and a `reuse_port` reload now costs one more `listen_timeout` than that.**
shep-deploy derives its verify budget that way (`deploy.rs::budget`), so for
the one combination of `reuse_port = true` AND a probe, the post-drain check
can outlast the budget and roll back a healthy release. A false rollback rather
than a false success, which is the right direction to fail, but it is a real
interaction and the fix belongs on shep-deploy's side of the line.

Tracked as https://github.com/shep-pm/shep-deploy/issues/20.


### `ProcessInfo` fuses four concerns behind one discriminator

Identity and lifecycle (`id`, `name`, `status`, `pid`, `restarts`,
`uptime_ms`, `fold`), log paths (`out_file`, `err_file`), resource stats
(`cpu_percent`, `memory_bytes`) and dog provenance (`dog`) all ride in one
struct, and a dog's row leaves several of them meaningless.

Deferred on the wire audit's own recommendation: do not split speculatively.
The trigger it named was the `lambs` field, and that trigger has fired.
`ProcessInfo::lambs` is on the wire, and the shepherd fills it
(`rpc/enrichment.rs` walks each sheep's process tree into the row), so a row
does carry a process tree and the question of what a `FlockMember` is has
stopped being cosmetic. The split is still open: the field arrived the cheap
way Phase 10 built for it (`ProcessInfo` is `#[non_exhaustive]` with a
builder), and nothing since has forced the four concerns apart.

Tracked as https://github.com/shep-pm/shep/issues/352.

### `check_log_ancestry`'s TOCTOU window, and the Linux syscall that would close it

`check_log_ancestry` verifies a log path's ancestry and `open_log_path` then
opens it, with no atomic tie between the two. The realistic local-multiuser
attack is caught — a loose or wrong-owned ancestor is refused, and
`O_NOFOLLOW` refuses a symlink standing at the final component — but an
attacker who can rearrange a directory between the check and the open still
wins that race.

The design, written down so it does not have to be rediscovered:

- Linux fast path: `nix::fcntl::openat2` (available under the `fs` feature this
  crate already enables) with `ResolveFlag::RESOLVE_NO_SYMLINKS`, opening
  relative to a directory fd for the log directory.
- The `RawFd` it returns is adopted into a `File` with `FromRawFd`, which is
  `unsafe`, so the wrapper lives in `shep-daemon/src/sys.rs` with a per-block
  `// SAFETY:` (IR-22/23) and nothing else in the crate touches the raw fd.
- Fallback ladder: `ENOSYS` (kernel < 5.6) and `EPERM` (seccomp filters that
  do not allow the syscall) both fall through to today's
  check-then-`O_NOFOLLOW`-open path, which stays as the portable
  implementation and remains the only path on macOS.

Not built in Phase 10 because it is new `unsafe` on a Linux-only path that this
project cannot execute a test for from a macOS development machine — the exact
shape of debt the platform audit's "never been compiled" finding exists to
complain about. What would force it: a Linux box in the regular test loop, or a
threat model that includes an attacker with write access to a log directory's
parent.

Tracked as https://github.com/shep-pm/shep/issues/353.

### `shep signal` cannot reach a sheep's lambs, on purpose

`signal` delivers to the sheep's own pid. An operator who wants a whole
process tree to get a `SIGHUP` — the nginx-worker shape — has no verb for it:
`stop` signals the group but also runs a kill ladder behind it, and there is
no group-wide nudge.

Deferred rather than built because the two are genuinely different asks and
one flag on `signal` (`--group`) would make the safe reading the non-default
one. What would force it: an app class where the sheep is a supervisor that
does not forward signals to its own workers, which is a real shape and simply
has not come up here yet.

Tracked as https://github.com/shep-pm/shep/issues/354.

### `UpDuration`'s grammar tops out at hours, which only bites outside shep

Found 2026-08-20 while building `shep-log-rotate`. Its `max_age` setting is a
log-retention window, so the natural spelling is `7d`. The grammar is
`^\d+(ms|h|m|s)?$` (`crates/shep-core/src/values.rs`), so `7d` is refused and a
week has to be written `168h`. A month is `720h`.

**For shep itself the grammar is right and should not change on this
argument.** Every duration shep owns is a lifecycle timer -- `min_uptime`,
`kill_timeout`, the backoff curve -- and nobody sets a kill timeout in days.
A day unit would be dead weight on every one of them. That is presumably why
it is not there, and it is a good reason.

What is new is that **dogs have durations shep does not**, and retention is
the obvious one: any rotator, any archiver, any bark-history trimmer wants
days. A dog that follows the ecosystem's spellings, as `shep-log-rotate`
deliberately does, inherits a grammar that was scoped for a different kind of
value. The alternative is a dog inventing its own duration parser, which is
the thing the ecosystem rule exists to prevent.

**The two halves carry different risk, and only one of them is breaking:**

- **Parsing `d` is purely additive.** `"7d"` errors today, so nothing that
  works now would change. `UpDuration` is `u64` milliseconds, so a day is
  `86_400_000` and even `49_710d` fits without overflow.
- **Rendering `d` is a wire-visible change.** `Display` currently walks
  hours, then minutes, then seconds, so a `d` arm placed before the hours arm
  would make `min_uptime = "24h"` round-trip as `"1d"`. That changes what
  `shep describe` and `--format json` print for configs nobody edited, and
  the type's own comment says "changing this is a breaking change (string
  form in `AppConfig`)".

So parse-only is available cheaply and asymmetrically, if it is wanted. Not
picked here: the grammar is the maintainer's, it is a wire decision, and the exercise's
job was to find the friction rather than resolve it.

Tracked as https://github.com/shep-pm/shep/issues/355.

### ~~A third-party dog has no way to ship its own defaults~~ -- done, 2026-10-24

Raised 2026-08-20 while designing `shep-log-rotate`, the first fully external
dog. `shep adopt <name> <path>` vets, registers, enables and starts in one
command, and then the operator has an adopted dog with no `[<name>]`
section in `dogs.toml` and nothing telling them what its knobs are. The
README was the only answer.

The maintainer asked whether a dog's repo could ship a `Flockfile.toml` that `shep adopt`
reads. It cannot: `RawFlockfile` is `deny_unknown_fields` over exactly
`$schema` and `app`, so a Flockfile cannot mention a dog. That is a
file-ownership line rather than an oversight. A Flockfile is the operator's
file, committed in a service's repo, describing what to supervise. `shep.toml`
is the daemon's, one per machine, holding dogs, style and interpreters. A dog's
registration belongs to the machine.

**Built as the manifest's `defaults` field** (#1073, #1086): a dog answers
`--shep-manifest` with TOML text for its own section, and `shep adopt`
writes it as `[<name>]` in `dogs.toml` when the file holds no section for
the dog, and never over one that exists. The trust question was answered by
the route rather than by a new file: the defaults arrive through the
manifest probe `adopt` already runs on the vetted binary and caches by its
sha256, so shep parses no file the dog's author wrote. The
`--print-config`-style flag this deferral recommended as the near-term
answer is no longer needed, and `docs/dogs.md` and the writing-a-dog page
say so.

Tracked as https://github.com/shep-pm/shep/issues/356.

### `shep install` does not exist, and a scanner is not what would make it safe

Asked by the maintainer on 2026-08-26, the day the first external dog published.

**What shep offers today is discovery, not installation.** `shep dogs
--available` reads the community index and prints two copy-pasteable commands
per entry: `cargo install <package>`, then `shep adopt <name> <path>`. shep
runs the second and vets the binary before registering it. The operator runs
the first. Nothing in shep fetches, builds, or executes a stranger's code.

**The split is deliberate, and `dog_index.rs`'s module header already says
why.** That index is built from pull requests by strangers, and every string
in it is printed to a terminal, so the whole module is a security boundary:
it sanitises escapes because a `description` carrying `\u{1b}[2J` clears the
operator's screen, and because shep emits colour of its own, a well-placed
escape can imitate shep's own output with the reader unable to tell an
entry's bytes from shep's. An index whose strings cannot be trusted to
**print** is not one that should be trusted to **execute**. `shep install`
would turn "somebody added a row to a table" into "somebody chose what builds
and runs on this machine", which is a far larger step than the sanitiser
guards.

**The door is left open on purpose.** `DogSourceKind` is a tagged enum rather
than a freeform string exactly so this can be added later without asking every
past contributor to rewrite their entry: "how do I install this" and "what
artifact would shep fetch" are two questions that look like one field. Adding
the `cargo` kind on 2026-08-26 cost almost nothing, which is that decision
paying out.

**The obvious next move is a scanner, and it is the wrong one.** The tooling
is real and worth knowing. `cargo-audit` and `cargo-deny` check a dependency
tree against published advisories. `cargo-vet` and `cargo-crev` record that a
*human* reviewed a specific version, and `cargo-vet` can import Mozilla's and
Google's audit sets rather than starting from nothing. OpenSSF Scorecard
grades repository practice. Socket does behavioural analysis and reached
general availability for Rust in 2026, though its deep behavioural tier
remains JavaScript and Python only, Cargo sits in a shallower tier, and git
and path dependencies are unsupported.

**None of that helps here, because a dog's legitimate job is the suspicious
behaviour.** `shep-log-rotate` renames files, deletes files, writes archives,
and holds a socket to a privileged daemon. A hostile dog does the same things.
The capability profile of a benign dog and a malicious one is identical by
design. Behavioural analysis earns its keep when a capability contradicts a
stated purpose, a JSON parser that opens a network socket being the classic
case. A process-supervisor plugin that spawns processes and writes files
presents no contradiction to detect, so the scanner flags every real dog and
clears a hostile one that stays inside the same envelope.

**A verdict would be worse than no command at all.** `shep audit <dog>`
printing "no issues found" manufactures confidence about a question that is
not decidable in general, and people would install things they would
otherwise think twice about. A green check on an unanswerable question is a
liability, not a feature.

**What would have to be true first, in rough order of signal per unit of
work:**

1. **Provenance.** Does the crate come from the repository its index entry
   claims? crates.io supports trusted publishing and attestations, so this is
   a fact rather than a judgement, and it closes the likeliest real attack: a
   typosquat or a hijacked publish that has nothing to do with the repository
   anyone reviewed.
2. **A recorded human audit.** `cargo-vet` exists for precisely this and its
   importable audit sets mean shep would not be starting a web of trust from
   zero.
3. **Confinement rather than inspection.** The one worth most. shep already
   resolves `user`/`group` for sheep; running dogs with reduced privilege
   bounds the damage whatever the code turns out to do. Analysis is advisory,
   confinement is enforcement, and an hour spent on a scanner is an hour not
   spent on the control that actually holds.
4. **Pinned versions, and reading the diff on update.** Most real supply-chain
   attacks arrive as an update to something already trusted, not as a first
   install.

**If a command is ever built, it reports facts and never a verdict.**
Something nearer "14 dependencies, 3 with build scripts, published from the
repository it claims, no human audits on record, 2 known advisories" than a
tick or a cross, and it does not gate `shep install`, because gating on an
undecidable question is how the false confidence gets manufactured in the
first place.

Tracked as https://github.com/shep-pm/shep/issues/357.


### `cmd` on Windows cannot carry a quoted argument, and shep cannot fix it

A Flockfile app whose `script` is `cmd` and whose `args` contain double
quotes will fail to start on Windows. `std::process::Command`'s escaping and
`cmd`'s own parsing disagree, so the quotes arrive mangled and the command
never runs as written.

The workaround is to put the commands in a `.cmd` file and point `script` at
that. A `.cmd` file's contents go through no argument escaping at all.

Not shep's bug and not fixable in shep: the only lever is
`CommandExt::raw_arg`, which would mean shep guessing at quoting on the app
author's behalf. Recorded here because an operator who hits it has no way to
tell it apart from a shep bug.

The full postmortem, including the wrong diagnosis it took to get here, is in
[deferred-history.md](deferred-history.md).

### ~~The handover blob's compatibility tests never load an old blob~~ -- done, 2026-10-14

Raised in review on #84, 2026-08-31, and agreed rather than argued away.

Ten cases are named `a_blob_written_before_<field>_was_carried_still_loads`
-- stdin, the channel, `pending_delete`, the manual marker, a swap in flight,
`ready_failed`, the restart deadline, the dog marker, the parked config
(`pending` and `pending_reidentifies`), and the disabled mark. Every one of
them builds a blob from today's `Handover`, serialises it, removes one key,
and loads the result.

That proves the thing each field's carry actually needs: an absent key loads
as `None` rather than refusing, so a successor boots against a predecessor
that never wrote the field. It is not nothing.

**What it cannot prove is that a blob written by an older binary still
parses**, because it round-trips through today's serialiser. Rename a field
and nine of these follow the rename and keep passing, while a real blob on
disk from v0.1.18 fails. IR-35 asks for exactly the missing half:
"committed byte-fixtures from the previous protocol version that must still
deserialize".

**The wrinkle the entry was waiting on, settled by the maintainer:** the rule
says fixtures from the PREVIOUS protocol version, and the blob's `VERSION`
has never moved from 1, so there is no version to key a fixture to. The policy
is **one fixture per distinct shape that shipped, named for the first
`shep-daemon-v*` tag that wrote it**. A shape is the set of keys a serialised
blob holds -- at the blob, at `sheep[]`, at `sheep[].fds` and at `reloads`.

What that turned out to be: every tag from `shep-daemon-v0.1.18` (the first
with a handover) to `shep-daemon-v0.14.0`, 61 tags, each dumped from its own
code rather than transcribed. Twelve distinct key sets, not the ten the issue
predicted. Seven are `shep-daemon`'s own structs adding a key, which is where
the ten cases come from -- several releases added more than one at a time, so
ten field additions land on seven shapes. The other five are the `AppConfig`
a sheep carries: `depends_on` (v0.5.0), `environment` (v0.6.2), `level_rules`
and the dropping of `increment_var` (v0.8.0), `dogs` (v0.11.0), and
`log_timestamps` (v0.12.0). Those are blob shapes too: a rename there breaks
a blob on disk exactly as a rename in `CarriedSheep` does.

Built as `crates/shep-daemon/fixtures/handover/`, twenty-four files: one per
shape plus a `-populated` sibling for each, every one the bytes that release's
own serialiser wrote, with a README naming the tag, the command and whether
that release crossed the blob as `$SHEP_HOME/run/handover.json` or as the state
of a `shep-handover` envelope (from v0.13.0). `crates/shep-daemon/src/handover/blob/tests/old_blobs.rs`
loads each plain shape through `Handover::load_value`, asserts the whole key
set that release wrote at the blob, at `sheep[]`, at `sheep[].fds` and at
`sheep[].app` (`old_blob_shape.rs` holds those sets as data), the values that
shape wrote, and that every key it predates reads as `None` through the
accessor; loads the oldest through `Handover::read`, which is what the
legacy arm of the boot uses; and guards that the fixture directory and the lists
of fixtures in those files are the same set. Nothing blesses or regenerates a
fixture: that is the bug these exist to catch.

Review on the pull request found what a key set on its own cannot do. A shape
with nothing in it passes a rename: `manual` renamed on the wire failed no
plain fixture and no `compat` case, and `kill_timeout` renamed on `AppConfig`
did the same, because an absent key and a `null` read alike and
`AppConfig` carries `#[serde(default)]`. The `-populated` siblings are the
answer, dumped the same way from each release's own code with every optional
key that release had a place for filled in, round-tripped through that
release's `Handover` before printing so the release's own deserialiser had to
accept the blob its own serialiser then wrote. Those two renames then failed
ten of the populated fixtures and twelve of them.

Review found the same gap twice more on the plain half. From v0.1.22 on a
per-fixture test pinned only the keys of `sheep[0]`: dropping `reloads` from the
blob or `channel` from a sheep's descriptors passed, and none of the five
`AppConfig`-derived shapes pinned the keys of `sheep[0].app`, so dropping
`depends_on`, `environment` or `level_rules` passed. Every fixture test now
compares all four levels against the release's key set, plain and populated
alike. Dropping `reloads` from v0.1.22, `channel` from its descriptors, `stdin`
from v0.13.0's, `depends_on` from v0.5.0's config, `environment` from v0.8.0's,
`level_rules` from v0.11.0's, `log_timestamps` from v0.12.0's, renaming `manual`
in v0.1.22's bytes, adding a key no release wrote to v0.1.30, and dropping
`kill_timeout` from a populated fixture each fail the test named for that file.

The mutation check this entry asks for, run against `pending_delete` renamed
to `pending_removed`: it failed each of the ten fixtures that carry the key,
`compat`'s own `a_blob_written_before_pending_delete_was_carried_still_loads`
(it removes the key by name and asserts the removal found one), and the `Debug`
redaction case. The nine other `a_blob_written_before_*` cases passed unchanged,
which is the nine this entry predicted, and the one that failed is the one that
names the field by hand.

Tracked as https://github.com/shep-pm/shep/issues/358.

### ~~The bark dog still restarts once per reload~~ -- done, 2026-09-13

Bark now re-subscribes across a handover instead of exiting, and both dogs
exit when their shepherd is genuinely gone. Measured on one machine with
the same `dogs.toml` either way: the base binary took bark from `restarts
0` to `restarts 3` across three `shep daemon reload` runs, moving its pid
each time, while this change held bark at pid 26504 and `restarts 0` across
ten. A `SIGKILL` to the shepherd then ended both dogs after 5.4 seconds
rather than instantly or never.

What unblocked it was the ruling this entry was waiting on, and it applies
to every dog rather than to bark: **a dog that loses its shepherd waits a
bounded time for a successor, then exits.** A reload is not losing your
shepherd, so the wait is what separates the two. The budget is
`DOG_SILENCE_BUDGET`, and `docs/dogs.md` carries the contract for
third-party dogs. The reasoning is in `docs/decisions.md` under "A dog
whose shepherd is gone exits".

The bound went into the dogs rather than into `ReconnectingClient`: the
supervisor dies with the process, so once a dog exits there is nothing left
reconnecting, and what lingers is the dog. shep-client gained only the two
waits that let a dog impose it.

### ~~`EXTEND_TIMEOUT_USEC` would remove an operator's readiness homework~~ -- done, 2026-10-25

systemd's `sd_notify` protocol accepts `EXTEND_TIMEOUT_USEC=<n>`, which lets a
`Type=notify` service push `TimeoutStartSec` out as it reports progress,
rather than needing the whole boot to fit inside one fixed budget set in
advance. The daemon already has a notify module and already emits one info
line per boot stage, so the hook this would ride on already exists.

**Why this is the systemd-native answer, not a nice-to-have.** A staged boot
with real dependencies takes an unbounded but progressing amount of time --
more stages, more `depends_on` chains, more apps waiting out their own
`listen_timeout` -- and the current answer is documentation telling an
operator to size `TimeoutStartSec` generously. `EXTEND_TIMEOUT_USEC` lets the
unit extend its own deadline as each stage lands, which removes that sizing
guess entirely rather than asking the operator to guess better. Left for its
own task because it is a new notify-protocol call, not a fix to anything
built on this branch.

Tracked as https://github.com/shep-pm/shep/issues/359.

Built: the boot restore sends `EXTEND_TIMEOUT_USEC=<n>` before each stage, with
`n` that stage's bound (its longest `listen_timeout` plus `STAGE_SLACK`), and
again once the spawns return for a stage something waits on, since each
message sets a fresh deadline from its receipt and the wait starts after the
spawns. One more of `STAGE_SLACK` follows the restore, before the setup ahead
of `READY=1`. `shep start` and `Muster` send nothing. A single stage that overruns its own
bound still lets systemd kill the shepherd.

### ~~A promoted dog cannot handshake during the restore~~ -- done, 2026-10-23

`[daemon] boot_first_dogs` spawns a dog ahead of the muster restore, but the
control socket was only served after it: `RpcServer::serve` runs inside
`RunningDaemon::run`. A promoted dog's first connect sat in the backlog,
timed out at `HANDSHAKE_TIMEOUT`, exited `daemon_unreachable` and was
restarted, a restart it had not earned (#360).

Each supervised dog is now spawned holding a connection of its own, a
socketpair end on fd 4 named in `SHEP_DOG_FD`, and the shepherd serves it
from spawn through `serve_dog_links`, before and apart from the listener.
`DogRuntime::start` uses it when the variable is set and falls back to the
socket when it is not, or when the link is lost. The decision is "A dog gets
a connection of its own, served from its spawn" in `docs/decisions.md`.

A handover carries the link: its descriptor and the dog's subscription travel
in the blob, and the successor serves it without a `Hello` and re-arms the
subscription after a `daemon.dropped` notice (#1088, "A handover carries each
dog's connection" in `docs/decisions.md`).

What it does not cover: a dog started by hand has no link and still waits for
the listener. A frame in flight at the exec, and events published between the
exec and the re-arm, are lost.

## Ideas, recorded but not designed

Not debt, not deferred spec surface, and not promised to anybody. Things worth
keeping because the reasoning behind them was expensive to arrive at and cheap
to lose.

### Per-sheep build and update scripts

The maintainer's, 2026-08-26, written down before it got lost:

> I would really love to add the ability for users to add build scripts to
> shep for each of their sheep. `shep build koji`,
> `shep build reactmap --restart`. Maybe even update scripts or git related
> scripts? `shep update koji` => git pull from cwd? It obviously doesn't make
> sense to mirror all of git's capabilities but just some basics of updating
> and building could go a loooong way in my experience. Do you know how many
> janky updating/deploying scripts I've written over the years?

**The last sentence is the evidence, and it is the strongest part.** "I keep
rewriting this by hand" beats any amount of design argument about whether a
feature is wanted. Anyone running a flock has written the pull-build-restart
script, and written it slightly differently each time.

**This revisits a documented non-goal, and the narrowing is the argument.**
[shep-v1.md](shep-v1.md) §1 cuts "deployment tooling" from v1, which is
`pm2 deploy`: host lists, revision directories, `ref` and `repo` per
environment, remote execution over SSH. What is described here is much
smaller. One machine, one checkout, the cwd a sheep already has, and two verbs
that stop well short of a deploy system. Reconsidering the cut is not the same
as reversing it, and a future design should say plainly which of pm2's deploy
surface it is still refusing.

**The sequencing is the actual feature, not the individual verbs.** `shep
build koji` on its own is a task runner with extra steps, and `just`, `make`
and npm scripts already exist. What replaces a janky script is pull, then
build, then restart, with the guarantee that a failed build does not restart
anything. The maintainer's own `--restart` flag already gestures at this. The
failure semantics are the whole value, so a design that ships the verbs
without deciding them has shipped the thin half.

Questions a design would have to answer, roughly in the order they bite:

- **Where do the scripts live.** The Flockfile is the natural home: per-app,
  committed beside the code, and it already carries `cwd`. `shep.toml` is
  daemon-scoped and would be the wrong shape for a per-sheep command.
- **Who executes them, the CLI or the shepherd.** This is the crux and it is
  the same trust question the `shep install` entry above works through. A
  shepherd that runs these is a long-lived privileged daemon executing
  arbitrary shell out of a config file. A CLI that runs them uses the
  operator's own shell and privileges, which is far easier to reason about,
  and costs nothing that matters while a shepherd supervising a process on another
  host remains a non-goal.
- **Which directory.** Mostly already decided: Phase 17 resolved a
  Flockfile's relative `script` against the app's own directory rather than
  the daemon's cwd, and an app's `cwd` defaults to the Flockfile's directory.
  `shep build` should use the app's resolved `cwd` and inherit that ruling
  rather than inventing a second one.
- **What `shep update` actually promises.** `git pull` is where this stops
  being simple. A dirty worktree, a detached HEAD, the wrong branch,
  submodules, and authentication that depends on whose SSH keys are in scope
  are all ordinary states, not edge cases. A pull that fails halfway leaves a
  half-updated checkout that the operator then has to reason about anyway,
  which is the exact position the janky scripts leave them in. Refusing
  loudly on anything but a clean fast-forward is probably the honest v1 of
  this.
- **Whether there is a rollback.** The frightening part of a hand-rolled
  deploy script is usually that there is not one. shep should either offer
  something or say clearly that it does not, because an operator who assumes
  wrong finds out at the worst moment.

**If it turns out to be a thin wrapper, that is a reason not to build it.**
The value has to come from the integration: knowing the cwd, the environment,
the restart, and the ordering across a flock. A design that cannot point at
something `just` plus three lines of shell does not already do should stop
there.

Tracked as https://github.com/shep-pm/shep/issues/361.


### Shepherd-channel libraries for the languages apps are written in

The maintainer's, 2026-08-30, after asking whether an app could speak fd 3
using something that already exists. Wanted, in her own list: **node, go,
rust, python**.

**When this was written, an app could, badly.** `shep_core::protocol::channel`
exported `ChildMessage`, `ShepherdMessage` and `CHANNEL_VERSION`, all
serde-derived, and shep-core is published. But it is a daemon's core rather
than a client: an app that wanted two enums also got toml, serde-saphyr,
json5, regex, tokio, croner, chrono, chrono-tz, globset, tempfile and nix.
Hand-rolling against [shepherd-channel.md](../shepherd-channel.md) is about
forty lines and was the better trade, which is an odd thing to have to say
about one's own published crate.

That is what `shep-channel` fixed, and `shep_core::protocol::channel` is a
deprecated shim onto it now. Go has a library too. JavaScript and Python
are the two languages with nothing but the examples.

**The examples half of this is built.** `examples/` now holds the same
channel app in all four languages: `chatty.rs` over the `shep-channel`
crate, and hand-rolled Go, Node and Python under `examples/polyglot/`. Each
carries both platform arms. What this entry described, an `examples/`
directory where nothing spoke fd 3 and a contract demonstrated only in
prose, is no longer the state of the repository.

**Three things a hand-roll gets wrong**, each named in the module doc of
`crates/shep-channel/src/wire.rs`, which is where that wire moved to: an
app must reply to a `ShepherdMessage::Action` even when it does not
recognise the name; it should echo the `id` so the reply is matched to its
exact trigger, and the name-and-order fallback costs something when it does
not; and there is a `params` quoting gap. Those are what a library
encodes once and prose asks every author to get right separately. Windows is
a fourth, since fd 3 is a named pipe there and every client needs two arms.

**Sequencing, offered as a recommendation rather than a decision.** Working
examples first, because the repository already has an app in every one of
the four languages: Rust under `examples/src/bin/`, and Go, Node and Python
under `examples/polyglot/`. Teaching those four to speak fd 3 is a small
diff against files that already exist, and it gives every community app
something to copy. A library second, for whichever language earns one. The
four chosen are also the ones the surrounding ecosystem is written in, so
the examples serve the whole audience on their own.

Both halves of that have moved. The examples exist, and two of the four
languages have a library: `shep-channel` for Rust and
`github.com/shep-pm/shep-go/channel` for Go. What is left of this entry is
JavaScript and Python, where the examples are what an author copies today.

Tracked as https://github.com/shep-pm/shep/issues/362 for JavaScript and https://github.com/shep-pm/shep/issues/363 for Python.

## Closed entries live next door

Two moves, not one. The bulk of what this file carried that is now FIXED,
STALE, resolved or rejected, plus the record of what shipped instead of being
deferred, moved to [deferred-history.md](deferred-history.md) on 2026-08-29,
the day of the split, when a reader had to get past all of it to reach the
entries under "Known debt" that are actually still open. The reload-readiness
entry followed on 2026-10-15, once the serial reload had made it a closed
record rather than open debt.

The two files now run 1412 lines there against 1056 here. Both counts were
stated once and then drifted, which is the failure this file warns about
elsewhere; they are counted, not remembered, and this pair was counted after
the 2026-10-15 move.

This file answers "what is not built". That one answers "what was not built,
and what happened to it".
