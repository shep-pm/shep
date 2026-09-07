# shep — phase history

Moved out of `CLAUDE.md` on 2026-09-07. That file is read at the start of
every session and injected into every subagent, so its cost is paid once per
agent rather than once per session. This narrative is worth keeping and is
not worth re-reading on every dispatch, so it lives here and `CLAUDE.md`
points at it.

Nothing here was rewritten in the move. Read it when you need to know why
something is the way it is; `CLAUDE.md` carries the rules you need in order
to work.

## What shipped, phase by phase

Phases 1–10 merged: shep-core, the daemon supervision engine, log plane, the
CLI, watch/cron/memory-limit restarts, overlapping reload, custom
actions over the shepherd channel (now with a correlation id), the pm2
cutover, the dogs subsystem with working metrics and bark dogs, and an
audit-debt phase.

**That reload is NOT "SO_REUSEPORT reload", which this line said until
2026-08-28.** shep never binds an app's listening socket and never sets
`SO_REUSEPORT` on one. The only endpoint it binds is its own control
endpoint, a unix socket at `$SHEP_HOME/run/shep.sock` and a named pipe on
Windows, which is a different thing entirely. The unix one does fail loudly
when its path exceeds the platform limit. Whether a reload's overlap is
zero-downtime depends on the app having set `SO_REUSEPORT` on its own
listener; without it the second instance takes `EADDRINUSE`.

**The overlap stopped being unconditional later the same day**, and this file
carried the old claim for a few hours, which is worse than it sounds because
this is the file every session reads first. `reuse_port` is no longer refused
at parse time: it is the field that decides which of two reloads an app gets.
An app with a `readiness_probe` and no `reuse_port` is reloaded SERIALLY
(DrainOld, ReapOld, SpawnNew, AwaitReady), because a probe asks an address and
an address cannot say which of two overlapping instances answered it. Anything
else still overlaps (SpawnNew, AwaitReady, DrainOld, ReapOld): no probe,
`wait_ready`, or `reuse_port = true`. See `ReloadMode` in `supervisor.rs`, and
`docs/specs/deferred.md` for the three residuals that fix does not cover.
A reload also PROMOTES config a Flockfile load parked (see the config-override
paragraph below), which is new as of 2026-09-03 and is the one thing that
makes `shep reload <sheep>` about config at all. Separately, `shep daemon
reload` now validates `shep.toml` BEFORE it touches the predecessor: the
handover arm execs a successor that re-reads that file, and a value that
fails to load there used to exit the successor with the predecessor already
gone, leaving the flock running with nothing supervising it.
Phase 11 merged too: the six remaining daemon-surface
verbs — `shep stock` (alias `scale`), `shep signal`, `shep whisper` (alias
`sendline`), the KV store's `set`/`get`/`unset`, lambs in `describe`, and
the `channel.*` bus topic. Phase 12a merged: `shep lookout`'s shell and its
flock table pane — dependency, terminal lifecycle, palette, event loop, link
supervision, and a table that subscribes to the bus and polls every two
seconds to repair drift. Phase 12b merged too: the table grows a selected
row, and the three remaining panes go up around it — a host-usage strip, a
sheep detail pane, and a bleats feed. The feed reads the selected sheep's
log files from disk on every refresh rather than subscribing to `log.*`,
deliberately — a busy flock costs one bounded read per pane instead of
making the dashboard the highest-volume subscriber on the bus. Rendered
frames for both phases are in `docs/lookout/frames.txt`. Phase 13 merged:
`shep whistle`, the MCP server over stdio (`rmcp`) — nine tools, five
read-only and always present, four
that mutate and present only when `[whistle] allow_control = true` in
`shep.toml`; `start_sheep` narrowed to already-registered sheep; every
daemon refusal a control tool can meet reaches the model as an in-band tool
result, not a protocol error. `docs/whistle/README.md` and the generated
`docs/whistle/tools.md` are the operator contract. Phase 14 merged: config
and packaging — `.js` Flockfiles behind `shep start --flockfile` (never by
discovery, never by extension alone: `shep start server.js` still starts
`server.js`), a schemars-exported Flockfile JSON Schema
(`crates/shep-core/assets/flockfile.schema.json`, generated from the parser's
own document type, printed by the hidden `shep schema`), a `file < env <
flags` daemon-config layer (`shep daemon --log-json/--log-level/--socket/
--max-cron-sleep`), and openrc plus FreeBSD/OpenBSD `rc.d` renderers for
`shep startup`/`unstartup` — the last two rendered and pinned by
exact-string tests only, never executed on their own operating systems.
Phase 15 merged: the last three v1 verbs — a hand-rolled `shep serve` (no
axum, no tower-http; dotfiles, directory listing, and every in-docroot
symlink all refused by default), `shep runtime` (foreground, no-daemon, PID-1
via a separate init process that reaps orphans and forwards signals), and
`shep dev` (isolated `$SHEP_DEV_HOME`, forced watch, auto-exit) — plus the
`shep` library extraction the two container-entrypoint `[[bin]]` aliases
needed underneath them. Phase 16 merged too: `shep lookout`'s last three
pieces — a name filter that narrows the flock table in place, lambs in the
sheep detail pane (fetched separately with `Request::Describe`, never on the
two-second poll), and the three action keys (`x` stop, `R` restart, `L`
reload) behind the `--allow-control` gate, each arming a confirm rather than
acting on the keypress that pressed it. No wire change.

