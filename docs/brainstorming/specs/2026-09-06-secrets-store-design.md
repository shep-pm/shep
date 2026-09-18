# Design: the secret store, environments, and external providers

Status: designed 2026-09-06, not yet implemented. This is spec 2 of the pair
begun by `2026-09-02-config-overrides-design.md`, which reserved a token for it
and made four calls on its behalf. Three of those four stand. The fourth,
`{{shared:NAME}}`, is revised here and the reasoning is under decision 5.

Two deliverables, one design. shep grows a secret store, a token, and a push
API. A separate repository, `shep-vercel`, becomes the first provider dog and
the first dog anybody has written outside Rust.

## The problem

A Flockfile is a template committed to an app's repository. Spec 1 established
that and shipped it: an operator's own values live in a shep-owned store at
`$SHEP_HOME/overrides.json`, and a Flockfile shipping
`env = { DB_PASSWORD = "" }` is the intended pattern.

That leaves the value itself with nowhere good to go. Filling `DB_PASSWORD` in
puts a live credential into the override store, which is right as far as it
goes, and then copies it into `flock.json` and the handover blob because both
carry each sheep's `AppConfig` verbatim. Three files hold the password and two
of them are snapshots that exist for other reasons.

It also leaves an operator running staging and production on one host writing
the value twice, under two names, with nothing relating them.

## What already exists

Established from the tree on 2026-09-06, not assumed.

- **`{{shared:NAME}}` never shipped.** `crates/shep-core/src/config/template.rs:17`
  still reads `const TOKENS: &[&str] = &["instance", "name"]`, and `render` at
  line 126 is still an infallible `#[must_use] pub fn`. There is no
  `shared-env.json` and no `SharedEnv` anywhere. Spec 1's decision 10 is
  unbuilt, so this spec owns the whole token seam rather than extending one.
  `shep export`, decision 9's other half, is also unbuilt.
- **The staging helper is already consolidated.** `atomic_file.rs:42` is
  `create_staging_file(parent, prefix, suffix)`, which its own doc says four
  stores share. `config_lock.rs:32`'s `create_config_file` is a deliberate
  two-name wrapper so `shep.toml` and `dogs.toml` cannot drift. There is no
  fifth helper to write and no duplication to fold in.
- **Two stores already have the shape this one wants.** `kv.rs` and
  `overrides.rs` are both a versioned JSON document, read-modify-rename under
  an exclusive lock on a sibling `.lock`, staged through
  `create_staging_file`, `0600`. `overrides.rs:53` is also the redaction
  precedent: a manual `Debug` that prints a field count instead of contents.
- **`assemble` is synchronous and pure.** `assemble.rs:131` is
  `#[must_use] pub fn assemble(app, instance, paths, credentials) -> SpawnSpec`,
  called from ten sites in `supervisor.rs` and `dogs.rs`. It calls
  `template::render` four times while building `SpawnSpec::env`.
- **Neither snapshot carries a rendered value.** `snapshot.rs:63` stores the
  `AppConfig`, and `handover/mod.rs:486` stores "the resolved config this
  instance runs under". Both hold what the Flockfile wrote. A token in `env`
  therefore travels as a token.
- **`Hello::dog_name` is self-declared.** `request.rs:21` documents it as the
  name a client says it was registered under; nothing checks it against the
  spawn. `server.rs:156`'s `peer_pid` answers `None` on Windows, and tokio's
  `UCred::pid` is optional besides, so no portable check exists.
- **A dog is a client.** `docs/dogs.md` is explicit: a dog connects to the same
  socket the CLI uses, sends `Hello`, and makes requests. The bus runs one way.
  `BusEvent::DogConfigChanged` carries a name and the dog re-asks with
  `Request::DogConfig`; there is no daemon-initiated request to a dog anywhere.
- **The wire is a u32 big-endian length prefix and a JSON payload**
  (`wire.rs:1`), over a unix socket or a Windows named pipe.

## Decisions

### 1. One store, `$SHEP_HOME/secrets.json`, daemon-owned

