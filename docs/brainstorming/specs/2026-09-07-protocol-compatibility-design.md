# Protocol compatibility: additive changes stop being breaking

**Status:** approved 2026-09-07. Not yet implemented.

`PROTOCOL_VERSION` went from 1 to 7 in about a week. Every move refuses every
peer that is not on the identical number, including peers untouched by the
change that caused it. This design makes additive changes free, so the version
moves once or twice a year instead.

## The problem

Compatibility is enforced in one line, `crates/shep-daemon/src/server.rs:406`:

```rust
if hello.protocol != PROTOCOL_VERSION {
```

Exact equality. No range, no capability negotiation. A dog that only sends a
`Hello` and reads bus events is refused by a bump that retyped a reload reply
it will never see. Dogs are ordinary clients on the same socket, so there is no
separate dog protocol to be lenient about.

### Only two of the six bumps were forced

| Bump | Cause | Forced? |
|---|---|---|
| 1 to 2 | `SelectorSpec` gained `Instance` | partly, a catch-all would have softened it |
| 2 to 3 | `ResetDepth::Settings` renamed to `Policy` | no, `#[serde(alias)]` costs nothing |
| 3 to 4 | four additive variants | no, and it was bumped deliberately |
| 4 to 5 | `AppConfig` gained `depends_on` | no, if the wire type were not `deny_unknown_fields` |
| 5 to 6 | `Response::Reloading` tuple to struct | yes, structural |
| 6 to 7 | `Response::Restarted` tuple to struct | yes, structural |

The 3 to 4 entry is the one that explains the rest. Those variants were
additive and the repo's own rule said not to bump. `CLAUDE.md` recorded why it
happened anyway: "skipping the bump is what made `ApplyConfig` fail on a dead
client rather than a named refusal."

The bump was not needed for correctness. It was needed because the alternative
failure was so bad. An unknown variant is a hard decode error, and at
`server.rs:365` the `?` propagates out of `read_loop` and out of `handle_conn`,
so one unrecognized request tears down the whole session. Faced with a silent
hang or a clean refusal, a clean refusal wins, and every defensive bump then
breaks every dog. Fix the failure mode and the defensive bumps stop.

### What the wire already tolerates

Verified against the tree, not assumed.

- JSON over `serde_json`, length delimited (`protocol/wire.rs`). Self
  describing, so additive evolution is possible at all.
- `Hello` deliberately has no `deny_unknown_fields`, with a comment at
  `protocol/request.rs:12`: "refusing an unknown field here would refuse a
  newer client before `protocol` is read." The handshake was already designed
  to evolve. Nothing has used that.
- `AppConfig` is `deny_unknown_fields` (`config/app.rs:73`) and travels on the
  wire, which is why a new Flockfile field is a protocol event.
- No `#[serde(other)]` exists anywhere in the wire path.

## The contract

> shep accepts any client at or above `MIN_SUPPORTED`, and `PROTOCOL_VERSION`
> moves only when a message shape changes in a way an older peer cannot read.

Two things this deliberately does not promise:

**It is forward only.** Accepting an old peer is not the same as answering it
in an old shape, and the daemon does not down convert. The promise is that
everything additive from the floor onward is free, not that old dogs work
forever.

**The floor rises on a structural break**, and that refuses every peer built
below it. That is today's behaviour. The difference is that it should happen
once or twice a year rather than six times in a week, because the four
avoidable causes above stop being causes.

## Design

### 1. The handshake takes a floor

```rust
// crates/shep-core/src/protocol/mod.rs
pub const PROTOCOL_VERSION: u32 = 7;   // what this build speaks
pub const MIN_SUPPORTED: u32 = 7;      // the oldest peer it accepts
```

```rust
// crates/shep-daemon/src/server.rs:406
if hello.protocol < MIN_SUPPORTED {
```

No upper bound, deliberately. A peer newer than the daemon is accepted; if it
asks for something that does not exist yet, that is answered per request by
section 2 rather than by hanging up at connect time. This is the direction that
matters for dogs, since rebuilding a dog against a newer shep-client than the
running daemon is refused today for no reason.