**After Phase 16** the CLI grew again, so "the v1.0 surface is closed" no
longer holds and this file will not claim it. `feat/pretty-cli` merged the
box-drawn table renderer with adaptive column dropping, colour and a sheep
face in the STATUS column, a `full`/`plain`/`bare` style dial resolved at one
seam, `shep style` persisting to `shep.toml`, and ASCII sheep in three
moments. Then 2026-08-19 added `ProcessInfo::last_exit` and an EXIT column
(a wire change), `shep bleats`' backlog and `--lines`, an opt-in
`[interpreters]` mapping with `--interpreter`, `~/` expansion in every
Flockfile path, a Flockfile app's `cwd` defaulting to its own directory, and
`reuse_port` refused rather than silently ignored (which it no longer is; see
the reload paragraph above). `shep init` shipped: it is in the CLI's `VERBS` array, has its own
module at `crates/shep-cli/src/commands/init.rs`, and is documented on the
Flockfile page.

**Phase 3b merged on 2026-08-31, and it is the one to read if a dog looks
healthy and is not.** A dog that has never completed a handshake used to
report `online` in `shep flock` and in `shep lookout`, with zero restarts,
while retrying a handshake that could never succeed. `ProcessInfo` gained
`handshook: Option<bool>` (additive, so neither `PROTOCOL_VERSION` nor
`SCHEMA_VERSION` moved) and such a dog now reads `silent`. The daemon grew
`DOG_SILENCE_BUDGET`, five seconds, so G8's one-restart ladder reaches a dog
that cannot name itself: before that, the ladder was keyed on
`Hello::dog_name`, which a client on an older protocol cannot send, so the
dogs most likely to need the ladder were the ones structurally unable to
reach it. `shep daemon reload`'s unsettled-dog report now points at `shep
bleats <dog>`, and the version-skew refusal labels its remedy.

**The repository moved to the `shep-pm` org on 2026-08-31**, along with both
dogs and the four `shep-deploy` testbeds, and the docs site moved to
`shep-pm.com`. Two things that cost real time and will again: a GitHub App
installation does NOT follow a transfer, so CodeRabbit stopped reviewing
until it was installed on the org; and the Pages custom domain DOES follow,
carrying the old value, so the site was unreachable while every setting
looked populated and every workflow ran green. A `CNAME` file under
`web/public` does not fix that second one, since Pages ignores it for an
Actions-based deploy. See `.github/workflows/pages.yml`'s header.

**Phase 4 merged on 2026-09-01.** A dog answers `--version` with the
protocol it was compiled against, `shep adopt` refuses a mismatch, and
`shep restart <dog>` warns before bringing a dog back on a binary that
cannot connect. The contract is published in `docs/dogs.md`; answering is
optional, and a dog that does not answer is adopted with its protocol
unknown rather than refused, which is every dog written before it.

One thing it deliberately does not do, argued at its call site: a
DAEMON-initiated restart gets no warning, since the check is CLI-side, so a
crash or an autorestart respawn still walks into G12 row 5 unannounced.

**A probe's descendants are contained on unix now.** This paragraph used to
say `Child::kill` does not reach them and that closing it needed a process
group rather than a patch, which was right about the mechanism. `ask` in
`crates/shep-cli/src/commands/dogs.rs` spawns with `process_group(0)` and
`kill_probe_tree` sweeps `-pid`, the same shape `probes/os.rs` already used
for the exec prober. Two holes stay open and are documented rather than
closed. A descendant that calls `setsid` leaves the group, as `kill.rs`
records for a sheep. And Windows has no process group, so the probe there
still kills only the binary it spawned; containment would mean reaching the
`pub(crate)` job object in `sys_windows.rs`.

**There are three doors into the override store, not one.** A Flockfile
load through `Request::ApplyConfig`, described below, is the one that
existed first and it is the one the rest of this paragraph is about. The
other two arrived with the lookout config panes: `Request::SetSheepEnv`
sets or removes one env key, and `Request::SetSheepField` sets one
non-env field. Both write an operator override directly.

The distinction is not cosmetic and it cost a review round to find. A
Flockfile load says the TEMPLATE declares this key, so the daemon spends
any operator override for it, correctly, because a key put back to the
template is not one an operator is still holding a value for. A pane says
the OPERATOR sets this key, so the override has to stay: the sheep really
does still differ from its file. A pane borrowing `ApplyConfig` with
`--reset=file` and a one-key `declared` set therefore wrote the right value
and erased the record of it, so the `*` marker built to show operator
overrides never appeared for the pane's own writes. `SetSheepField` exists
because of that.

**Config overrides merged on 2026-09-03, and it changes what a Flockfile
IS.** A Flockfile is a project template committed to the app's repository,
never written by shep, and what an operator tunes afterwards lives in a
shep-owned store at `$SHEP_HOME/overrides.json` (locked and `0600`, like the
KV store). `shep start <Flockfile>` now sends `Request::ApplyConfig` and
merges the file into the sheep of the same name: additive by default, so it
appends keys nobody has established and overwrites nothing, because a
Flockfile arrives through a pull request. Widening it takes
`--reset=<mode>`, one flag with four required values rather than two
booleans: `file` puts back what the template declares and leaves `env` and
every undeclared key alone; `policy` does the same but for every key,
declared or not; `env` puts back only `env`; `all` does both. `--reset`
with no value is a usage error naming the four. Every mode is refused when
the target names a sheep, since a name reads no file, and on a bare script
path too, for the same reason. A load with NO FLAG never prunes and never
kills, and the merge itself registers nothing, though the `shep start`
carrying it still registers and starts an app the flock does not have, by
its own fresh path --
a field the running child holds parks as pending and `shep reload`/`shep
restart` promote it, re-resolving identity only when `user` or `group` moved.
**A reset can kill, and that is deliberate.** `instances` is Structural, held
out of a plain load entirely and routed through `handle_scale` under any
mode but `env`, whose `Ordering::Less` arm deletes the instances above the
new count on the same path `shep delete` takes
(`a_plain_load_never_scales_and_a_reset_does` pins it). `file` scales too,
when the template declares `instances`: it takes the count on the same
terms as every other key it declares. The sharp edge is the undeclared
case. A Flockfile that never mentions `instances` still means 1 under
`policy` or `all`, because that is the compiled default the reset falls
back to, so an app stocked to four goes to one against a file that has no
opinion about the count. `file` is the mode that survives that specific
case, because `merge_declared` never puts an undeclared `instances` in
scope, so there is nothing to put back. The overrides page carries the
warning. A `CFG`
column in `shep flock` and in `shep lookout` marks a sheep with pending
(`!N`) or overridden (`*N`) fields, and `shep describe` lists the names. A
per-app refusal exits non-zero. The four-way field classification lives in
`crates/shep-core/src/config/apply.rs` and is measured against read sites,
not guessed from field names: `kill_signal` is NextSpawn rather than Live,
and `shutdown_with_message` needs a respawn. `web/src/pages/docs/overrides.astro`
is the operator-facing account.

**`shep add` is decision 7 of that same spec, and it is the verb that makes
the template model usable.** It takes the targets `shep start` takes, runs the
same load, and spawns nothing: the app lands registered and `Stopped`, its
declared keys established, and `shep start <name>` brings it up. Without it
the first thing an operator does with a template shipping `env = { DB_HOST =
"", DB_PASSWORD = "" }` is start it, which spawns against an empty database
URL, crash-loops through the restart budget, and has to be stopped before it
can be configured. `start` and `add` are ONE code path (`lifecycle::load`,
carrying a `Load`), because a document that registered differently depending
on which verb read it is one nobody could reason about. Four places consult
it: which request a fresh app goes out as, whether an app the flock already
has is resumed after the merge, what a name target that resolves to a
registered sheep does, and the notice code. `Request::Add` /
`Response::Added` are additive and did not move `PROTOCOL_VERSION` on
their own; it later moved to 3 for an unrelated reason, recorded below, and
the paragraph below applies to `shep add` too. **The fill-in half of
"register, fill in, start" shipped with the config panes**: `shep lookout`'s
sheep pane sets or removes one `env` key at a time through
`Request::SetSheepEnv`, and one non-`env` field through
`Request::SetSheepField`, both behind `--allow-control`. Env stays
write-only: the pane sets a value and no request ever sends one back, so an
operator who forgets one reads it from wherever they got it, not from shep.
Before that slice an established key moved only through the file plus
`--reset=env` (or `--reset=all`, which also drops the override record).

**Restart the shepherd after upgrading to it.** `PROTOCOL_VERSION` did NOT
move for `ApplyConfig` itself or for `Add` (both variants are additive, and
six precedents in shep-core's changelog agree), so an older shepherd cannot
decode either one. **This paragraph said the operator meets a dead client,
full stop, and that is wrong: it skips the skew guard, which fires first in
the common case.** `refuse_version_skew` compares the shepherd's reported
crate version against the client's own and refuses every verb but the three
in `RECOVERY_VERBS` (`kill`, `ping`, `daemon reload`), at the connect site,
before any request is sent. So the two cases are:

- **Versions differ**, which is every release upgrade through cargo or brew:
  `error[version_skew]`, naming `shep daemon reload` as the remedy. No
  request is sent and nothing is ambiguous.
- **Versions match**, which is a client built from a commit that added the
  variant against a shepherd built from an earlier commit of the same
  version, so every development build and every branch: the guard passes,
  the request goes out, and the shepherd ends the connection on an envelope
  it cannot decode. THAT is the dead client, and it is a working-tree
  hazard rather than an operator one.

`shep daemon reload` is the fix in both, and `getting-started.astro` says so
where an operator reads. `every_exempt_verb_is_one_of_the_documented_recovery_verbs`
pins `add` at `Enforce`, since it reaches that through the `_` arm rather
than by being named.

**`PROTOCOL_VERSION` and `MIN_SUPPORTED` moved to 8 on 2026-09-07, for
`AppConfig::environment`.** Not for `Request::PutSecrets` and
`Response::SecretsPut`, which arrived on the same branch and moved nothing.
The secrets branch had bumped 4 to 5 for exactly those two, on the grounds
the two paragraphs below give, and #173 took that reason away: `Request`
grows a `#[serde(other)] Unrecognized` variant, so a daemon that has never
heard of `put_secrets` decodes it, answers `unsupported` naming its own
protocol, and keeps serving the connection. An addition is free now.