Same on-disk shape as `kv.json` and `overrides.json`: a `version` field, a
`BTreeMap` so two writes of the same content produce identical bytes, a
read-modify-rename under a sibling `secrets.json.lock`, staged through
`create_staging_file`, `0600` on unix. `SECRETS_VERSION` starts at 1 and a
store carrying a higher version is refused rather than read or replaced, for
the reason `kv.rs:30` gives: there is no undo for a downgrade that overwrites
an operator's store.

It sits beside `flock.json` rather than inside it, for spec 1 decision 8's
reason. `flock.json` is a snapshot rebuilt from live state; this is authored
intent, and losing it loses the deployment.

Key grammar is `kv`'s, unchanged: `[A-Za-z0-9._-]`, one to 128 bytes, not
starting with a dot. That leaves `/` free as the namespace separator in
decision 6.

**The CLI writes the file directly and the daemon reads it at spawn.** No
request variant for `set`, `get`, `unset` or `list`, which is `kv.rs:1`'s rule
and it holds for the same reason: the store has to work with no shepherd
running, and `shep secret set` before a first `shep start` is the ordinary
first-run sequence.

The daemon reads the store in the *caller* of `assemble`, not inside it. That
is already the pattern: `assemble.rs:120` says `credentials` is resolved by the
caller "since passwd/group lookups are real I/O and this function otherwise
stays pure". A secret view arrives the same way, so `assemble` stays a pure
function of its arguments and the spawn path pays one small file read rather
than one per environment variable.

### 2. No encryption at rest, and the reason is not laziness

Spec 1 argued this and it holds. With a literal in `env`, the value is copied
into `flock.json` and the handover blob. With a reference, those carry the
reference and plaintext exists in exactly two places: this store, and the
child's own environment.

Encrypting the store would need a key on the same host, readable by the same
process, at the same `0600`. That is a second file to lose, not a second
factor. The threat it defends against, an attacker with read access to
`$SHEP_HOME` but not to the key, does not exist when both live under the same
`0700` directory owned by the same user.

So shep performs no cryptography here and takes no cryptographic dependency.
Anyone wanting real key custody uses a provider dog, which is decision 6.

### 3. Environments live inside the store, selected per sheep

An entry holds a value per environment:

```json
{
  "version": 1,
  "entries": {
    "DATABASE_URL": { "production": "postgres://db/app", "staging": "postgres://db/app_staging" },
    "SENTRY_DSN":   { "all": "https://k@o0.ingest.sentry.io/1" }
  }
}
```

`all` is reserved and cannot be activated as an environment name. It is the
Vercel "All Environments" slot and it is what stops an operator typing every
value once per environment.

Resolution is exact match on the sheep's environment, then `all`, then
unresolvable. **Never a fallback to a different named environment.** A rule
that filled an empty `staging` slot from `production` would hand a live Stripe
key to staging the day somebody forgot to set one, and that is the single worst
failure this design could have.

### 4. `environment` is an `AppConfig` field with a host default

Not a host-wide property, which is where this design started and where it was
wrong. `shep-deploy` keeps one remote and one branch per target under
`$SHEP_HOME/deploy/<sheep>/`, so one shepherd running `web` off `main` and
`web-staging` off `develop` is its ordinary layout rather than an edge case.
A host-level environment would give both the same secrets.

As an `AppConfig` field it is settable four ways with almost no new machinery:

- in a committed Flockfile, per `[[app]]` block;
- in `shep-deploy`'s `Flockfile.override.toml`, which deep merges over the
  committed file per key and is the operator's own;
- live, through `Request::SetSheepField { name, key: "environment", value }`,
  which already accepts any `AppConfig` key, so this needs no new request
  variant of its own;
- not at all, falling back to `[daemon] environment` in `shep.toml`, which
  defaults to `"production"`.

Classified `NeedsRespawn` in `config/apply.rs`, beside `env`. It is baked into
the child at exec and it decides what every `{{secret:}}` in that child
resolved to.

Two costs, both accepted:

- Adding an `AppConfig` field breaks an older `shep-deploy` reading a
  `deploy.toml` written by a newer one, because `AppConfig` is
  `deny_unknown_fields` and the record carries one as `origin`. That crate's
  own `CLAUDE.md` records this edge and records the maintainer accepting it on
  2026-09-04.
- `[daemon] environment` makes `shep.toml` unreadable by an older shep, since
  `RawDaemonConfig` is `deny_unknown_fields` (`config/daemon.rs:200`). Same
  hazard class as a protocol bump, and it belongs in the docs.

