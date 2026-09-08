# Design: importing a `.env` into shep's stores

Status: designed 2026-09-07, not yet implemented.

An operator with a `.env` and an app under shep has no way to get one into the
other. This design adds `shep import env`, which reads a `.env`, puts the keys
the operator names into the secret store, puts the rest into that sheep's own
env, and refuses rather than half finishing.

It also splits `shep import`, which is currently one verb meaning "read a pm2
dump", into `shep import pm2` and `shep import env`. That is a breaking change
to the CLI and it is decision 1.

## The problem

A `.env` is where an app's environment already lives. It is gitignored, it is
the operator's own file, and it holds twenty keys of which six are credentials.
shep has three places those keys could go and no way to put them there short of
typing each one.

The secret store answers the six. `shep secret set KEY VALUE` works, and the
Flockfile then needs `KEY = "{{secret:KEY}}"` written by hand, per key. The
other fourteen have no store at all: they belong in the sheep's env, which is
reachable through `Request::SetSheepEnv` and no CLI verb.

So the ask carries a design question. "Which keys should be secrets" only means
something if the keys that are not secrets go somewhere else, and that
somewhere has to be a place a running sheep actually reads.

## What already exists

Established from the tree on 2026-09-07 at `origin/main` (3a7cc61), not
assumed.

- **`shep import` is taken.** `crates/shep-cli/src/commands/import/mod.rs:1`
  reads a `dump.pm2` and writes a Flockfile. `ImportArgs`
  (`cli.rs:1313`) is four flags and no positional. `import/env.rs` already
  exists under that directory and means "split a pm2 row's env", so the verb
  names and the module names collide at the same time.
- **`secrets.json` and `kv.json` are written by the CLI directly.**
  `commands/kv.rs:1` says why: the store has to work with no shepherd running.
  `commands/secret.rs:1` repeats it. Neither module holds a `Client`.
- **`overrides.json` is daemon owned.** Only `Request::SetSheepEnv` writes it,
  and no CLI verb sends one. The lookout config pane is the sole caller.
- **An operator override survives a later Flockfile load.**
  `supervisor.rs:2378` skips any env key already present in
  `overrides.fields.env` when a load merges a template. So a value written this
  way is not undone by the next `shep start Flockfile.toml`.
- **`SetSheepEnv` needs a loaded sheep, and refuses a dog.**
  `supervisor.rs:4564` answers `Ok(None)`, which becomes
  `RpcErrorCode::NotFound`, for a name the flock does not know.
  `supervisor.rs:4556` refuses a dog by name with `SupervisorError::IsADog`.
- **`Request::SheepConfig` (`request.rs:284`) answers a `SheepConfigView`**
  carrying the effective `AppConfig` with `env` cleared, plus `env_keys`. It is
  the one request that answers "what environment does this sheep resolve to"
  without reading a value.
- **`shep describe` resolves an environment the same way**, at `query.rs:208`:
  `AppConfig.environment`, else `[daemon] environment` from `shep.toml`. It
  reads the muster roll rather than asking the daemon, and its own doc records
  that the roll can trail a config change by the snapshot debounce window.
- **A glob rule already exists.** `selector.rs:39`'s `is_glob` treats an
  argument as an exact name when it holds no metacharacter and as a glob when
  it does. `globset` is already a workspace dependency.
- **An additive request variant no longer costs a version bump.**
  `protocol/mod.rs:50` states the rule, and `Request::Unrecognized`
  (`request.rs:584`, answered at `rpc.rs:905`) is what made it safe to follow:
  a daemon that has never heard of a variant refuses it by name instead of
  dropping the session. `Request::PutSecrets` rode in on exactly this and
  forced nothing.
- **`EnvValue` is the redacted wire type for an env value**, and
  `request.rs:312` says why: it is the most secret-dense field on the wire, and
  a derived `Debug` on `Request` would print it.
- **Secret keys and values have limits.** `secrets.rs:30`, `MAX_KEY_BYTES` is
  128; `secrets.rs:36`, `MAX_VALUE_BYTES` is 4096. The key grammar is
  `[A-Za-z0-9._-]`, not starting with a dot.
- **No `.env` parser and no dotenv dependency anywhere in the workspace.**