What is not free is a new field on a `deny_unknown_fields` struct. An older
peer refuses the whole payload rather than ignoring a key it does not know,
which is why `depends_on` moved the number to 5 and `environment` moved it
to 8. `MIN_SUPPORTED` goes with it, since the handshake compares against
the floor. 6 and 7 came from a third cause again, a retype:
`Response::Reloading` and `Response::Restarted` became struct variants, so
they serialize as an object where an older peer reads an array.

Three causes, and only the first is exempt: an addition is free, a field on
a `deny_unknown_fields` struct bumps, a retype bumps. The two paragraphs
below predate the tolerant decode and record the rule it replaced.

**`PROTOCOL_VERSION` moved to 4 on 2026-09-04.** It went to 3 first, for
`ApplyConfig`'s payload rename described below, and then to 4 for the four
requests the lookout config panes needed. The second move is argued in
`docs/decisions.md` and is the one that broke the additive rule on purpose:
those four variants are additive, and the rule said not to bump, and skipping
the bump is what made `ApplyConfig` fail on a dead client rather than a named
refusal. The paragraph below is about the 3.

**`PROTOCOL_VERSION` moved to 3 on 2026-09-04, for `ApplyConfig`'s payload
rather than for `ApplyConfig` itself.** The two-case analysis above still
holds for `Add` and for `ApplyConfig`'s own addition. It stopped holding for
`ResetDepth::Settings`, renamed to `ResetDepth::Policy` (with `File`/`Env`
added) in the same commit: a rename changes the wire spelling of an
operation (`--reset`) that already ships, so the "versions match, same
commit lineage" case above is no longer the only hazard. A daemon at
protocol 2 that has simply not restarted since the upgrade now fails to
decode `"policy"` for what it already understood as `"settings"`, which is a
regression of live functionality rather than an unreachable new one, so the
bump closes that gap with a named `protocol_mismatch` refusal, exit 6,
instead of leaving it as an accepted cost. Not `version_skew`, exit 12,
which this said until 2026-09-04: `refuse_version_skew` runs only after
`connect_or_spawn` returns `Ok`, and a protocol refusal fails the handshake,
so it returns `Err` and that check is never reached. `docs/decisions.md`'s entry on this reverses the
"`PROTOCOL_VERSION` stayed 2" ruling that predates it.