### 5. One token, `{{secret:NAME}}`, and `{{shared:}}` is dropped

Spec 1 reserved two tokens for two stores: `{{shared:NAME}}` for non-secret
values several apps share, `{{secret:NAME}}` for sensitive ones. Both are
lookups in a JSON map under `$SHEP_HOME`, differing only in whether the value
is called sensitive. That is two things where one is meant.

The shared case it drops is real and small. Five apps sharing a hostname have
five Flockfiles in five repositories and were never going to share a line
anyway. Anyone who wants a plain value in this store can put one there; it is
subject to the read gate in decision 10, and the docs say the store is for
secrets.

**The prefix is load-bearing, not decoration.** `validate` lives in shep-core
and runs from `normalize`, at config time, with no access to the store. It can
check that `secret` is a known prefix; it could never check that `DB_HOST` is a
known key. Drop the prefix and the closed token set collapses, and
`template.rs:4` says what that costs:

> An unknown token between doubled braces is refused at config time rather
> than reaching a child process as literal text.

Three regressions follow from a bare `{{KEY}}`, each against a test that
exists today:

| Written in a Flockfile | Today | Bare `{{KEY}}` |
|---|---|---|
| `{{instnace}}` | config error naming the typo and listing valid tokens | lookup for `instnace`, refused at spawn |
| `{{ .Values.port }}`, Helm passed through as an arg | config error telling the author to double the braces | lookup for ` .Values.port `, refused at spawn |
| `shep secret set name ...` | not applicable | a key nothing can reference, since `{{name}}` is the sheep name |

`doubling_escapes_a_literal_token` exists because these values carry other
people's template syntax. The current grammar turns that into a loud error with
the fix inside it.

`{{shep:NAME}}` was considered and rejected for a different reason: it is the
last generic prefix, `{{name}}` and `{{instance}}` are shep-set too so the
label does not even separate cleanly, and a later lookup of another kind would
find the obvious name spent on this one.

### 6. Namespaces route to a provider dog, and a dog pushes

`{{secret:NAME}}` reads the store. `{{secret:vercel/NAME}}` reads what the dog
registered as `vercel` has pushed. `/` cannot appear in a key, so the two can
never collide.

**Push, not pull.** The alternative is the daemon asking a dog for a value at
spawn, and it fails on two structural points rather than on taste. `assemble`
is a synchronous pure function on the hottest path shep has, so a pull turns
every spawn of every instance into a socket round trip with a timeout. And the
bus has no reverse direction: a daemon-initiated request to a dog does not
exist and would be new machinery. Push mirrors what `bark` already does, where
the daemon owns the event and the dog decides what to do with it.

So `Request::PutSecrets { namespace, environment, entries }`, answered by
`Response::SecretsPut { accepted }`. One push replaces that namespace's values
for that environment rather than merging into them, so a key deleted at the
provider disappears here on the next poll instead of lingering forever.

**A namespace claim is bookkeeping, not authorization, and the docs must say
so.** `Hello::dog_name` is self-declared and cannot be portably verified. The
real boundary is the socket, which lives under `$SHEP_HOME` at `0700`, and
anything through it can already `Request::Start` an arbitrary script as the
shepherd's user. Namespaces separate providers from each other for the
operator's benefit. They do not defend against a hostile local process, and
claiming otherwise in the docs would be a lie.

No `env_from` and no set membership, which is spec 1's call and stands. An app
draws the keys it needs rather than inheriting a set it never asked for.

### 7. Pushed values are cached to disk, in their own file

`$SHEP_HOME/secrets-cache.json`, same lock-and-rename shape, same `0600`,
keyed by namespace and environment.

Separate from `secrets.json` because the two have different lifecycles, which
is spec 1 decision 8's argument applied one level down. `secrets.json` is
authored intent and losing it loses the deployment. The cache is derived,
disposable, rebuilt on the next push, and an operator will eventually want to
delete it. One file that is safe to remove and one that is not should not be
the same file.

Cached by default. `persist = false` in the dog's own `dogs.toml` section turns
it off per provider, since it is the operator's risk and they already configure
the dog. Without the cache, a reboot leaves every provider-backed sheep waiting
on the dog plus a network round trip, and an outage at the provider means those
sheep cannot start at all.

