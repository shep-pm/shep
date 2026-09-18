# Protocol compatibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make additive protocol changes stop breaking every connected peer, so `PROTOCOL_VERSION` moves once or twice a year instead of six times in a week.

**Architecture:** Three independent mechanisms. The handshake takes a floor (`MIN_SUPPORTED`) instead of demanding exact equality. Every wire enum gains a way to absorb a variant it has never heard of, using whichever of three serde techniques its tagging allows. `AppConfig` stops denying unknown fields on the wire while keeping typo detection when parsing a Flockfile.

**Tech Stack:** Rust 1.88, edition 2024, serde + serde_json, one new dependency (`serde_ignored`).

**Spec:** [docs/brainstorming/specs/2026-09-07-protocol-compatibility-design.md](../../brainstorming/specs/2026-09-07-protocol-compatibility-design.md)

## Global Constraints

- `PROTOCOL_VERSION` stays at **7**. Nothing in this plan bumps it. `MIN_SUPPORTED` is introduced at **7**.
- `SCHEMA_VERSION` is untouched. It answers a different question about JSON output.
- Nothing here may break a peer built at protocol 7. Every change is additive or strictly more tolerant. If a task appears to require a break, stop and report rather than bumping.
- Conventional commit subjects, `type(scope): summary`, with `!` only on a commit that actually breaks something. No commit in this plan should need a `!`.
- Invoke the `shep-idiomatic-rust` skill before writing Rust. Cite `IR-<n>` where a rule applies.
- Every new public item needs docs and a deliberate `Debug` decision.
- One cargo shape for the whole plan: `--workspace`. Do not alternate with `-p <crate>`; it churns the build cache badly in this repo.
- Iterate with `cargo test --workspace --all-features --lib --bins`. Run the full `cargo test --workspace --all-features` once per task, before committing.
- A new `RpcErrorCode` variant requires updating in-crate exhaustive matches. The enum is `#[non_exhaustive]`, so out-of-crate matches already carry a `_` arm.

---

## File Structure

| File | Responsibility in this plan |
|---|---|
| `crates/shep-core/src/protocol/request.rs` | `RpcErrorCode` tolerance, `Request::Unrecognized`, `RpcErrorCode::Unsupported`, `MIN_SUPPORTED` re-export surface, `HelloAck.min_supported` |
| `crates/shep-core/src/protocol/events.rs` | `ProcessEventKind` tolerance |
| `crates/shep-core/src/protocol/mod.rs` | `MIN_SUPPORTED` constant |
| `crates/shep-daemon/src/server.rs` | handshake floor, answering an unrecognized request |
| `crates/shep-client/src/actor.rs` | turning an undecodable reply into a named error instead of a silent hang |
| `crates/shep-core/src/config/app.rs` | drop `deny_unknown_fields` from `AppConfig` and `ProbeConfig` |
| `crates/shep-core/src/config/flockfile.rs` | `serde_ignored` at the Flockfile parse site |
| `crates/shep-cli/src/commands/dogs.rs` | two exact-equality checks move to the floor |
| `docs/dogs.md`, `web/src/pages/docs/dogs.astro`, `CLAUDE.md` | the published contract |

---

## Task 1: Tolerant decode for the two bare-string enums

`RpcErrorCode` and `ProcessEventKind` are all-unit enums that serialize as bare strings. Each gains an `Unrecognized` variant carrying `#[serde(other)]`, keeping the ordinary derive. This task lands first because every later task benefits and nothing else can safely add a variant until it does.

**Corrected 2026-09-07, mid-execution.** This task originally prescribed a hand-written `Deserialize` for each enum, on the stated grounds that `#[serde(other)]` is not allowed on a bare-string enum. That was read out of serde's documentation rather than measured, and it is wrong. The review caught it after the hand-written version shipped; commit 6eb653d0 replaced roughly seventy lines per enum with the attribute. Step 3 below describes what the code does now.

**Files:**
- Modify: `crates/shep-core/src/protocol/request.rs` (`RpcErrorCode`, around line 1637)
- Modify: `crates/shep-core/src/protocol/events.rs` (`ProcessEventKind`, around line 15)