**Verb count: 41 generated, 42 listed, and the difference is `help`.**
`./web/scripts/generate-cli-reference.sh` prints its own number every time it
runs, and its `VERBS` array holds 41 because it does not generate a page for
`help`. `shep --help`'s grouped listing shows 42 because it does. Both are
right about different questions, so neither is a bug to fix; check which one is
being asked before changing either. README.md deliberately quotes the grouping
without a count, so there is no third number to keep in step.

What's built vs. deferred to v1.1+: [docs/specs/deferred.md](specs/deferred.md).

**Windows is built and runs.** This line said "0%, not partial — every verb
prints 'not yet supported' and exits" for eighteen phases, and that is no
longer true of anything. A Windows host became available, and
[windows-estimate.md](specs/windows-estimate.md)'s own first
recommendation — dispatch the CI leg before scoping anything — was run: the
tree was already compile-green on native MSVC. Tier A is now implemented and
verified against a live flock on real Windows.

What that means for anyone editing this workspace:

- **`cfg(unix)` is no longer a free choice.** `shep-client`, `shep-daemon`'s
  `boot`/`server`/`tokio_runner`, and every `shep-cli` module tree are
  portable now. The OS transport lives in ONE place,
  `shep_core::transport` — a unix socket or a Windows named pipe — and
  everything above it (codec, handshake, actor, RPC dispatch) carries no
  platform gate at all. Adding one back is a design decision, not a shrug.