`HelloAck` gains one field so a client can see the window:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub min_supported: Option<u32>,
```

`Option` with a default because a new client meets old daemons that do not send
it. `HelloAck` has no `deny_unknown_fields`, so an old client meeting the new
field ignores it. Additive in both directions, so it does not itself bump.

The CLI keeps `refuse_version_skew` (`shep-cli/src/lib.rs:1296`) unchanged. It
compares crate versions rather than the protocol integer, runs after connect,
and already names `shep daemon reload` as the remedy, which is the better error
for the ordinary upgrade case.

Two other exact-equality checks move to the floor, both in
`shep-cli/src/commands/dogs.rs`:

| Site | Today | Becomes |
|---|---|---|
| `vet_binary_within`, the check at :495, `shep adopt` | `dog != PROTOCOL_VERSION` refuses | `dog < MIN_SUPPORTED` refuses |
| `warn_of_a_dog_a_restart_would_break`, the check at :1027, `shep restart` | warns on any difference | warns only below the floor |

The second matters more than it looks. It currently fires on every dog after
every bump, which is how a real warning becomes noise people learn to skip.

### 2. Tolerant decode

`#[serde(other)]` covers two of the three shapes on this wire, and one shape
needs a wrapper. All three were measured on this tree.

| Wire type | Tagging | Fix | Measured result |
|---|---|---|---|
| `Request` | internally tagged | `#[serde(other)] Unrecognized` unit variant | absorbs unknown kinds carrying payloads, and `Envelope.id` survives |
| `RpcErrorCode`, `ProcessEventKind` | bare strings | the same `#[serde(other)]`, derive kept | unknown spelling lands on `Unrecognized`, and a number is still an error |
| `Response`, `BusEvent` | adjacently tagged | fail the caller by id, not a wrapper | see "Corrected 2026-09-07" below |

**Corrected 2026-09-07, after this spec was approved.** The row above for bare
strings first said `#[serde(other)]` was not allowed on them at all, and
prescribed a hand-written `Deserialize` instead. That was read out of serde's
documentation rather than measured, while the two rows either side of it were
measured. It is wrong: an all-unit enum with `rename_all`, no tag attribute and
`#[serde(other)]` on a unit variant decodes an unknown string to the catch-all,
round-trips every known variant, and still refuses a number. The Task 1 review
caught it after the hand-written version had shipped, and the code now uses the
attribute. Roughly seventy lines per enum, and a three-place sync burden, came
out again.

The behaviour change is on the daemon side. With the catch-all, an unrecognized
request decodes, arrives with its id intact, and is answered
`RpcError { code: Unsupported }` while the connection keeps serving. That
single change is what makes additive free, and its absence is what caused the
3 to 4 bump.

**Corrected 2026-09-07, after this spec was approved.** The `Response`/
`BusEvent` row above prescribed an `#[serde(untagged)]` wrapper at the decode
site. No such wrapper was built, and what shipped instead does not need one.
`shep-client`'s `route_frame` already decodes every server frame through
`ServerFrame`, itself `#[serde(untagged)]` over `Reply`/`Event` and marked
`#[non_exhaustive]` for exactly this growth. A frame this build cannot decode
is handled by id instead: `reply_id` reads the id straight off the raw bytes,
without decoding the frame's own type, and if that id names a pending request,
that caller fails now with `RequestError::Undecodable` rather than waiting out
its deadline for an answer that already arrived. An undecodable frame with no
id, an event nobody can name, is dropped. Neither path adds a wrapper type;
both live in `route_frame` and `reply_id` (`crates/shep-client/src/actor.rs`,
`crates/shep-core/src/protocol/wire.rs`). It is the better design: a wrapper
would still need to fail the caller by id on decode failure, so `route_frame`
would have needed writing either way, and the wrapper would have added a type
with no job beyond deciding what `route_frame` already decides.

**Ordering constraint for the implementation plan.** `RpcErrorCode`'s tolerant
`Deserialize` has to land before anything that emits the new `Unsupported`
code, because that code is itself a new variant. Telling an old client
"unsupported" in a message it cannot decode would be a poor first outing.

`ProcessEventKind`'s own comment ("A new variant is not free here") stops being
true, which is the point. Dogs are exactly the subscribers that would otherwise
break on a new process event.

### 3. `AppConfig` stops being a bump generator

`AppConfig` is `deny_unknown_fields` because a Flockfile typo should be loud.
That is right for a file and wrong for the wire, where an unknown field means a
newer peer. It rides the wire in three places, and the directions hurt
different people:

| Carrier | Direction | Who breaks on a new field |
|---|---|---|
| `Request::Start`, `Request::Add` | CLI to daemon | older daemon, already caught earlier by `refuse_version_skew` |
| `Response::SheepConfig` | daemon to client | older client, including a dog reading config |

The second row is why this cannot be left alone. Flockfile fields are added
often, so leaving it would make the most common kind of change a structural
break and quietly undo most of what sections 1 and 2 buy.

Drop `deny_unknown_fields` from `AppConfig` and from `ProbeConfig` nested
inside it. Deserialize through `serde_ignored` at the Flockfile parse site
only, collect the ignored key paths, and refuse the file naming them. The wire
path deserializes normally and ignores what it does not know.