What persistence actually costs, stated plainly so the docs do not oversell the
alternative: the child already holds the value in its own environment, and
`/proc/<pid>/environ` is readable by the same uid. Caching adds a copy that
survives a reboot and outlives the process. That is a real difference and a
smaller one than "the secret is now on disk" sounds.

### 8. `render` and `assemble` become fallible, and a spawn can refuse

`template::render` grows a `Result`. `assemble` grows one with it, which
touches its ten call sites. This is the signature change spec 1 anticipated and
it is a breaking change to shep-core's public surface, so the commit carrying
it takes a `!`.

Two refusals, and the namespace is what tells them apart:

| Case | Meaning | Behaviour |
|---|---|---|
| `{{secret:X}}`, key absent from the store | nothing will fill it but a person | refuse, `Errored` at once, no retry. The message names the key and the `shep secret set` that fixes it |
| `{{secret:ns/X}}`, namespace has no value yet | the dog is not up, or is mid-fetch | refuse, and let the ordinary restart machinery retry. Reads as `waiting-restart` |
| `{{secret:ns/X}}`, namespace is populated and lacks `X` | the provider genuinely does not have it | refuse, `Errored` at once, as the first row |

No new `ProcStatus` variant. `status.rs:33`'s `WaitingRestart` already means
not running, coming back, not a fault, and the default budget of 16 restarts on
a 100ms exponential backoff covers a dog that boots in seconds. A provider
outage long enough to exhaust that ends `Errored`, which is correct: an hour of
unreachable provider should be loud.

Without the namespace, "not filled in" and "not fetched yet" would be one
refusal and one of the two behaviours would have to be wrong.

### 9. `SHEP_ENVIRONMENT` is injected into every sheep and every dog

Beside `SHEP_NAME` and `SHEP_INSTANCE` in `assemble.rs`, and set the same way
`SHEP_DOG_NAME` already is for a dog. An app learns its own environment with no
configuration, and a provider dog learns which environment to fetch for with no
wire change.

Cannot be set by hand in `[app.env]`, the same rule `SHEP_INSTANCE` and
`SHEP_NAME` already carry.

### 10. `shep secret`, with reading gated

```
shep secret set <KEY> <VALUE> [--env <name>]
shep secret get <KEY> [--env <name>]
shep secret unset <KEY> [--env <name>]
shep secret list
```

A noun subcommand because `shep set`, `get` and `unset` belong to the kv store,
and `shep daemon <sub>` is the precedent.

`shep secret list` names keys and the environments each has a value for. Never
a value.

`shep secret get` needs `allow_read = true` under a new `[secrets]` section in
`shep.toml`, defaulting to false, exactly as `[whistle] allow_control` gates
whistle's control tools. Off by default, one line to turn on, and the operator
who turns it on has decided rather than discovered.

The honest counter, on the record because it is a good one: the file is `0600`
and its owner can read it with `cat`. Refusing to print is therefore partly
theatre. The difference is that `cat` is deliberate and `shep secret get` is a
command whose output lands in scrollback during a screen share. One gate covers
a future lookout pane too, so shep has one rule here rather than two.

Refusal exits `InvalidConfig`, 4, naming the setting.

### 11. `shep describe` names what a sheep needs, never what it holds

Per sheep: which secrets its config references, resolved in that sheep's own
environment, and whether each one resolves. Keys and namespaces only.

This is the operator's actual question when a sheep will not start, and
answering it anywhere else means reading a Flockfile and a store side by side.

### 12. whistle gets nothing

No read-only tool lists keys and no control tool sets one. A model holding
`allow_control` can already restart a sheep, and that is a different thing from
a surface whose whole purpose is credentials.

`shep secret list` names keys only, so an argument exists for exposing that one
later. Not in this spec.

### 13. Redaction, per IR-41

Every new type that can hold a value gets a manual `Debug` printing a count or
a placeholder, with an exact-string test, following `overrides.rs:53`. That
covers the store document, the in-memory view `assemble` reads, the push
request's entry map, and any error type that could carry a value.

Errors name the key, the namespace and the environment. Never the value. The
refusal for an unresolvable name is the one error an operator sees most, and it
is the one most likely to be pasted into an issue.