- **A per-sheep job object replaces the process group.** `sys_windows.rs` is
  the crate's only unsafe on that platform, mirroring `sys.rs`'s rule. It is
  stronger than the unix design: `kill.rs` documents an escaped-`setsid`
  hole that a job simply does not have.
- **Three refusals are permanent and deliberate**, each argued at its own
  call site: no graceful signal outside the shepherd channel, no
  `shep startup` (that is Tier B — an SCM service), and no `user`/`group`.
- **The local gate does not run Windows tests.** `cargo test` on a Mac never
  compiles a `cfg(windows)` item, and the `windows-gnu` cross-check is
  `cargo check`, which executes nothing. `.github/workflows/test.yml`'s
  `windows-latest` legs are what actually run this tier. Read the CI result.
- **A `cfg(windows)` arm that compiles has been checked for spelling, not
  for behaviour, and the difference has already cost a shipped bug.**
  shep-channel's named-pipe arm type-checked on every Windows CI run for as
  long as it existed and deadlocked the first time a process actually
  executed it: the shepherd hands an app ONE pipe instance, `try_clone` is
  `DuplicateHandle`, and Windows serialises every operation on a synchronous
  file object, so the reader thread parked in `ReadFile` held it against the
  writer thread's `ready()` forever. Fixed 2026-09-02 by `PipeReader`, which
  peeks rather than parks. **When a platform arm has no test that runs it,
  say so out loud rather than letting a green CI imply otherwise** — the PR
  that introduced it did say so, in as many words, which is the only reason
  anyone went looking. The same audit found the docs site's Python sample
  opening the pipe twice, which succeeds and silently discards everything
  the app writes.