`serde_ignored` was audited rather than assumed: 49,348,630 total downloads,
10,671,442 recent, latest 0.1.14 published 2025-09-14, repository
`dtolnay/serde-ignored`, licence MIT OR Apache-2.0 matching shep's own. It is
what cargo uses to report unused manifest keys, and it handles nesting, which a
hand rolled key list would have to grow separately for `ProbeConfig` and for
the free-form `env` map.

### 4. The rules that keep it true

| Change | Was | Becomes |
|---|---|---|
| New `Request`/`Response`/`BusEvent` variant | bumped (3 to 4) | free |
| New field on a wire struct | bumped (4 to 5) | free |
| Renamed variant tag | bumped (2 to 3) | free, via `#[serde(alias = "old")]` |
| New `RpcErrorCode` or `ProcessEventKind` | never free | free |
| Retype a variant | bumped (5 to 6, 6 to 7) | bumps, and raises `MIN_SUPPORTED` |

Prefer adding to retyping. A retype is the only change that costs every dog its
connection, so a slightly awkward shape is usually the cheaper answer. This is
a convention rather than a mechanism, and it is the one part of this design a
person can forget.

### 5. Where the floor starts

`MIN_SUPPORTED` starts at 7. PR #173 already takes `PROTOCOL_VERSION` there and
is green, and forward only means nothing below today's version was going to be
supported regardless. #173 lands unchanged, the floor starts at 7, and the
add-rather-than-retype convention applies from 8 onward. No existing work is
discarded.

## Testing

A "frames from the future" module feeding each wire type something this build
cannot know:

- a request with an invented kind carrying a payload, absorbed and answered
- a response with an invented kind carrying a payload, fails its caller by id
  with `RequestError::Undecodable` rather than absorbing it (**corrected
  2026-09-07**: see the note above)
- a bus event with an invented kind carrying a payload, dropped rather than
  absorbed, since nothing is waiting on an event nobody can name (**corrected
  2026-09-07**)
- an unknown `RpcErrorCode` spelling, absorbed
- an unknown `ProcessEventKind` spelling, absorbed
- a wrong type where a code belongs, still an error

Plus one integration test that drives the door a caller actually uses: send an
unrecognized request over a live connection, assert the daemon answers
`Unsupported`, and assert **the connection is still usable for the next
request**. A unit test on the decoder cannot see that second half, and that
second half is the whole point.

Plus a handshake test per boundary: a peer at the floor connects, a peer below
it is refused by name, and a peer above the daemon's own version connects.

## Documentation

`docs/dogs.md` and `web/src/pages/docs/dogs.astro` promise strict equality with
no window today. Both get the contract above. The `--version` contract table
changes from "it cannot connect until one side moves" to the floor rule.

`shep --version` prints both numbers, so what a build speaks and how far back
it reaches is answerable without reading source.

`CLAUDE.md`'s invariants list gains a line, since it already carries the
`PROTOCOL_VERSION` versus `SCHEMA_VERSION` distinction and this adds a third
number to keep straight.

## Costs, stated plainly

- Three decode mechanisms instead of one, forced by serde's rules.
- One new dependency in shep-core, a published crate.
- A convention that will at some point make someone accept an awkward shape
  rather than retype it.
- `MIN_SUPPORTED` is a promise, and promises need someone to notice when they
  are about to be broken. The handshake tests are that someone.

Set against six breaking bumps in a week, the trade is worth making, but it is
a trade rather than a free win.

## What this does not do

- No down conversion. An old peer is accepted, not translated for.
- No capability negotiation. Once additive changes are free and the version
  moves only on structural breaks, "daemon speaks N" already means every
  feature through N exists, and a dog checks `HelloAck.protocol >= N`. Nothing
  in shep today can have a protocol feature present in one build and absent in
  another at the same version. `Hello` and `HelloAck` both tolerate unknown
  fields, so a capability list can be added additively if that ever changes.
- No change to `SCHEMA_VERSION`, which answers a different question about JSON
  output and is untouched here.

## Decisions

Recorded so a later reader can tell what was chosen from what was never
considered.

1. **The promise is "additive changes are free"**, not "N versions back".
   Supporting a range through a retype needs two live encode paths, kept and
   tested until the window closes, and someone to delete them.
2. **The handshake takes a floor**, rather than keeping exact equality and
   bumping less often. Bumping less often still refuses every dog when a break
   lands.
3. **Forward only, no down conversion.** The floor starts at today's version
   and the promise runs forward from it.
4. **`serde_ignored` rather than a hand rolled key check**, because nesting is
   where a hand rolled one stops being under a hundred lines.
5. **The version number is the capability check.** A capability list is YAGNI
   until something can be present at one build and absent at another.
