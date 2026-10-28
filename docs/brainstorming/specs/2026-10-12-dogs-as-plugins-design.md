# Dogs as plugins

Status: design settled with the maintainer in a grill on 2026-10-12.
Nothing is filed yet. Each step under "Order of work" becomes its own spec,
written against the code as it is when its turn comes, then issues sized
for one PR each.

This is the one place the dog rework is written down. It replaces digging
through the conversations and unmerged specs it was built from.

## Where it comes from

| Date | Source | What it contributed |
|---|---|---|
| 2026-08-26 | `docs/specs/deferred.md`, #357 | Why shep had no `shep install` |
| 2026-09-21 | #566 | A dog marketplace in lookout |
| 2026-09-22 | Spec 1, branch `feat/dog-install` | Dogs as keys, not flags. The roadmap. `shep install` and `shep upgrade` |
| 2026-09-23 | Spec 0, branch `feat/verb-model` | One verb model for sheep and dogs |
| 2026-10-01 | Kelpie control session | Dogs shaped like Bevy plugins, permissions, panes built by dogs |
| 2026-10-12 | The grill | Everything under "The design" |

Both branches are docs only and local, never pushed. They were cut from
v0.10.1 (`8e0dc454`), 379 commits behind main on 2026-10-12.

| Branch | Holds | Fate |
|---|---|---|
| `feat/verb-model` | Spec 0 (397 lines), plan A (4,446), plan B (2,911) | Spec 0's decisions stand. Both plans are dropped |
| `feat/dog-install` | Spec 1 (628 lines) and the roadmap | Rewritten against main at step 3, keeping its decision table |

## The goal

A dog should feel like a key: getting one visibly adds to shep, and getting
it is one command. Dogs stop being "an optional footnote" at chapters 19 to
21 of the docs.

The shape is closer to a Bevy plugin than a pm2 module. A dog declares what
it adds to shep and what it depends on, and reaches what the operator lets
it reach. Unlike a Bevy plugin it stays a separate process: a crashing dog
can't take shep down, a dog can be written in any language, and a dog
upgrades without restarting shep.

## What a dog is today

On main at `d2483f10` (v0.12.5).

- A dog is a marker on an existing process entry, not a second registry.
- Two built-ins, `metrics` and `bark`, are compiled into the CLI binary
  (`crates/shep-cli/src/dog/mod.rs:65`). Everything else is adopted: `shep
  adopt <name> <path>` vets the binary, writes `adopted_dogs` and
  `enabled_dogs` in `shep.toml`, and starts it.
- An adopted dog becomes the verb `shep <name>`
  (`crates/shep-cli/src/adopted.rs`).