## Decisions

### 1. `shep import` splits, and the bare form breaks

`shep import pm2` carries today's `ImportArgs` unchanged. `shep import env` is
new. Bare `shep import` exits with clap's own missing subcommand help.

An untagged serde enum over the two inputs was considered and does not work.
Serde needs one `Deserializer` feeding both arms; a `dump.pm2` is JSON and a
`.env` is a line format with no deserializer in the tree and no crate providing
one. Writing a serde `Deserializer` for `.env` is more code than a hand parser
and buys nothing.

Sniffing the file kind and dispatching on it fails for a second reason. The two
paths produce unrelated output: the pm2 path writes a Flockfile and touches no
store, the `.env` path writes `secrets.json` and calls the daemon. Their flags
do not overlap either, so one verb would accept `--out` against a `.env` at
parse time and refuse it at run time.

Keeping the bare form working means an optional subcommand beside flattened
legacy args, which leaves a verb that is permanently both a subcommand host and
an arg taker. The break is one `!` commit and a docs sweep, on a verb an
operator runs once during a migration. shep is 0.6.x.

### 2. Non-secret keys land in the sheep's env, not the kv store

Three candidates existed and two lose on the same point.

`kv.json` is flat and nothing injects it into a sheep's environment, so a key
landing there reaches no process. It is a store, but not one that answers the
question.

A Flockfile is the committed template. A `.env` is gitignored and operator
local. Importing the second into the first runs backwards, and it would put an
operator's laptop values into an app's repository.

`overrides.json` is the operator's own store for exactly this class of value,
it reaches the child at spawn, and `supervisor.rs:2378` keeps it across a later
template load. That is the target.

The cost is that `--app` names a sheep that already exists, since
`SetSheepEnv` answers `NotFound` otherwise. An operator importing before their
first `shep start` runs the start first. That is a real limitation and it is
stated in the docs rather than worked around.

### 3. The grammar

```
shep import pm2 [--from <FILE>] [--out <FILE>] [--dry-run] [--force]
shep import env <FILE> --app <NAME>
                [--secret <PATTERN>]... [--only <PATTERN>]...
                [--env <NAME>] [--dry-run] [--force]
```

Per key, after classification:

| Class | `secrets.json` | sheep env |
|---|---|---|
| secret | value in the `<env>` slot | `KEY = "{{secret:KEY}}"` |
| plain | untouched | `KEY = "<value>"` |

Both halves go through the same door, so both survive a later load.

`<FILE>` is a required positional with no `./.env` default. The pm2 verb
defaults because `~/.pm2/dump.pm2` is one global file. A `.env` is per project,
and a default is one more way to import the wrong project's credentials from
the wrong directory.

No `--stdin`. The values come from a file by definition.

### 4. `--secret` and `--only` are the selector's glob rule

An argument with no metacharacter is an exact key; one with a metacharacter is
a glob, per `selector.rs:39`. `--secret DB_PASSWORD` and `--secret '*_TOKEN'`
are both valid and neither needs its own flag.

`--only` filters and defaults to every key. `--secret` classifies what
survives the filter.

Reusing the sheep-name rule rather than inventing a second one is the whole
argument. An operator who has typed `shep stop 'web-*'` has already learned
this grammar.

### 5. A pattern that matches nothing refuses the import

For `--secret` the reason is a leak. `--secret 'DB_*'` against a file whose
credentials are named `DATABASE_URL` and `DATABASE_PASSWORD` would otherwise
put both into `overrides.json` in the clear and exit 0. For `--only` the reason
is smaller: a filter matching nothing produces an empty import that looks like
a successful one.

### 6. The environment default is the sheep's own, never `all`

`--env` names the slot in the secret store. Omitted, it is the environment the
sheep itself resolves to: `AppConfig.environment`, else `[daemon] environment`,
read from `Request::SheepConfig` rather than from the muster roll, because the
import is already connected and the roll can trail.

Defaulting to `all` was rejected. Every environment reads the `all` slot, so a
laptop `.env` imported without a flag would reach production. This differs from
`shep secret set`, which does default to `all`, and the difference is that
`import env` always knows which sheep it is importing for.

### 7. A key that looks secret and was not named gets a warning

