# The lookout pane design pass

Eight changes to `shep lookout`, from using the config panes on a real
flock. Four of the original twelve pieces of feedback shipped in the pane
polish branch already, and one needed no work; this covers what was left.

The items are separate features that interact, which is why they are one
spec. Separating dogs on the dashboard dissolves the dog-editing complaint.
A menu on close reuses the `esc` semantics the polish branch just settled.
Splitting the field groups moves what `shep init` writes.

Status: implemented on `feat/pane-design-pass`.

## Decision 1: dogs get their own section of the dashboard table

A `Flock` header, then the sheep, then a `Dogs` header, then the dogs. One
table, so `j`/`k` still walk the whole list and no key moves focus between
panes.

**Why:** today a dog sorts by name into the middle of the flock with no
marker of any kind. In the pinned gallery, `metrics` sits between `cron` and
`web` and nothing on the row says it is a dog. `shep flock` on the CLI
already prints a separate `Dogs` section, so this also stops the two
surfaces disagreeing.

Two rejected alternatives. Separate stacked panes read strongest but cost
vertical space on a short terminal and need a focus key. A marker column
alone identifies a dog without grouping it, which fixes half the problem.

## Decision 2: `e` on a dog row opens the dog pane

The refusal that names the settings screen goes away.

The pane spawns the dog's binary with `--schema` when it opens, so it needs
that path. **It is already on the wire.** `ProcessInfo::dog` is an
`Option<DogSource>`, and `DogSource::Adopted { path: String }` carries "the
binary's path, exactly as the operator gave it to `adopt`". So the dashboard
reads it from the row it already has.

No wire change, no new field, no version constant moves.

This paragraph originally specified a new `ProcessInfo::adopted_path:
Option<PathBuf>` and was wrong twice. The data was already there, and
`ProcessInfo`'s own header comment rules out the type: paths travel as
`String` because serde's `PathBuf` refuses a non-UTF-8 path and would blank
a whole `Reply`.

**Two sources of a dog's path exist and they can disagree.** The settings
screen reads `adopted_path` from the local `shep.toml` through
`dog_candidates`, with no daemon round trip. The daemon's `DogSource` holds
what the running dog was actually adopted with. Where they differ, the
daemon is right, so the dashboard path uses `DogSource` and the settings
screen keeps its own reader.

The rejected alternative was fetching the settings snapshot when `e` is
pressed. It costs a round trip on the keypress and a failure mode `e` on a
sheep does not have, and it now also costs more than reading a field that
has been on the wire all along.

## Decision 3: `--allow-control` inverts to `--read-only`

Control is on by default. `--read-only` opts out.

**Why:** the flag was never a security boundary and could not be, since
anyone who can run `shep lookout` can run `shep stop`. What it guards is a
stray keypress on an open dashboard.

**An earlier draft of this argued the confirm already made the flag
redundant, and that is false.** `arm` checks `self.control` first, before
anything else in its refusal ladder, so under `ReadOnly` no `Action` is
created and there is no confirm to be redundant with. Today the two are
stacked: gate, then confirm. Inverting leaves one layer, it does not remove
a duplicate.

The case is that one layer is enough here. After a stray `x`, the confirm is
armed and nothing has happened; a second stray key applies it only if that
key is `Enter`, any other key cancels silently, and the whole thing expires
after `CONFIRM_EXPIRY`, ten seconds. That is two particular keys in order
inside ten seconds, against friction on every single invocation.

**And the friction has no convenient escape today.** `lookout.allow_control`
is read by `resolve_control` and is real, but nothing in the settings screen
writes it: `SettingField::AllowControl` is the *whistle* server's
`[whistle] allow_control` in `shep.toml`, a different flag entirely. So an
operator's only way to set it once is `shep set`.

The KV key stays and keeps its meaning, now as the way to ask for read-only
everywhere. What changes is the default when neither the flag nor the key
says anything.

This changes shipped behaviour. The commit that lands it carries a `!` and
the release notes name it.

## Decision 4: the `CFG` column gets the legend the pane has

`*N` and `!N` are as unexplained on the dashboard as they were in the pane.
The pane's hint gained `* yours` and `! parked`; the dashboard gets the same
two, subject to its own width budget.

## Decision 5: the field groups go from four to eight

| group | fields |
| --- | --- |
| `process` | name, script, interpreter, cwd, user, group, instances, fold, reuse_port |
| `logging` | out_file, err_file, merge_logs |
| `inputs` | args, env, stdin, channel |
| `restart` | autostart, autorestart, max_restarts, min_uptime, restart_delay, exp_backoff_restart_delay, stop_exit_codes, max_memory |
| `readiness` | readiness_probe, liveness_probe, wait_ready, listen_timeout |
| `shutdown` | kill_signal, kill_timeout, graceful_timeout, shutdown_with_message, action_timeout |
| `watch` | watch, watch_delay, ignore_watch, watch_options |
| `cron` | cron_restart, cron_timezone |

`control` held 20 of the 39 fields, which is what made the pane hard to
scan. Two fields change group rather than only moving between sections:
`kill_signal` from `process` to `shutdown`, and `max_memory` from `control`
to `restart`, since exceeding it is what causes one.

Two consequences, and the second is smaller than this spec first claimed.
`GROUP_ORDER` is a published `pub const` in shep-core and its contents
change, which is a value change rather than a signature one.

**`shep init` gains no section headers, because it never had any.**
`GROUP_ORDER` is read by `grouped_order` for ordering alone, and the group
names appear exactly once in `scaffold.rs`, in the const itself. So
regrouping reorders the fields in a generated Flockfile and prints nothing
new. Group headers exist only in the lookout pane, through
`view::pane::section_header`.