### 14. `PROTOCOL_VERSION` moves to 5

`Request::PutSecrets` and `Response::SecretsPut` are additive, and the rule in
`protocol/mod.rs` says additive variants keep the version. It bumps anyway, for
the reason `docs/decisions.md` records for the move to 4:

> The precedent was tested by `ApplyConfig` and it failed the operator. A
> newer CLI against a not-yet-restarted daemon passed the handshake, sent the
> variant, and had the connection dropped on an envelope the daemon could not
> decode.

A refused handshake naming `protocol_mismatch`, exit 6, beats a dead
connection with no diagnosis. That is now the house answer and this follows it.

### 15. The namespace seam for boot dependencies

Boot ordering is being solved elsewhere and is not designed here. What this
spec owes it is one queryable fact: **which namespaces does this sheep's config
reference.** The store can answer that from the sheep's `AppConfig` alone, with
no I/O, so whatever dependency mechanism lands has something to consume without
either design guessing at the other's shape.

## What is not built

- No lookout pane. The panes are being redesigned elsewhere and anything added
  here would conflict.
- No `{{shared:}}` and no second store.
- No general environment axis over config. `overrides.json`, the Flockfile
  grammar and `shep flock` are untouched by decision 4; the axis exists only
  inside the secret store and as one `AppConfig` field.
- No encryption at rest, per decision 2.
- No `shep export`, which is spec 1 decision 9's other unbuilt half and is not
  claimed here.
- No secret rotation, no TTLs, no versioned values.

## shep-vercel: the first provider dog, and the first dog outside Rust

Its own repository, its own spec, and it merges after shep's half so there is a
protocol for it to speak.

**Node, not Rust.** Two reasons, and the second is the one worth having.

`@vercel/sdk` is the official Speakeasy-generated TypeScript SDK and covers
`filterProjectEnvs` and `getProjectEnv`. The Rust side has nothing comparable:
`vercel-client` is the only management-API candidate on crates.io, at 235 total
downloads and created 2026-07-28, which fails an obscurity audit on both age
and reach, on a path that carries a bearer token. Writing the HTTP by hand in
Rust would also mean writing it against shep's deliberate absence of reqwest
(`Cargo.toml:174`, which avoids `aws-lc-sys` and its C toolchain).

And shep claims a dog is any process speaking the client wire protocol.
Nothing has ever tested that outside its own workspace. The wire is a u32
big-endian length prefix and a JSON payload over a unix socket, which is
perhaps sixty lines of framing in Node.

Expect it to surface gaps in shep's protocol documentation. That is the
finding, not a setback, and it may add a documentation task on shep's side.

**The hard part is reconnection.** `docs/dogs.md` is blunt that a dog which
does not re-dial after `shep daemon reload` becomes a live process holding a
dead socket, alive on every column a listing has and answering nothing.
`ReconnectingClient::connect_as_dog` handles that for Rust dogs and has no Node
equivalent, so this dog writes one.

**Draw the `@shep-pm/dog` boundary now.** The client half lives in its own
directory with no Vercel imports, its own tests, and a public surface shaped
like the package it becomes. Extraction later should be a move and a
`package.json`, not a rewrite. The package is not created here.

Shape of the dog itself: read `$SHEP_DOG_NAME` and `$SHEP_HOME`, fetch its own
`[vercel]` section with `Request::DogConfig`, poll Vercel on an interval, map
Vercel's targets onto environment names, and `PutSecrets` what changed. Its
token lives in `dogs.toml`, which is `0600` and whose `Debug` is already
redacted.

## Risks

- **Ten `assemble` call sites.** Making it fallible is mechanical but wide, and
  a `?` in the wrong arm turns a refusal into a panic. Every call site needs
  reading, not just compiling.
- **A new `AppConfig` field moves lookout's field-count test** from thirty-nine
  to forty, in files another session is rewriting.
- **Windows has no `/proc`**, so the honest framing in decision 7 about what
  caching costs is a unix statement. The Windows equivalent is weaker, not
  stronger, and the docs should not claim otherwise.
- **A provider dog is a credential holder running at the shepherd's trust
  level.** `docs/dogs.md` already says a dog has no sandbox. This spec adds a
  reason to care and changes nothing about the bound.
