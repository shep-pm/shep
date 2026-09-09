# Lookout pane 1h: secrets

**Status:** approved 2026-09-08.

The last pane of the redesign bundle. `docs/lookout/design-files/rulings.md` held
it back until the secret store landed, with the instruction to build against what
lands rather than against the frame. The store landed in
`crates/shep-core/src/secrets.rs`, and it is narrower than the frame in some
places and wider in others.

Where a frame and a ruling disagree, the ruling wins. Where the frame and the
shipped store disagree, this document records which way it went and why.

## What the frame gets wrong

Verified against the code before any of it was designed around.

| Frame | Shipped | Outcome |
|---|---|---|
| `sealed at rest` | no encryption anywhere in `crates/`. `crates/shep-daemon/src/snapshot.rs:814` calls shipping unencrypted a spec decision | line removed. The terms row says `not encrypted` |
| per-sheep scope, `catcher.SENTRY_DSN` nested under a flock-wide row | `entries` is key to environment to value, with `ALL_ENVIRONMENTS = "all"` | tab row is environments |
| `LAST SET` column | no timestamp in the module | column dropped, replaced by `SET IN` |
| `READ BY` from a reader record | no reader record, no audit log | kept, but sourced from the muster roll. See below |
| "a reveal is written to the daemon's audit log with your uid and the time" | no audit log, no uid capture, and the daemon is not in the path at all | removed. The panel states the gate instead |
| `lookout.allow_control` is this pane's gate | that key lives in `kv.json`. The reveal gate is `[secrets] allow_read` in `shep.toml` | two gates, stated separately |
| store at `~/.shep-play/kv.db` | `$SHEP_HOME/secrets.json` | corrected |
| a sealed value renders as a block run whose length is the value's length | `MAX_VALUE_BYTES` is 4096 and the column is 30 cells | run is proportional, byte count exact and stated |
| `g` opens the pane | `g` is `SelectFirst` (`crates/shep-cli/src/lookout/input.rs:59`) | `S` |

One frame claim survives intact. `SecretFile` has a redacted `Debug` printing the
key count and never a value, so "never printed to a log" is true today.

The frame also missed an axis. `{{secret:vercel/TOKEN}}` reads what a provider
dog pushed, held in `secrets-cache.json` and separate from the operator's store.
A pane showing only `secrets.json` hides half of what a config resolves against.

## Two decisions worth their reasoning

### LAST SET became SET IN

Timestamps could be added without breaking anything: a `set_at` sibling map in
`SecretFile` under `#[serde(default)]` leaves `entries` alone, so `all()` keeps
its signature and `SECRETS_VERSION` does not move. It was still the wrong first
move. That column would read `-` for every key already in the store, and would
fill only as keys got re-set. A column that is empty on day one has the same
defect as a column with nothing behind it.

`SET IN` reads `2 of 3 . all, production` from `entries` alone and is complete
immediately. The design system's second rule wants every measurement to state its
denominator, and this one can. `shep secret list` already prints the same pair, so
the pane and the CLI agree by construction.

Timestamps stay available for whenever rotation tracking is wanted.

### READ BY comes from the roll, not from a new request

The frame's own wording is forward looking: a meadow block if the reader "currently
holds the value", a shaded block if it "will read it at next start". That is a
question about who consumes a key, not about who has looked at one, so no audit
trail is involved.

`secrets::references(&AppConfig)` already answers it. It walks `env` values, `args`,
`out_file` and `err_file` through the same tokenizer `template::render` resolves
against at spawn.

The wire cannot supply the input. `SheepConfigView::new` clears `env` before the
struct exists (`crates/shep-core/src/protocol/request.rs:1404`), and secret
references live in env values.

The muster roll can. It stores `env` verbatim
(`crates/shep-daemon/src/snapshot.rs:278`) while storing the `{{secret:...}}`
reference rather than a resolved value, which is what the test at
`crates/shep-daemon/src/snapshot.rs:814` pins. References present, values absent.

It is also current. The roll is rewritten on every registry change and every
lifecycle state change, debounced 250ms
(`crates/shep-daemon/src/snapshot.rs:50`, writer loop at line 735). Lookout's own
poll is 2000ms, so the roll is fresher than the pane refreshes.

`gather_secrets` in `crates/shep-cli/src/commands/query.rs:179` already does this
whole walk for `shep describe`. It is a private function in `shep-cli`, and
`lookout` is in that same crate, so the pane reuses it after a module extraction.
A second implementation daemon-side would be two answers to one question.