**Interfaces:**
- Consumes: nothing.
- Produces: `RpcErrorCode::Unrecognized` and `ProcessEventKind::Unrecognized`, unit variants nothing constructs to send. `#[serde(other)]` governs decoding only, so each still has a serialized spelling; the invariant is held at the call sites. Later tasks rely on unknown spellings decoding rather than erroring.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-core/src/protocol/request.rs`, inside its existing `mod tests`:

```rust
/// A code this build has never heard of must decode, not fail. Without
/// this, adding any error code is a breaking change for every peer.
#[test]
fn an_unknown_error_code_decodes_as_unrecognized() {
    assert_eq!(
        serde_json::from_str::<RpcErrorCode>(r#""invented_next_year""#).unwrap(),
        RpcErrorCode::Unrecognized
    );
}

#[test]
fn every_known_error_code_still_round_trips() {
    for code in [
        RpcErrorCode::NotFound,
        RpcErrorCode::InvalidConfig,
        RpcErrorCode::SpawnFailed,
        RpcErrorCode::ProtocolMismatch,
        RpcErrorCode::Internal,
        RpcErrorCode::DeadlineExceeded,
    ] {
        let json = serde_json::to_string(&code).unwrap();
        assert_eq!(serde_json::from_str::<RpcErrorCode>(&json).unwrap(), code);
    }
}

/// The fallback absorbs an unknown STRING, not an unknown TYPE. A number
/// where a code belongs is still a defect worth reporting.
#[test]
fn a_non_string_error_code_is_still_an_error() {
    assert!(serde_json::from_str::<RpcErrorCode>("42").is_err());
}
```

In `crates/shep-core/src/protocol/events.rs`, inside its existing `mod tests`, the same three shapes for `ProcessEventKind`, using its own variants (`Start`, `Online`, `Exit`, `Restart`, `Reload`, `Reloaded`, `ReloadAbandoned`, `Stop`, `Delete`, `Errored`):

```rust
/// The comment above this enum said a new variant is not free because
/// there is no fallback. This is the fallback.
#[test]
fn an_unknown_process_event_decodes_as_unrecognized() {
    assert_eq!(
        serde_json::from_str::<ProcessEventKind>(r#""invented_next_year""#).unwrap(),
        ProcessEventKind::Unrecognized
    );
}

#[test]
fn every_known_process_event_still_round_trips() {
    for kind in [
        ProcessEventKind::Start,
        ProcessEventKind::Online,
        ProcessEventKind::Exit,
        ProcessEventKind::Restart,
        ProcessEventKind::Reload,
        ProcessEventKind::Reloaded,
        ProcessEventKind::ReloadAbandoned,
        ProcessEventKind::Stop,
        ProcessEventKind::Delete,
        ProcessEventKind::Errored,
    ] {
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(serde_json::from_str::<ProcessEventKind>(&json).unwrap(), kind);
    }
}

#[test]
fn a_non_string_process_event_is_still_an_error() {
    assert!(serde_json::from_str::<ProcessEventKind>("42").is_err());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace --all-features --lib --bins -- unknown_error_code unknown_process_event`
Expected: FAIL, no variant named `Unrecognized`.

- [ ] **Step 3: Add the catch-all to `RpcErrorCode`**

Keep the derive exactly as it is. Add one variant.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RpcErrorCode {
    // ... existing variants unchanged ...
    /// A code this build has not been taught.
    ///
    /// Only ever produced by decoding. Nothing constructs one to send,
    /// which is a call-site invariant: `#[serde(other)]` governs decoding
    /// only, so serializing this would emit `"unrecognized"`.
    #[serde(other)]
    Unrecognized,
}
```

Leave `RpcErrorCode::ALL` alone. It must NOT list `Unrecognized`, because
`exit.rs`'s `From<RpcErrorCode> for ExitCode` maps unknown codes onto `Internal`
through a wildcard, and adding it would collide there and break
`every_rpc_error_code_maps_to_a_distinct_nonzero_exit_code`.

- [ ] **Step 4: Add the same catch-all to `ProcessEventKind`**

Identical shape in `events.rs`: keep the derive, add an `Unrecognized` variant
carrying `#[serde(other)]`, with the same doc reasoning.

Then delete the now-false comment above the enum (currently lines 13-14: "A new
variant is not free here: there is no `#[serde(other)]` fallback, so an old
subscriber is sent a frame under `process.*` it cannot decode") and replace it
with one saying a new variant IS free for a subscriber built from this point on.

- [ ] **Step 5: Fix in-crate exhaustive matches**

Run: `cargo build --workspace --all-features 2>&1 | grep -A 5 'non-exhaustive patterns'`
Add an arm for `Unrecognized` wherever the compiler asks. For `RpcErrorCode`, treat it as `Internal` would be treated. For `ProcessEventKind`, it should be ignored rather than acted on.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --workspace --all-features --lib --bins -- unknown_error_code unknown_process_event round_trips non_string`
Expected: PASS, six tests.

- [ ] **Step 7: Full gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

```bash
git add crates/shep-core/src/protocol/request.rs crates/shep-core/src/protocol/events.rs
git commit -m "feat(core): decode an unknown error code or process event instead of failing"
```

---

## Task 2: An unrecognized request is answered, not fatal

**Files:**
- Modify: `crates/shep-core/src/protocol/request.rs` (`Request` enum at line 191, `RpcErrorCode`)
- Modify: `crates/shep-daemon/src/rpc.rs` (request dispatch)
- Test: `crates/shep-daemon/tests/daemon_e2e.rs`

**Interfaces:**
- Consumes: Task 1's tolerant `RpcErrorCode`.
- Produces: `Request::Unrecognized` (unit, decode-only) and `RpcErrorCode::Unsupported`.

- [ ] **Step 1: Write the failing unit test**

In `crates/shep-core/src/protocol/request.rs` tests:

```rust
/// The id has to survive a body this build cannot name, or the daemon
/// has nothing to address a refusal to.
#[test]
fn an_unknown_request_kind_keeps_the_envelope_id() {
    let envelope: Envelope = serde_json::from_str(
        r#"{"id":42,"deadline_ms":null,"body":{"kind":"from_the_future","extra":{"a":1}}}"#,
    )
    .unwrap();
    assert_eq!(envelope.id, 42);
    assert_eq!(envelope.body, Request::Unrecognized);
}
```

- [ ] **Step 2: Write the failing integration test**

In `crates/shep-daemon/tests/daemon_e2e.rs`, following the existing handshake helpers in that file:

```rust
/// The whole point of the catch-all. An unknown request must be refused
/// BY ID and leave the connection usable, because the alternative is the
/// dropped connection that made every additive change a version bump.
#[tokio::test]
async fn an_unrecognized_request_is_refused_and_the_connection_survives() {
    // Use the same daemon + handshake setup the neighbouring tests use.
    // Send a raw frame with an invented kind, then a normal Ping on the
    // SAME connection.
    let refusal = send_raw_and_read(
        &mut conn,
        r#"{"id":1,"deadline_ms":null,"body":{"kind":"from_the_future"}}"#,
    )
    .await;
    assert_eq!(refusal.id, 1);
    let Err(err) = refusal.result else {
        panic!("an unknown request must be refused");
    };
    assert_eq!(err.code, RpcErrorCode::Unsupported);

    let pong = request(&mut conn, 2, Request::Ping).await;
    assert!(
        pong.result.is_ok(),
        "the connection must still serve after refusing one request"
    );
}
```

- [ ] **Step 3: Run both to verify they fail**

Run: `cargo test --workspace --all-features -- unknown_request_kind unrecognized_request_is_refused`
Expected: FAIL, no `Request::Unrecognized`, no `RpcErrorCode::Unsupported`.

- [ ] **Step 4: Add the catch-all variant and the error code**

`Request` is internally tagged, which is the one tagging where `#[serde(other)]` absorbs a variant carrying a payload.

```rust
pub enum Request {
    // ... existing variants unchanged ...
    /// A request kind this build has not been taught.
    ///
    /// `#[serde(other)]`, which serde allows here because `Request` is
    /// internally tagged and this variant carries nothing. The unknown
    /// body's own fields are discarded: the only thing to do with a
    /// request we cannot name is refuse it, and the refusal needs the
    /// envelope's id rather than the body.
    #[serde(other)]
    Unrecognized,
}
```

Add to `RpcErrorCode`, remembering to add it to Task 1's `Known` enum and match:

```rust
    /// The peer asked for something this build does not implement.
    ///
    /// Distinct from `NotFound`, which means a selector matched nothing.
    /// This means the verb itself is unknown here, and the remedy is a
    /// newer shepherd rather than a different selector.
    Unsupported,
```

- [ ] **Step 5: Answer it in the daemon**

In `crates/shep-daemon/src/rpc.rs`, in the dispatch that matches on the decoded `Request`, add the arm. Do not touch the `?` at the `decode_frame` call in `server.rs`: with the catch-all, an unknown kind no longer reaches it as an error, and that path should still end the connection for a genuinely malformed frame.

**Corrected 2026-09-07, mid-execution.** This step first named `server.rs` for the dispatch, which holds the handshake but not the request match, and first set `daemon_version: Some(...)` on the arm below. Both were defects in this plan. `RpcError::daemon_version` is documented as set only on a `ProtocolMismatch` refusal, because that is the one path where `HelloAck` never reaches the client; `Unsupported` is answered after a successful handshake, so the client already holds the version. The snippet also read it from `env!("CARGO_PKG_VERSION")`, shep-daemon's own version, where the real producer uses `ctx.daemon_version`.

```rust
        Request::Unrecognized => Outcome::Reply(Reply {
            id,
            result: Err(RpcError {
                code: RpcErrorCode::Unsupported,
                message: format!(
                    "this shepherd speaks protocol {PROTOCOL_VERSION} and does not \
                     implement the request the client sent"
                ),
                daemon_version: None,
            }),
        }),
```

- [ ] **Step 6: Run to verify they pass**

Run: `cargo test --workspace --all-features -- unknown_request_kind unrecognized_request_is_refused`
Expected: PASS.

- [ ] **Step 7: Full gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

```bash
git add crates/shep-core/src/protocol/request.rs crates/shep-daemon/src/server.rs crates/shep-daemon/tests/daemon_e2e.rs
git commit -m "feat(core): refuse an unrecognized request by id instead of ending the connection"
```

---

## Task 3: An undecodable reply becomes a named error, not a silent hang

`crates/shep-client/src/actor.rs:170` already survives a bad frame:

```rust
let Ok(frame) = decode_frame::<ServerFrame>(bytes) else {
    return;
};
```

The connection lives, but the frame is dropped silently. For a `Reply` that means the caller's oneshot never fires and it waits out its deadline with no explanation. That is the failure this task replaces.

**Files:**
- Modify: `crates/shep-client/src/actor.rs` (`route_frame`, around line 165)
- Test: `crates/shep-client/src/actor.rs` tests

**Interfaces:**
- Consumes: Task 1 and Task 2.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Write the failing test**

```rust
/// A reply this build cannot decode used to be dropped, leaving the
/// caller to wait out its deadline for an answer that had already
/// arrived. It must fail the caller by id instead.
#[test]
fn an_undecodable_reply_fails_its_caller_by_id_rather_than_hanging() {
    let (tx, rx) = oneshot::channel();
    let mut pending = HashMap::from([(7_u64, tx)]);
    let (events, _) = broadcast::channel(4);

    route_frame(
        br#"{"id":7,"result":{"Ok":{"kind":"from_the_future","data":{"a":1}}}}"#,
        &mut pending,
        &events,
    );

    let got = rx.now_or_never().expect("the caller must be answered now");
    assert!(matches!(got, Ok(Err(RequestError::Rpc(_)))));
    assert!(pending.is_empty(), "the pending entry must be cleared");
}

/// An event this build cannot decode is still just dropped. Nothing is
/// waiting on it, and a subscriber that cannot name it cannot act on it.
#[test]
fn an_undecodable_event_is_dropped_without_disturbing_the_connection() {
    let mut pending = HashMap::new();
    let (events, mut sub) = broadcast::channel(4);

    route_frame(
        br#"{"event":"from_the_future","data":{"a":1}}"#,
        &mut pending,
        &events,
    );

    assert!(sub.try_recv().is_err(), "nothing decodable to deliver");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --workspace --all-features --lib -- undecodable_reply undecodable_event`
Expected: FAIL, the caller is never answered.

- [ ] **Step 3: Implement the wrapper**

`ServerFrame` is `#[serde(untagged)]` already, and `Response` and `BusEvent` are adjacently tagged, where `#[serde(other)]` cannot absorb a payload. Decode the id separately so a reply can be failed by id.

```rust
/// Enough of a `Reply` to answer its caller when the `Response` itself
/// does not decode. `result` is deliberately `IgnoredAny`: this exists to
/// recover the id, not the payload.
#[derive(serde::Deserialize)]
struct ReplyIdOnly {
    id: u64,
}

fn route_frame(
    bytes: &[u8],
    pending: &mut HashMap<u64, oneshot::Sender<Result<Response, RequestError>>>,
    events: &broadcast::Sender<BusEvent>,
) {
    let Ok(frame) = decode_frame::<ServerFrame>(bytes) else {
        // Not a frame this build can name. If it carries an id, some
        // caller is waiting on it, and a named error beats a silent wait
        // to the deadline. If it does not, it is an event nothing can act
        // on, and dropping it is correct.
        if let Ok(ReplyIdOnly { id }) = decode_frame::<ReplyIdOnly>(bytes) {
            if let Some(reply_to) = pending.remove(&id) {
                let _ = reply_to.send(Err(RequestError::Rpc(RpcError {
                    code: RpcErrorCode::Unsupported,
                    message: "the shepherd answered with something this build \
                              cannot decode; it is newer than this client"
                        .to_string(),
                    daemon_version: None,
                })));
            }
        }
        return;
    };
    // ... existing match unchanged ...
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --workspace --all-features --lib -- undecodable_reply undecodable_event`
Expected: PASS.

- [ ] **Step 5: Full gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

```bash
git add crates/shep-client/src/actor.rs
git commit -m "fix(client): fail a caller whose reply cannot be decoded instead of leaving it waiting"
```

---

## Task 4: The handshake takes a floor

**Files:**
- Modify: `crates/shep-core/src/protocol/mod.rs` (`PROTOCOL_VERSION`, line 48-50)
- Modify: `crates/shep-core/src/protocol/request.rs` (`HelloAck`, around line 36)
- Modify: `crates/shep-daemon/src/server.rs:406`
- Test: `crates/shep-daemon/tests/daemon_e2e.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `shep_core::protocol::MIN_SUPPORTED: u32` and `HelloAck.min_supported: Option<u32>`.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-daemon/tests/daemon_e2e.rs`. Note the existing `protocol_skew_is_refused_over_the_real_socket` (line 1137, sending `PROTOCOL_VERSION + 1` at line 1147) expects a refusal. It must be updated in Step 4, not deleted, since a newer peer is now accepted. Its name stops being true, so rename it as part of the change.

```rust
#[tokio::test]
async fn a_peer_at_the_floor_is_accepted() {
    let ack = handshake_with_protocol(MIN_SUPPORTED).await.expect("at the floor");
    assert_eq!(ack.protocol, PROTOCOL_VERSION);
    assert_eq!(ack.min_supported, Some(MIN_SUPPORTED));
}

#[tokio::test]
async fn a_peer_below_the_floor_is_refused_by_name() {
    let err = handshake_with_protocol(MIN_SUPPORTED - 1)
        .await
        .expect_err("below the floor");
    assert_eq!(err.code, RpcErrorCode::ProtocolMismatch);
}

/// A dog rebuilt against a newer shep-client than the running shepherd.
/// Refusing it bought nothing: anything it asks for that does not exist
/// is refused per request by `Request::Unrecognized`.
#[tokio::test]
async fn a_peer_above_the_daemons_own_version_is_accepted() {
    let ack = handshake_with_protocol(PROTOCOL_VERSION + 1)
        .await
        .expect("a newer peer connects");
    assert_eq!(ack.protocol, PROTOCOL_VERSION);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --workspace --all-features -- at_the_floor below_the_floor above_the_daemons_own_version`
Expected: FAIL, no `MIN_SUPPORTED`.

- [ ] **Step 3: Add the constant**

In `crates/shep-core/src/protocol/mod.rs`, beside `PROTOCOL_VERSION`:

```rust
/// The oldest protocol this build accepts from a peer.
///
/// The handshake compares against this rather than [`PROTOCOL_VERSION`],
/// so a change that only adds does not refuse anyone. This rises only
/// when a message shape changes such that an older peer cannot read it,
/// and raising it refuses every peer built below it, which is why the
/// rules in the spec exist to make that rare.
pub const MIN_SUPPORTED: u32 = 7;
```

Add a test in the same file asserting `MIN_SUPPORTED <= PROTOCOL_VERSION`, which is a real invariant and cheap to pin.

- [ ] **Step 4: Change the comparison and the ack**

`crates/shep-daemon/src/server.rs:406`:

```rust
    if hello.protocol < MIN_SUPPORTED {
```

Update the refusal message to name the window rather than one number:

```rust
                "client sent protocol {}, daemon speaks {PROTOCOL_VERSION} and accepts {MIN_SUPPORTED} and above",
                hello.protocol
```

`HelloAck` gains:

```rust
    /// The oldest protocol this daemon accepts, or `None` from a daemon
    /// predating the floor.
    ///
    /// Absent rather than `null` on the wire, so it does not move
    /// [`crate::protocol::PROTOCOL_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_supported: Option<u32>,
```

`Option` makes the field optional ON THE WIRE. It does not make the Rust field
optional at construction, and `HelloAck` is built as a literal in a good many
places across the workspace and its test fixtures. Every one needs
`min_supported: None` added or the branch will not compile. Thirteen did when
this task ran.

Update `protocol_skew_is_refused_over_the_real_socket` (`daemon_e2e.rs:1137`) to assert acceptance, rename it to match, and say in its doc comment why the expectation reversed. A test whose name asserts the opposite of its body is worse than no test.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test --workspace --all-features -- at_the_floor below_the_floor above_the_daemons_own_version`
Expected: PASS.

- [ ] **Step 6: Full gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

```bash
git add crates/shep-core/src/protocol/mod.rs crates/shep-core/src/protocol/request.rs crates/shep-daemon/src/server.rs crates/shep-daemon/tests/daemon_e2e.rs
git commit -m "feat(core): accept any peer at or above a protocol floor"
```

---

## Task 5: The two dog checks move to the floor

**Files:**
- Modify: `crates/shep-cli/src/commands/dogs.rs:495` (`shep adopt`) and `:1027` (`shep restart`)
- Test: same file's `mod tests`, which already has fixtures at lines 1983 and 2222 using `PROTOCOL_VERSION + 1`

**Interfaces:**
- Consumes: `MIN_SUPPORTED` from Task 4.
- Produces: nothing.

- [ ] **Step 1: Write the failing tests**

```rust
/// A dog built against a NEWER protocol than this shep is adoptable. The
/// shepherd's own handshake accepts it, so refusing it here would refuse
/// a dog that works.
#[test]
fn a_dog_above_this_protocol_is_adopted() {
    let answer = VersionAnswer { protocol: Some(PROTOCOL_VERSION + 1), ..fixture() };
    assert!(vet_answer(&answer).is_ok());
}

#[test]
fn a_dog_below_the_floor_is_still_refused() {
    let answer = VersionAnswer { protocol: Some(MIN_SUPPORTED - 1), ..fixture() };
    assert!(matches!(vet_answer(&answer), Err(AdoptRefusal::ProtocolMismatch { .. })));
}

/// The restart warning fired on every dog after every bump, which is how
/// a real warning becomes noise. It should fire only when the dog cannot
/// actually connect.
#[test]
fn no_restart_warning_for_a_dog_inside_the_window() {
    assert!(warn_of_a_dog_a_restart_would_break(&[dog_at(PROTOCOL_VERSION + 1)]).is_none());
    assert!(warn_of_a_dog_a_restart_would_break(&[dog_at(MIN_SUPPORTED)]).is_none());
    assert!(warn_of_a_dog_a_restart_would_break(&[dog_at(MIN_SUPPORTED - 1)]).is_some());
}
```

Adapt the helper names to what the file already uses; grep rather than assuming these exist.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --workspace --all-features --lib -- dog_above_this_protocol dog_below_the_floor no_restart_warning`
Expected: FAIL.

- [ ] **Step 3: Change both comparisons**

Line 495, `&& dog != PROTOCOL_VERSION` becomes `&& dog < MIN_SUPPORTED`.
Line 1027, `if disk == PROTOCOL_VERSION { continue }` becomes `if disk >= MIN_SUPPORTED { continue }`.

Update both user-facing messages to name the floor rather than the exact version, and read them aloud for the voice rules before committing.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --workspace --all-features --lib -- dog_above_this_protocol dog_below_the_floor no_restart_warning`
Expected: PASS.

- [ ] **Step 5: Full gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

```bash
git add crates/shep-cli/src/commands/dogs.rs
git commit -m "fix(shep): judge a dog's protocol against the floor rather than exact equality"
```

---

## Task 6: `AppConfig` stops being a bump generator

**Files:**
- Modify: `crates/shep-core/Cargo.toml` (add `serde_ignored`)
- Modify: `Cargo.toml` (workspace dependency entry)
- Modify: `crates/shep-core/src/config/app.rs:73` (`AppConfig`) and `:30-31` (`ProbeConfig`)
- Modify: `crates/shep-core/src/config/flockfile.rs:218` (`Flockfile::parse`)

**Interfaces:**
- Consumes: nothing.
- Produces: `FlockfileError::UnknownKeys { keys: Vec<String> }`.

- [ ] **Step 1: Write the failing tests**

```rust
/// A typo in a Flockfile must still be loud. This is the whole reason
/// `deny_unknown_fields` was there.
#[test]
fn a_misspelled_flockfile_key_is_refused_and_named() {
    let err = Flockfile::parse(
        "[[app]]\nname = \"web\"\nscript = \"./srv\"\nmax_restrts = 5\n",
        FlockFormat::Toml,
    )
    .expect_err("a typo must be refused");
    let FlockfileError::UnknownKeys { keys } = err else {
        panic!("expected UnknownKeys, got {err:?}");
    };
    assert!(keys.iter().any(|k| k.contains("max_restrts")), "got {keys:?}");
}

/// Nesting is why this uses serde_ignored rather than a key list.
#[test]
fn a_misspelled_key_inside_a_probe_is_also_named() {
    let err = Flockfile::parse(
        "[[app]]\nname = \"web\"\nscript = \"./srv\"\n[app.readiness_probe]\ntimeuot = \"5s\"\n",
        FlockFormat::Toml,
    )
    .expect_err("a nested typo must be refused");
    assert!(matches!(err, FlockfileError::UnknownKeys { .. }));
}

/// The wire path is the opposite: an unknown field means a newer peer,
/// and ignoring it is what stops a new Flockfile field breaking an older
/// client that reads a config off the wire.
#[test]
fn an_unknown_field_on_the_wire_is_ignored_rather_than_refused() {
    let config: AppConfig = serde_json::from_str(
        r#"{"name":"web","script":"./srv","invented_next_year":true}"#,
    )
    .expect("the wire path tolerates what it does not know");
    assert_eq!(config.name, "web");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --workspace --all-features --lib -- misspelled_flockfile_key misspelled_key_inside_a_probe unknown_field_on_the_wire`
Expected: FAIL. The wire test fails on `deny_unknown_fields`; the others fail because `UnknownKeys` does not exist.

- [ ] **Step 3: Add the dependency**

Workspace `Cargo.toml`:

```toml
serde_ignored = "0.1.14"
```

`crates/shep-core/Cargo.toml`:

```toml
serde_ignored = { workspace = true }
```

Run `cargo deny check` afterwards, since the licence allowlist gates new dependencies. `serde_ignored` is MIT OR Apache-2.0, which the list already carries.

- [ ] **Step 4: Drop the denial and add the parse-site check**

Remove `deny_unknown_fields` from both `AppConfig` (keep `default`) and `ProbeConfig`. In each case replace the attribute with a comment saying the denial moved to `Flockfile::parse` and why.

In `flockfile.rs`, wrap the existing `parse_into::<RawFlockfile>` call:

```rust
    pub fn parse(source: &str, format: FlockFormat) -> Result<Self, FlockfileError> {
        // `deny_unknown_fields` used to live on `AppConfig` itself, which
        // made every new Flockfile field a protocol event: the same type
        // rides the wire, where an unknown field means a newer peer
        // rather than a typo. The denial belongs here, where the input
        // really is a hand-written file.
        let mut unknown = Vec::new();
        let raw: RawFlockfile = parse_into_ignoring(source, format, |path| {
            unknown.push(path.to_string());
        })?;
        if !unknown.is_empty() {
            return Err(FlockfileError::UnknownKeys { keys: unknown });
        }
        // ... existing destructure and validation unchanged ...
    }
```

Write `parse_into_ignoring` beside the existing `parse_into`, mirroring its per-format dispatch and routing each format's `Deserializer` through `serde_ignored::deserialize`. Read `parse_into` first; it handles both TOML and JSON, and both need the same treatment.

Add the error variant with a `Display` naming every key, since one message listing all typos beats one refusal per run.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test --workspace --all-features --lib -- misspelled_flockfile_key misspelled_key_inside_a_probe unknown_field_on_the_wire`
Expected: PASS.

- [ ] **Step 6: Full gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

```bash
git add Cargo.toml Cargo.lock crates/shep-core/Cargo.toml crates/shep-core/src/config/app.rs crates/shep-core/src/config/flockfile.rs
git commit -m "feat(core): deny unknown Flockfile keys at the parse site, not on the wire"
```

---

## Task 7: The operator surface and the published contract

**Files:**
- Modify: `crates/shep-cli/src/cli.rs` (version output)
- Modify: `docs/dogs.md`, `web/src/pages/docs/dogs.astro`, `CLAUDE.md`
- Modify: `web/src/data/cli-reference.generated.txt` (regenerated, never hand-edited)

**Interfaces:**
- Consumes: `MIN_SUPPORTED` from Task 4.
- Produces: nothing.

- [ ] **Step 1: Write the failing test**

```rust
/// An operator asking what a build speaks should not have to read
/// source to learn how far back it reaches.
#[test]
fn version_output_names_both_the_protocol_and_the_floor() {
    let text = version_text();
    assert!(text.contains(&format!("protocol {PROTOCOL_VERSION}")), "got {text}");
    assert!(text.contains(&format!("accepts {MIN_SUPPORTED}")), "got {text}");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --workspace --all-features --lib -- version_output_names_both`
Expected: FAIL.

- [ ] **Step 3: Print both numbers**

Extend the version output to name the protocol and the floor. Keep the existing crate-version line first; this is an addition below it, not a replacement.

- [ ] **Step 4: Rewrite the published contract**

`docs/dogs.md` and `web/src/pages/docs/dogs.astro` currently promise strict equality. Both carry this sentence or a close variant, which is now false:

> "PROTOCOL_VERSION covers a dog exactly as it covers every other client: a version mismatch is a typed error at handshake, not silence."

Replace with the spec's contract: shep accepts any client at or above `MIN_SUPPORTED`, and `PROTOCOL_VERSION` moves only when a message shape changes in a way an older peer cannot read. Say plainly that it is forward only and that the floor rising refuses older peers, because a promise an operator misreads is worse than none.

The `--version` contract table's row "it cannot connect until one side moves" becomes the floor rule.

`CLAUDE.md`'s invariants list gains one line next to the existing `PROTOCOL_VERSION` versus `SCHEMA_VERSION` entry, naming `MIN_SUPPORTED` as the third number.

**These are prose read by people. Run `humanizer`, then `rin-voice`, before committing.**

- [ ] **Step 5: Regenerate and build the site**

```bash
cargo build --release
```
```bash
./web/scripts/generate-cli-reference.sh
```
```bash
cd web && npx astro build
```
```bash
cd web && npx astro check
```

`git diff` on the generated reference is the check that the version output really changed.

- [ ] **Step 6: Run to verify it passes**

Run: `cargo test --workspace --all-features --lib -- version_output_names_both`
Expected: PASS.

- [ ] **Step 7: Full gate and commit**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
```

Two commits, since these are two concerns:

```bash
git add crates/shep-cli/src/cli.rs web/src/data/cli-reference.generated.txt
git commit -m "feat(shep): name the protocol floor in version output"
git add docs/dogs.md web/src/pages/docs/dogs.astro CLAUDE.md
git commit -m "docs(shep): publish the protocol floor as the compatibility contract"
```

---

## Self-review

**Spec coverage.** Every section maps to a task: contract to Task 7, handshake floor to Task 4, tolerant decode to Tasks 1 through 3, `AppConfig` to Task 6, dog checks to Task 5, rules and docs to Task 7. The spec's "frames from the future" test module is distributed into the task that introduces each mechanism rather than collected into one, because a test landing with its own change is what makes a task independently reviewable.

**Ordering.** Task 1 precedes Task 2, since `RpcErrorCode::Unsupported` is added in Task 2 and its tolerance in Task 1. Task 4 precedes Task 5, which consumes `MIN_SUPPORTED`. Task 3 reads better after Task 2 but does not depend on it. Tasks 6 and 7 are independent of everything except Task 4 for the constant.

**One thing the spec got slightly wrong, corrected here.** The spec says an undecodable response is fatal to the connection. It is not: `actor.rs:170` already drops the frame and returns. The real defect is a silent drop that leaves the caller waiting out its deadline, which is what Task 3 fixes and why its test asserts the caller is answered rather than that the connection survives.

**No placeholders.** Every code step carries the code. Two places direct the implementer to read neighbouring code first (`parse_into` in Task 6, the test helper names in Task 5) rather than inventing signatures, because those helpers exist and a guessed signature would be worse than a grep.