Closed substring list: `PASSWORD`, `SECRET`, `TOKEN`, `KEY`, `DSN`,
`CREDENTIAL`, `PRIVATE`. A key matching one that no `--secret` pattern claimed
is named on stderr along with where it went. The import proceeds.

Refusing on the heuristic was rejected. False positives are real
(`SSH_KEY_PATH`, `KEYBOARD_LAYOUT`, `API_KEY_NAME`), and a refusal an operator
clears with `--force` every time is a net that has already been removed.

The precedent is `import/pm2/env.rs`, which names undecidable keys on stderr
rather than guessing, and whose own comment says not to grow the list by
guessing. That applies here too: too long loses the operator's attention, too
short costs one line of output.

### 8. A collision refuses the whole import, and `--force` overwrites

A collision is a key the import would write whose stored value is not the one
in the file, in `secrets.json` for that environment or in the sheep's env. An
identical value is not a collision, so re-importing an unchanged `.env`
succeeds with no flag.

On any collision, every one of them is named with the store it is in, nothing
is written, and the exit is `Usage`.

The precedent is `shep import pm2`, which refuses an existing Flockfile without
`--force`. The reasoning is the provider push's: a partial import looks like a
complete one.

### 9. The parser is hand written, and there is no interpolation

Roughly 200 lines in shep-cli plus tests. `$` is literal everywhere.

dotenvy 0.15.7 was read and disqualifies itself. `parse.rs:265`'s
`apply_substitution` resolves `${NAME}` against `env::var`, the shep process's
own environment, before falling back to earlier keys in the file, and an
undefined name becomes an empty string through `unwrap_or_default()`. It is
unconditional with no flag to disable it. On a credential path that means
`TOKEN=${PART}-live` silently picks up the operator's shell, and a typo'd name
silently stores a truncated secret.

The parser stays in shep-cli. Nothing in the daemon reads a `.env`.

### 10. The grammar, exhaustively

Anything not on this list refuses with a file and a line number, and any
refusal aborts before anything is written.

| Input | Result |
|---|---|
| blank line, `#` at line start | skipped |
| `export KEY=v` | prefix dropped |
| `KEY = v` | whitespace around `=` trimmed |
| unquoted value | trimmed, no escapes, `$` literal |
| `'single'` | literal, must close on the same line |
| `"double"` | `\n \r \t \" \\` only, may span lines, any other `\x` refuses |
| duplicate key | refuses, naming both lines |
| leading BOM, trailing `\r` | stripped |
| not UTF-8 | refuses |

Trailing comments get their own rule, because the obvious readings are both
wrong on a credential path:

| Line | Parses to | Why |
|---|---|---|
| `NODE_OPTIONS=--a --b` | `--a --b` | no ` #`, the rest of the line is the value |
| `PORT=8080 # dev` | `8080` | ` #` present, and the value before it is one token |
| `KEY=v#3` | `v#3` | no whitespace before the `#` |
| `PASSPHRASE=correct horse battery #4` | refuses | ` #` present and the value has internal whitespace |

Stripping every trailing comment turns the password `hunter2#3` into `hunter2`.
Never stripping one stores `8080 # dev` as a port. Both are silent. The rule
above splits only where one reading exists, and refuses the line where two do.

Requiring an unquoted value to be a single token was considered and refuses
`NODE_OPTIONS=--a --b`, which is a common and unambiguous line. It would force
an operator to edit a file they may not own.

### 11. shep's own limits, applied at classification

A key classified secret must also be a legal secret key: `[A-Za-z0-9._-]`, at
most `MAX_KEY_BYTES`, no leading dot. Its value must fit `MAX_VALUE_BYTES`. A
4096-bit RSA private key is about 3.2 KB and fits; a certificate chain does
not. Both refuse by name, and neither refusal prints a value.

### 12. `Request::SetSheepEnvBatch`, and `PROTOCOL_VERSION` stays 8

```rust
Request::SetSheepEnvBatch {
    name: String,
    entries: BTreeMap<String, EnvValue>,
    force: bool,
    dry_run: bool,
}
Response::SheepEnvBatch {
    set: Vec<String>,
    unchanged: Vec<String>,
    collisions: Vec<String>,
}
```