- Settings live in the dog's `[<name>]` section of `$SHEP_HOME/dogs.toml`,
  and per sheep in `[app.dogs.<dog>]` (#623). Config travels over the
  socket, never the environment.
- shep probes the binary with flags for its version, its settings schema
  and whether it wants the shepherd channel (#656).
  `shep_client::dogs::probe` answers them (#613).
- Lookout renders and edits a dog's settings from that schema (#628,
  #743).
- A dog connects to the control socket like the CLI and names itself with
  `connect_as(identity)`, an identity it reads from its own environment
  (`shep-client/src/dogs/runtime.rs:55`). It can subscribe to the bus.
- A dog with the channel gets fd 3, so `shep trigger` reaches it. A reply
  times out after 3 seconds by default, at most 58.
- No access control. The socket admits any peer with the daemon's uid
  (`server/peer_auth.rs`), so every dog can do what the operator can.
- `shep dogs --available` browses a community index and prints the install
  commands. shep installs nothing itself.

## The design

### Manifest

- A dog's manifest is what it declares about itself before it runs. shep
  reads it through one probe flag: the binary prints a versioned manifest,
  and shep caches it by the binary's sha256. Today's version, schema and
  channel answers fold into it.
- A dog in any language just prints JSON. Rust dogs get it from
  `shep_client::dogs`.
- Manifest v1 holds the version, the settings schema, the channel, the
  permissions the dog declares, its verbs and its dependencies. Events and
  panes arrive in later versions as additive fields.
- A dog's own defaults (#356) belong in the manifest.

### Permissions and grants

- A dog declares the permissions it wants. The operator grants them. The
  stored set is the dog's grants. Everything not granted is denied.
- Each dog gets a connection of its own: a second descriptor handed to it
  at spawn, already connected and speaking the existing client protocol.
  Holding it is the dog's identity, so a request's owner is never a claim.
  The shepherd carries its end across a handover, as it does for the
  channel. The dog's own child processes inherit its grants.
- The shepherd checks every request on that connection against the dog's
  grants.
- That is the whole of enforcement. There is no confinement: no OS sandbox,
  no uid per dog. A dog still runs as the operator and can open the control
  socket like the CLI, and the prompt says so plainly. Permissions keep
  honest dogs honest and make every request attributable.
- A dog started by hand, without the descriptor, connects like the CLI and
  acts as the operator.
- Permissions are coarse groups, scoped where it matters. Starting set:
  - `flock.read`, `flock.control` (start, stop, restart), `flock.edit`
    (add, delete, change config)
  - `logs.read`
  - `events:<topic prefix>`
  - `secrets.read:<namespace>`, `secrets.write:<namespace>`
  - `dogs.control`, `dogs.trigger:<name>`
  - always granted: the dog's own config section, its own channel, the
    handshake
- One table in shep-core maps every request to exactly one permission. A
  test fails on a request that isn't mapped.
- Names are stable: never renamed or split. A new request joins a group
  only if it is no more powerful, otherwise it gets a new name.
- Each declared permission is required or optional. An unknown required
  one refuses the adopt. An unknown optional one is dropped with a warning,
  so a newer dog still installs on an older shep. A permission the dog
  doesn't know about is never granted to it.
- Permissions are strings under the manifest's own version and never move
  `PROTOCOL_VERSION`.
- Every dog prompts with its declared list, first-party and built-in
  included: `shep enable metrics` prompts the first time. `--yes` grants
  everything declared.
- On upgrade, newly declared permissions wait for approval while the dog
  runs on its old grants. `shep flock` marks it, and `shep upgrade` prompts.
- The operator can grant or revoke at any time.
- Grants live in `shep.toml`, beside `adopted_dogs` and `enabled_dogs`,
  written only by the CLI.
- The first shep with permissions breaks nothing. A dog with a manifest
  gets what it declares, once, marked "granted at upgrade, not reviewed"
  in `shep flock` and `shep doctor` until the operator looks. A dog with no
  manifest is "undeclared": it keeps full access, is marked so everywhere,
  and adopting a new one prompts "declares nothing, runs with full access".
- Only dogs fall under permissions. The CLI, lookout and sheep using
  `shep_client` act as the operator. The whistle is the next candidate, in
  a design of its own.

### Verbs and dependencies

- A dog only ever gets `shep <name>`. Its manifest adds a description and
  subcommands, so `shep --help` lists it in a Dogs group and completion
  works. A dog can't claim a top-level verb.
- A dog depends on other dogs, by name and version range, and on a minimum
  shep version. `shep install` offers missing ones and enabling starts
  them first. A disabled dependency means start anyway with a warning, as
  spec 0 does for sheep. Nothing depends on a sheep.

### Calls and events

- One dog calls another with a trigger routed through the shepherd, which
  needs `dogs.trigger:<name>`. The callee is told who called. A dog lists
  its actions in its manifest, so discovery is reading manifests.
  Discovery by capability name ("whoever provides `skills`") comes later.
- Work longer than a trigger's timeout answers with an id and reports
  back through events.
- A dog may publish events, only under `dog.<name>.*`, declared in a later
  manifest version. Subscribing needs `events:dog.<name>`. This is the
  live stream a GUI needs.

### Panes

- A dog's pane is data over the channel, and lookout draws it. The dog
  sends sections from lookout's existing pieces: tables, key-value lists,
  a log tail, forms built from a schema, and actions that send a trigger.
  Live values come from the dog's events. Lookout decides layout, keys and
  styling, so a dog's pane feels like the rest of lookout.
- A new kind of section gets added only when a real dog needs one.
- The pane code lookout and dogs share moves out of `shep-cli` into a
  crate of its own, giving Rust dogs typed builders.
- A pane only shows what the dog's grants let it read.

```rust
Pane::new("paddock")
    .fields("host", [("VRAM", "20 / 24 GiB"), ("RAM", "41 / 63 GiB")])
    .table("models", ["MODEL", "STATE", "FOOTPRINT"], rows)
    .action("revoke", "Revoke lease", Needs::Row)
```

### Not in this design

| What | Why |
|---|---|
| Confinement (OS sandbox, uid per dog) | Overkill for now. The prompt tells the truth instead |
| Free-layout panes, or a dog's own TUI in a pty | A toolkit to maintain. A pty process isn't the dog, so it would step around the dog's grants |
| Shared state held by the shepherd | No consumer. Dogs share through calls and events |
| Machine leases in shep | shep-paddock owns its host's GPU and RAM (its ADR 0003). The `cargo-test` CPU share and account budgets stay in shep-kelpie until a second consumer appears; then a paddock on that host is the likely home |
| Permissions for the whistle | A different trust story, for its own design |

## Carried from earlier specs

### Spec 0 (2026-09-23), decisions stand

- `enable` and `disable` work on sheep and dogs. Disabled means stopped,
  and only `enable` starts it again. Wildcards skip it, and `all` is the
  flock, never dogs.
- The mark is per app, never per instance. `disable api:1` refuses.
- Every known dog is listed in `shep flock`, at rest when disabled, and a
  disabled dog stays out of `shep_dog_up`.
- `delete` is the one removal verb and absorbs `rehome`, which goes
  hidden. A built-in can only be disabled. `adopt` stays as it is.
- Nine smaller calls, approved as a batch on 2026-09-23, are listed in
  the handoff memory `handoff-dogs-as-keys`. They move into spec 0's text
  when it is re-read.

### Spec 1 (2026-09-22), decisions stand except where amended

| | Decision |
|---|---|
| 1 | A dog unlocks new surface: verbs, a status line, a lookout pane |
| 2 | The dog declares it in a manifest. Stored per binary identity |
| 3 | shep installs any indexed dog. Amended: every dog prompts, not only ones outside shep-pm |
| 4 | An index row approves one exact version |
| 5 | shep runs the dog's registry installer (`cargo install`, `go install`, npm later). Prebuilt downloads are spec 1b |
| 6 | Log rotation stays a dog. shep offers it when logs grow |
| 7 | shep owns a fixed list of needs and writes the hint text. The index maps a dog to a need |
| 8 | `shep install <dog>`, bare `shep install` browses, `shep upgrade <dog>` and `--dogs`. `delete` removes. Bare `shep upgrade` waits for spec 5 |
| 9 | The CLI refreshes a cached index after 24 hours. The daemon never touches the network |
| 10 | Operator dog pages move to part 2 of the docs. Writing a dog moves to Machine surfaces |

Spec 1's prompt text ("it runs as you, and can control shep and read its
secrets") is rewritten at step 3 to list what the dog declares and say what
grants do and don't stop. It also reverses three recorded decisions, each
needing a `docs/decisions.md` entry when it lands: #357's "print, never
execute", rule 7 of the dog config spec ("asked fresh, never stored"), and
"no dogs in the welcome".

## Superseded

| Earlier | Now |
|---|---|
| Roadmap: manifest (spec 3) after install (spec 1) | The manifest comes first, since the install prompt lists what a dog declares |
| Spec 1: first-party installs without a prompt | Every dog prompts |
| Spec 1, #357: confinement is the control worth most, deferred | Not planned. Grants on a per-dog connection instead |
| 2026-10-01: dogs reach anything the operator allows, "full access" | Deny by default, granted per permission |
| 2026-10-01: machine leases as a shep feature in the OS state folder | shep-paddock and shep-kelpie, as above |
| #566: a browse-only marketplace first | Install exists by step 5, so the catalogue installs |
| Plans A and B for spec 0 | Dropped, re-planned against main |

## Order of work

| Step | Spec | Covers | Existing issues |
|---|---|---|---|
| 1 | Spec 0, re-planned | Verbs for sheep and dogs, every dog listed, `delete` absorbs `rehome` | #600, #382 |
| 2 | Manifest and grants | Probe flag, manifest v1, per-dog connection, the permission table, prompts, the upgrade path | #1073, #356, #350 |
| 3 | Spec 1, rewritten | `shep install`, `shep upgrade`, receipts, SOURCE tiers, VERSION column | #357 |
| 4 | Calls and events | Triggers between dogs, `dog.<name>.*` topics | #1074 |
| 5 | Panes, then the catalogue | Pane sections over the channel, the shared crate, browse and install in lookout | #566 |
| 6 | 1b, spec 2, spec 5 | Prebuilt downloads, the on-ramp (welcome, hints, docs reorder), shep upgrading itself | #660 |

Progress is tracked in #1075.

#350 (the Windows channel pipe authenticates nobody) sits at step 2
because the per-dog descriptor needs a Windows arm.

## Left to the specs

Deliberately not decided here, so each spec can settle it against the code:

- the probe flag's name and the manifest's JSON schema
- the full permission-to-request table
- the CLI and lookout surface for granting and revoking
- the prompt's wording
- the per-dog descriptor's Windows arm
- the section kinds a pane starts with, and the shared crate's name

## Open issues, sorted

39 open issues carry `area: dogs`. Most are unaffected.

| Bucket | Issues |
|---|---|
| Reshaped or closed by the steps above | #356, #357, #566, #660, #605, #600, #382, #350 |
| Leans on them | #689 and #692 (a sheep asks the operator), #744 (remote sheep), #362 and #363 (channel clients for any-language dogs) |
| Dog config, secrets and their lookout editing. Unaffected | #681, #713, #718, #719, #720 to #731, #745 |
| Metrics, bark and OTLP features. Unaffected | #287, #341, #347, #386, #399, #558, #560, #569, #576, #610, #694, #717, #733 to #739 |
| Test and duplication cleanups. Unaffected | #322, #327, #331, #334, #412, #451, #455, #490, #494 |

Shipped toward this design: #607, #613, #623, #628, #630, #632, #656,
#665, #667, #695, #732.