One honest cost: a failed roll write only warns
(`crates/shep-daemon/src/snapshot.rs:788`), so a stale roll is silent. The panel
states the roll's age from `FlockSnapshot::saved_at_ms`.

The caption is corrected either way. Nothing can tell a sheep spawned before a
`set` from one spawned after, so the pane never claims a reader "holds the value":

- `online, was given a value at spawn`
- `not running, reads it at next start`

## Where the data comes from

| What | Source | Mechanism |
|---|---|---|
| operator keys and values | `$SHEP_HOME/secrets.json` | `secrets::all` on `Effect::LoadSecrets`, landing as `Msg::Secrets` |
| provider keys and values | `$SHEP_HOME/secrets-cache.json` | `secrets::provider_cache_on_disk`, same effect, read-only rows |
| who reads each key | `$SHEP_HOME`'s muster roll | the extracted `gather_secrets` |
| reveal gate | `$SHEP_HOME/shep.toml` | `[secrets] allow_read`, already in the settings snapshot |

Writes go to the file, as `shep secret` and the settings screen already do:
`Effect::WriteSecret(SecretEdit, WriteAuthority)`, landing as `Msg::SecretWritten`.

Both file paths run on `spawn_blocking`, for the reason `WriteSetting` gives at
`crates/shep-cli/src/lookout/app.rs:335`. The lock acquires with no deadline, and
secrets takes one on `secrets.json.lock`. Blocking the UI task stalls redraw, tick
and bus drain together.

Because the write is file-direct, the running shepherd learns nothing. A new value
reaches a process at its next spawn. That is what fills `LANDS`, and it lets the
pane answer "when does this take effect" without asking anyone.

### Two gates, two questions

- `lookout.allow_control` in `kv.json` decides whether the pane may change
  anything. Set, delete and new-key refuse without it, through the existing
  `READ_ONLY_REFUSAL`.
- `[secrets] allow_read` in `shep.toml` decides whether a value may be shown.
  Reveal alone, and off by default.

Neither implies the other. A read-only lookout with `allow_read` on can reveal but
not write. A controlling lookout without it can rotate a secret it cannot read.

## The frame

160 cells. `SENTRY_DSN` is mid-reveal, so both value states appear at once.

```
██ SECRETS   flock-wide values a Flockfile refers to and never carries
   store $SHEP_HOME/secrets.json · not encrypted · never printed to a log, never carried in a bleat · read at spawn, not now
   reveal  [secrets] allow_read = false in shep.toml · change  lookout.allow_control = true

   all │ [production] │ staging │ ci                        4 environments in this store · ←/→ or tab

  KEY                         VALUE                         IN FORCE      SET IN                READ BY               LANDS
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
██ OPERATOR   17 keys · you set these
  DB_PASSWORD                 ██████ 12 bytes               production    3 of 4                2 of flock · 2 up    at next start of catcher, web
> SENTRY_DSN                  hunter2-not-really            all           1 of 4 · all          1 of flock · 1 up    visible 6s ███████░░░
  STRIPE_KEY                  not set here                  -             1 of 4 · ci           nothing reads it      no sheep names it yet
  OLD_TOKEN                   ████ 31 bytes                 production    1 of 4 · production   nothing reads it      no sheep names it yet
  + new key                   NEW_KEY_█                                   letters, digits, . _ -                      up to 128 bytes, not starting with a dot

██ vercel (dog)   4 keys · pushed by a provider · read-only here
  vercel/API_TOKEN            ████████ 40 bytes             production    2 of 4 · all, produ…  1 of flock · 0 up    at next start of api
  vercel/PROJECT_ID           ████ 24 bytes                 all           1 of 4 · all          1 of flock · 0 up    at next start of api

────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
FOCUSED  SENTRY_DSN                                                                     WHO READS IT  SENTRY_DSN
  the value leaves the screen after 10s. nothing                                          █ catcher   online · was given a value at spawn
  records that you looked: the store is 0600 and                                          ░ web       not running · reads it at next start
  anyone who can reveal can also delete a log.
  set in  all · length 18 bytes · named by 1 of the flock                                        named in env, args, out_file or err_file

  v reveal for 10s   ↵ set a value   D delete the key   y copy to clipboard   esc back      █ control enabled
```