`SetSheepEnv` sets one key. Twenty keys is twenty round trips, none of them
atomic, and a failure at key eleven leaves the partial import decision 8
refuses. `request.rs:329` argues against a map variant, and its argument is
that no caller wants the per-key reporting back. This import is that caller.

`EnvValue` rather than `String`, per `request.rs:312`. No removal arm: the
pane's variant takes an `Option` because a pane deletes rows, and an import
never does.

The daemon applies it as one read-modify-write of `overrides.json` under the
existing lock, so every key lands or none does.

The version does not move. Additive variants keep it by `protocol/mod.rs:50`,
and the reason that rule stopped being trusted is already fixed by
`Request::Unrecognized`.

### 13. The order of writes, and what each failure leaves

1. Parse and classify. Nothing written.
2. `Request::SheepConfig`. The sheep exists, is not a dog, and its environment
   is known.
3. `SetSheepEnvBatch { dry_run: true }`. The daemon holds the live values, so
   it decides env collisions; the CLI decides secret store collisions, since it
   holds those.
4. Any collision without `--force`: name all of them across both stores, exit
   `Usage`, write nothing.
5. Write `secrets.json`.
6. `SetSheepEnvBatch { dry_run: false }`.

Secrets first and env second, deliberately. A failure at step 6 leaves values
in the store that nothing references, which are inert, and a re-run is clean
because an identical value is not a collision. The reverse order leaves
`{{secret:KEY}}` pointing at nothing, and the re-run then needs `--force`.

CLI `--dry-run` stops after step 3 and prints one line per key: key, store,
slot, byte length. Never a value.

The change parks for the next spawn, inheriting `SetSheepEnv`'s behaviour
(`request.rs:298`). The import says so on stderr and names `shep reload <app>`.
It restarts nothing itself, matching `shep import pm2`, which starts nothing.

### 14. Redaction, per IR-41

A manual `Debug` printing a count or a byte length, each with an exact-string
test, on: the parsed entry type (key visible, value as `<N bytes>`), the plan
type, `Request::SetSheepEnvBatch`, and every new error type.

Errors name the key, the line and the file. Never the value. A value reaches no
stream: `shep secret get` stays the only path by which a stored value reaches
stdout.

### 15. Module layout

`import/env.rs` already means "split a pm2 row's env", so the verb split forces
a module split.

```
import/mod.rs              dispatch
import/pm2/{dump,convert,render,env}.rs
import/dotenv/{parse,plan,apply}.rs
```

`parse` is the grammar in decision 10 and holds no shep concepts. `plan`
applies `--only`, `--secret` and the limits, and produces the two write sets.
`apply` performs decision 13's ordering.

### 16. One honest line for the docs

A key not marked `--secret` lands in `overrides.json` in the clear, and from
there it is copied into `flock.json` and the handover blob, both of which hold
each sheep's `AppConfig` verbatim. That is the complaint the secret store spec
opens with. It is correct for a value that is not a credential, and it belongs
in the docs rather than being left to be discovered.

## What is not built

- No `shep export`, which remains unbuilt from spec 1's decision 9.
- No writing a `.env` back out.
- No import into a sheep that does not exist yet, and no Flockfile output.
- No lookout pane for the import.
- No `--secret` heuristic that classifies on its own. Decision 7 warns and
  never decides.
- No kv store target.
- No whistle tool, following the secret store spec's decision 12.

## Risks

- **The docs sweep is the largest single piece of the change.**
  `from-pm2.astro` alone carries about a dozen `shep import` occurrences,
  including a `VerbSignature` and two `CodeBlock` labels, and the generated CLI
  reference has to be regenerated from a release build. A stale reference fails
  no build, which is why it drifts.
- **A `.env` parser is a grammar, and grammars grow.** Decision 10 is the whole
  of it, and every addition to that table needs the same question asked: does
  the new arm have exactly one reading.
- **`--app` naming a loaded sheep is a real constraint** for the first-run
  case, which is the case a `.env` import is most wanted in. It is a docs
  sentence today. If it becomes the common complaint, the answer is a Flockfile
  output path, not a second store.
- **The warning list in decision 7 will be wrong for somebody.** It is closed
  and short on purpose, and growing it by guessing is how it stops being read.