The test that has to move with this is
`field.rs`'s `the_real_flockfile_schema_yields_thirty_nine_fields_in_four_groups`,
which asserts the four names as an exact list.

## Decision 6: an array field opens a list sub-screen

`enter` on an array opens it. Elements one per line, with add, edit, remove,
and move. Only `args` is order-sensitive, but the move keys are cheap enough
to offer everywhere rather than conditionally.

Four fields qualify and all four are flat arrays of scalars: `args`,
`ignore_watch` and `watch_options` of strings, `stop_exit_codes` of
integers. No nesting, so the screen never needs to recurse.

No new request. `Request::SetSheepField` already carries a
`serde_json::Value`, so an array value travels as it is.

**The array screen shows its values, unlike the env screen.** Env is
write-only because a value there can be a credential and the pane is never
told one. An args list is not a secret, and hiding it would leave the screen
unable to say what it is editing.

## Decision 7: suggestions become a schema concept

A new `init.suggest`, a list of strings, read by the same code that reads
`init.blurb` and `init.group`. In the pane it becomes
`FieldKind::Suggested`: `space` cycles the suggestions and `e` types
anything else, which is what those two keys already do elsewhere.

Both fields that need it use the same mechanism, which is a simplification
on what this spec first said:

- `cron_restart` has an open grammar and can never be a closed list, so it
  keeps free text and gains suggestions for the common patterns.
- `kill_signal` gets suggestions too, `SIGTERM`/`SIGINT`/`SIGQUIT`/`SIGUSR2`,
  rather than becoming a schema `enum`.

**The enum route is closed, and deliberately so.**
`flockfile.rs`'s `kill_signal_stays_an_unconstrained_string` asserts the
field carries no `enum` and no `pattern`, and says why: the four names belong
to `normalize`, and "a schema that listed them would be describing something
it cannot see". `normalize` is what refuses a bad one, through
`KillSignal::parse`, which also accepts a missing `SIG` prefix and any case,
neither of which an enum of four exact strings would allow.

`init.suggest` sidesteps that cleanly. It is neither `enum` nor `pattern`, so
the test passes untouched and the schema still constrains nothing. It offers
four values to cycle while `e` still types `sigterm` or anything else the
daemon will accept.

So one mechanism covers both fields, and `FieldKind::Choice` is left for
schemas that really do close a set.

A dog's schema goes through the same reader, so a dog author can declare
`suggest` and have their field cycle without shep knowing anything about
that dog.

## Decision 8: a menu on close, when something is parked

Closing a pane with parked fields opens a menu instead of closing. Silent
when nothing is parked, so reading a pane costs nothing. `esc` is the only
close now, so `esc` is what opens it, and `esc` again leaves.

**It is an offer, not a save prompt, and the wording has to say so.** A pane
edit is written to the override store on the same call that makes it. Parked
means not yet in the running process, never unsaved. Closing loses nothing.

**It counts parked fields once, not split by kind.** The feedback asked for a
count of fields needing a reload against fields needing a restart, and that
distinction does not exist: `promote_pending` runs from `respawn` for a
restart, and a reload's replacement reads `intended_spec`, which already
carries the pending values. Every parked field lands either way.

What is worth showing instead is which reload this app would get, since that
is real and per-app. **The rule takes three fields, not two.** `ReloadMode`
reads a `ReadinessSource`, and `ReadinessSource::of` answers `Channel` for
`wait_ready` before it ever looks at `readiness_probe`. So a reload is
serial only when `wait_ready` is false, `readiness_probe` is set, and
`reuse_port` is false. An app with both `wait_ready` and a probe still
overlaps.

The pane can compute that itself with no new request. `ConfigPane::sheep`
builds its values by serializing the whole effective `AppConfig` from
`SheepConfigView`, and none of the three fields is renamed on the wire. Read
`readiness_probe` off the values map for presence rather than through
`display_value`, which stringifies the whole object.

`ProcessInfo` carries none of the three, so this works in the pane and would
not work from the dashboard.

Three choices: reload, restart, leave.

This is the first thing in lookout that appears without a keypress aimed at
it. That is why it must stay silent when nothing is parked, and why `esc
esc` has to work.

## What this does not do

`cron_timezone` gets no suggestions. The IANA set is too large to cycle and
picking a handful would be arbitrary.

Nothing here addresses editing a nested object such as a probe. Those stay
`FieldKind::Opaque` and read-only. Every array in scope is flat, and a
recursive editor is a bigger design than this pass.

## Wire and compatibility

**Nothing changes on the wire.** The one decision that looked like it needed
a field turned out to be reading data `ProcessInfo::dog` already carries.
No version constant moves and no request gains a variant: an array value
travels through `SetSheepField`'s existing `serde_json::Value`.

`--allow-control` inverting is the only behaviour change an operator can
trip over, and it is in the direction of doing more rather than less, so a
stale habit costs a flag that no longer exists rather than a surprise.

## Testing

Each decision needs its own tests, in the shapes this module already uses:
rendered-output assertions for the table and the panes, insta snapshots for
the wide layouts, and the pinned gallery for anything that changes a frame.

Three specific risks are worth naming for whoever writes the plan:

- Adding section headers to the table changes what a row index means. Every
  selection, filter and action path reads that, and the group rollup row for
  a multi-instance app already complicates it.
- The menu on close and the help overlay both draw into the pane's counted
  line budget. The rows-versus-lines invariant in `view/scroll.rs` is what
  keeps the cursor reachable, and it is the thing most likely to break.
- Regrouping the fields moves `shep init`'s output, which has exact-string
  tests.