### Columns

Losing `LAST SET` forces a reallocation, so these are new numbers rather than the
frame's.

| Column | Frame | Now | Note |
|---|---|---|---|
| gutter | 2 | 2 | |
| `KEY` | 26 | 28 | namespaced keys carry a `vercel/` prefix the frame did not budget for |
| `VALUE` | 32 | 30 | |
| `IN FORCE` | 14 | 14 | was `SCOPE` |
| `SET IN` | 16 | 22 | was `LAST SET`. The list needs the room |
| `READ BY` | 26 | 22 | a count here, names in the panel |
| `LANDS` | 44 | 42 | |

`SET IN`'s denominator is the number of tabs, `all` included. `all` is a slot a
key can hold and a tab an operator can select, so counting it keeps the number on
screen checkable against the row above it. Excluding it produces `1 of 3 · all`,
which names a slot outside its own denominator.

Neither panel nor column says a bare "sheep" for a count. `docs/terminology.md`
rules that out: the plural is always the flock, because a bare "sheep" cannot be
told from the singular. A count reads `2 of flock`, and `named by 1 of the flock`.

`IN FORCE` names the slot supplying this tab's value: the exact environment, or
`all` as fallback, or `-` when nothing resolves here. It mirrors
`SecretView::resolve` (`crates/shep-core/src/secrets.rs:607`) rather than
describing it, so a test can hold the two together. There is never a fallback to
another named environment, for the reason that function's own doc gives.

Rows are keys. The tab picks the environment. Namespaces are row groups, not tabs,
because an environment is a slice of one key while a namespace is a different
store. Provider rows allow `v` and refuse `Enter` and `D`.

### Glyph widths

`rulings.md` asked for the whole glyph vocabulary to be width-checked, and two of
the frame's choices fail it.

The frame marks the selected row with `▌` and the active tab with `▐ ▌`. Both are
East-Asian Ambiguous, and `crates/shep-cli/src/lookout/view/flock.rs:75` already
rejected `▌` in the gutter for that reason: a doubled cell shifts the whole row.
The selection therefore uses the existing `mark` and `edge` pair, an ASCII `>` with
no colour and a painted space with it. The active tab is bracketed as
`[production]`, which is also what the design's third rule wants, since a tab
marked by colour alone says nothing without it.

`█` and `░` stay. They are Ambiguous too, but they already ship inside fixed
gauges at `crates/shep-cli/src/lookout/view/host.rs:308` and
`crates/shep-cli/src/lookout/view/flock.rs:1481`, so the vocabulary is settled.

An absent value is an ASCII `-`, matching `crates/shep-cli/src/lookout/view/detail.rs:52`.

Narrow terminals get a `SECRET_TIERS` table shaped like `TIERS` in
`crates/shep-cli/src/lookout/view/flock.rs:345`, with the same
`every_tier_fits_the_width_it_claims` test. Drop order is `LANDS`, then `READ BY`,
then `SET IN`. `KEY`, `VALUE` and `IN FORCE` are the pane and never drop.

## Keys

`S` opens and `S` closes, mirroring settings at
`crates/shep-cli/src/lookout/app.rs:330`. `S` was picked rather than found: the
frame said `g`, which is `SelectFirst`. `s` opens settings and `S` opens secrets,
both butter change-screens over the shepherd's own configuration. `F` against `f`
already ships as an unrelated shift twin.

| Key | Does | New `KeyPress` |
|---|---|---|
| `j` `k` `g` `G` | move selection | no |
| `left` `right` | previous and next environment tab | yes, both codes unbound |
| `z` | collapse a namespace group | no, `Collapse` exists |
| `r` | re-read store, cache and roll | no, `Refresh` |
| `v` | reveal for 10s | yes |
| `Enter` | set a value | no, `Confirm` |
| `D` | delete the key | yes |
| `y` | copy | yes |
| `esc` `q` | back, quit | no |

`map_key` dispatches on `InputMode` rather than pane, so every pane-local key is a
global `KeyPress` the app routes by screen. The open key therefore cannot be one
of the four the pane itself uses.

### Enter collides with itself

`D` is destructive, so it arms and `Enter` confirms, like `x`, `R` and `L`. But
`Enter` also sets a value. Armed, it confirms the delete. Not armed, it opens the
input. The status bar says which, and both directions are tested.

### Reveal