The instances redesign merged too: `increment_var` is removed, and refused
with the replacement named rather than a bare serde error. Env values, args,
`out_file` and `err_file` can now carry `{{instance}}` and `{{name}}`
templates (doubled braces escape a literal brace), `SHEP_INSTANCE` and
`SHEP_NAME` are always injected and can no longer be set by hand in
`[app.env]`, and an explicit `out_file`/`err_file` on a multi-instance app is
refused unless it carries `{{instance}}` or the app sets `merge_logs`. A
sheep name can no longer contain a colon, since `name:slot` (for example
`web:2`) is now a selector that reaches one instance of a multi-instance
app. `PROTOCOL_VERSION` moved from 1 to 2, because `SelectorSpec` gained
an `Instance` variant an older daemon cannot deserialize, so it refuses a
newer client at the handshake and an operator restarts the daemon after
upgrading. The output envelope's `SCHEMA_VERSION` did NOT move and is
still 1: `ProcessInfo.instance` is purely additive, and the envelope's own
rule is that only a rename, a removal or a retype bumps it. The two
constants answer different questions and it is easy to move the wrong one.
A third, `MIN_SUPPORTED`, sits beside `PROTOCOL_VERSION` and answers yet
another question: the oldest protocol this build still accepts, not the
newest it speaks. `PROTOCOL_VERSION` moving does not refuse anyone by
itself; only `MIN_SUPPORTED` moving does, and it refuses every peer built
below the new floor. `shep flock` groups a multi-instance app under one rollup row
(`web ×3`, with `↳ :0` marker rows beneath it) in `full` and `plain` style;
`bare` and JSON still print one row per instance, with `bare` suffixing the
name and JSON carrying the slot as its own field. `shep lookout`'s flock
table gained the same group row, selectable like any other, and an action
on it reaches every instance behind a confirm naming the count. `shep
bleats` now reads a log file shared by several instances once instead of
once per instance, and labels a multi-instance app's lines with their slot.

**A dog's config moved out of `shep.toml` on 2026-09-03.** A dog's section
used to live under `[dog.<name>]` in `shep.toml`; it now lives under
`[<name>]` in a new, hand-editable `$SHEP_HOME/dogs.toml`. The daemon
migrates any old sections once, at boot, and refuses to boot rather than
guess when a name holds VALUES in both files. **Not when a name merely
exists in both**, which is what this line said and is the rule the branch
removed: an empty section is a header, not a second value, so an empty
`[dog.<name>]` in the source is skipped and an empty `[<name>]` in
`dogs.toml` is written over. Every `shep enable` older than this branch
scaffolds the first shape, and refusing on it took a mixed-version host to
a shepherd that would not boot. `RawDaemonConfig::dog` is kept on
purpose: removing it would turn an un-migrated `shep.toml` into a refused
boot under `deny_unknown_fields`, so it stays as the thing the migration
reads from. The migration itself lives in
`crates/shep-cli/src/commands/dog_migration.rs`.

**Boot ordering merged on 2026-09-06.** A Flockfile app can name
`depends_on = ["db"]`, and shep sorts the flock into stages that start one
after another. An app something depends on is armed with
`ReadinessSource::Heuristic`, so it holds its stage for its own
`listen_timeout` instead of going straight to `Online`. A cycle refuses at the
keyboard and only warns at boot, where a typo must not strand a machine
nobody is watching. Shutdown walks the same stages in reverse. Dogs are held
out of that walk and stop in the backstop after every sheep, and `[daemon]
boot_first_dogs` is the only thing that moves one earlier: those dogs spawn
before the restore, every other dog after every stage. `PROTOCOL_VERSION`
moved to 5, because `AppConfig` is `deny_unknown_fields` and a shepherd at 4
cannot decode `depends_on`, so restart the shepherd after upgrading to it.

**It moved again, to 6, on the same branch.** `Response::Reloading` carried
the refused half of a staged reload back to the caller and had to say which
apps a walk could not reload, so the variant went from a tuple over
`Vec<ProcessInfo>` to a struct carrying `accepted` and `refused`. That
serializes as an object where it used to serialize as an array, which is a
retype under the constant's own rule, not an addition, so it bumps on the
same terms as the 4-to-5 move rather than skating past it. Restart the
shepherd after upgrading to this one too.

Project memory (cross-session state) tracks decisions; docs above are the
source of truth.