Refused outright when `[secrets] allow_read` is false, with the gate sentence
rather than a silent no-op. Clears on timeout, selection move, tab change, `esc`,
close, quit, and a successful write. The countdown rides the existing tick.

### Copy

No clipboard code and no clipboard dependency exist in the repo. OSC 52 needs
neither. `shep-cli` already hand-rolls base64: `crates/shep-cli/src/serve/auth.rs:201`
decodes and its test module at line 239 encodes. The encoder moves beside the
decoder, and the comment claiming Basic auth is the only base64 in the crate gets
corrected.

Two constraints on the caption. OSC 52 is write-only and the terminal never
replies, and many terminals refuse it by default, so the pane says `copy sent to
the terminal, some terminals refuse it` and never `copied`. The system clipboard
is readable by every process on the desktop, which the FOCUSED panel says once.

### Redaction

Three new types carry plaintext: the revealed value in `App` state, `SecretEdit`,
and the new-key input buffer. Each gets a redacted `Debug` with an exact-string
test, following `SecretCommand::Set` printing `Some(<7 bytes>)` (IR-41). The set
input seeds empty and never with the current value, matching
`crates/shep-cli/src/lookout/pane.rs:1196`.

## Errors

All on screen, none silent.

| Cause | Shown |
|---|---|
| `FutureVersion` or `Decode` | table replaced by the error, as `gather_secrets` already reports an unreadable store |
| write fails | status bar, from `Msg::SecretWritten` |
| value over `MAX_VALUE_BYTES` | refused at the input, before submit |
| key outside the grammar | refused at the input, with the grammar |
| roll missing or stale | panel states its age from `saved_at_ms` |
| namespace row, `Enter` or `D` | refused: pushed by a dog, not yours to change |

## Testing

Two failures from the panes that just shipped are designed out rather than
promised around.

`contains` over a whole buffer pins nothing. The existing view tests assert that
way, at `crates/shep-cli/src/lookout/view/settings.rs:1053` and line 920, which is
how two tests last round passed on text present for an unrelated reason. This
pane's columns are fixed, so assertions go through a
`cell(buffer, row, Column::InForce)` helper. `production` in the wrong column then
fails instead of passing.

Every test carries a mutation receipt. The plan names what to break and what
should fail. The implementer mutates, watches it go red, restores, and quotes the
failure line in the DONE report. A test whose mutation stays green is reported
rather than quietly kept.

Every capability is tested from `map_key` through `KeyPress` to the state or
`Effect`. Frame 1i counted its filter axes without asking whether a key reached
them, and testing an inner function proves only the inner function.

The coverage table cites the test's `file:line` and the `file:line` it exercises.
The reviewer greps both. A row that cannot cite a line is not covered.

What needs pinning:

| Claim | Test |
|---|---|
| `IN FORCE` agrees with `SecretView::resolve` | a table of store states through both, on the same input |
| `S` opens, `S` closes, `g` still selects first | from `map_key` |
| `Enter` sets when idle and confirms when `D` armed | both directions |
| reveal clears | one test per trigger: timeout, selection, tab, `esc`, close, quit, write |
| `allow_read` off refuses reveal, `allow_control` off refuses write | independently, since neither implies the other |
| the three plaintext types redact | exact-string `Debug` |
| a 4096-byte value's block run stays inside 30 cells | the frame's own rule cannot hold, so its replacement is pinned |
| namespace rows refuse `Enter` and `D` | and still allow `v` |
| every tier fits its width | shaped like `every_tier_fits_the_width_it_claims` |

## Docs

The `CLAUDE.md` trigger fires: `S` is a new key. Regenerate the CLI reference from
the built binary, grep `web/src/pages/docs/*.astro` for the keymap, then run both
`npx astro build` and `npx astro check`. `check` is the one that catches a wrong
prop.

The 1k keymap overlay gains `S` under `LOOKING`, and `v`, `D` and `y` under the
pane's own listing.

## Not in this pane

- An audit log. The store is `0600` and owned by the operator, so anyone who can
  reveal a secret can also delete the log. That records honest operators and
  cannot record anyone else. It becomes worth building when something else holds
  the log: a provider dog, a syslog sink, a remote bark.
- Encryption. `crates/shep-daemon/src/snapshot.rs:814` records the existing
  decision and nothing here revisits it.
- Timestamps, per the reasoning above.
- Editing a provider dog's pushed values. They are a cache of what the dog said.
