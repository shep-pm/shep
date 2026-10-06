# Ask the operator, PR 1: wire, shepherd, `shep answer` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A sheep sends `ask` and `withdraw` over its shepherd channel, the shepherd holds the open questions and carries them across a handover, and `shep answer` (or any client sending `Request::Answer`) delivers an `answer` message back.

**Architecture:** The question grammar lives in shep-channel beside the other wire types, so an app, a dog and the shepherd share one checker. The shepherd keeps a per-process store on each `SheepSlot`, fills `ProcessInfo::questions` from `Actor::snapshot_all`, and publishes `question.settled` when one closes. `shep answer` is a new verb that lists open questions with no arguments and answers one with them.

**Tech Stack:** Rust 2024, MSRV 1.88, serde, tokio, clap, insta.

**Spec:** `docs/brainstorming/specs/2026-10-05-ask-the-operator-design.md`. Read it before any task. This plan covers its slice 1 only. Lookout (slice 2) and bark (slice 3) are later PRs.

## Global Constraints

- Commit subjects are conventional: `type(scope): summary`, types `feat fix perf refactor docs test ci chore style`. `!` goes on the commit that breaks Rust callers (Task 1, `feat(channel)!`). Every commit message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Invoke the `rust-house-style:rust-house-style` skill before writing Rust, and read `docs/rust-house-style-addendum.md`. Most common drift: panicking constructors, `std::error::Error` instead of `core::error::Error`, missing `# Errors`, comments that narrate history.
- Wire keys, verbatim: child kinds `ask` and `withdraw`; shepherd kind `answer`; fields `question`, `text`, `takes`, `answer`, `note`, `via`, `who`. `takes` values `yes-no` and `text`. Bus topic `question.settled`. Settled kinds `answered`, `withdrawn`, `gone`.
- Limits, verbatim: question id 1 to 32 characters from `[A-Za-z0-9._-]`; text 1 to 1000 characters, newline allowed, any other control character refused; a `text` answer 1 to 1000 characters with the same rule; a `yes-no` answer exactly `yes` or `no`; note at most 500 characters, no control character, only on a `yes-no` answer; `via` at most 64 characters and `who` at most 128, no control character. 64 open questions per process, and the 65th is dropped, not an older one. The last 64 settled questions per process are remembered for `not open` messages, in memory only, never carried across a handover.
- `PROTOCOL_VERSION` stays 11 and `MIN_SUPPORTED` stays 8. `Request`, `Response` and `BusEvent` are `#[non_exhaustive]`, and new variants there keep the version (`crates/shep-core/src/protocol/mod.rs:57-68`). `CHANNEL_VERSION` stays `"1"`. `SCHEMA_VERSION` does not move: every JSON addition is a new optional key.
- No hand edits to any `CHANGELOG.md`: release-plz writes them.
- IR-48: no `.rs` file over 1000 lines. A change that takes a file past 500 weighs a split, and the task report says which option it took.
- One cargo shape per task, named in the task. Never alternate `--workspace` with `-p`. Iterate with `--lib --bins` or `--lib`, not bare `cargo test`, which also runs doctests.
- Terminology: a sheep, the flock, the shepherd (the daemon only). Error text stays plain.

## Review Focus

1. **A sheep that exits while a question is open.** The question must disappear from `describe`, a `question.settled` with `gone` must publish, and a late `shep answer` must exit 3 rather than write to a dead channel. Pinned in Task 4.
2. **A restart that reuses the slot before the old process's `ask` reaches the actor.** The `ask` is keyed by the sending process's pid and dropped when the slot's pid has moved on, the way `Msg::LambLabel` is keyed. Pinned in Task 4.
3. **Two instances of one app asking the same question id.** `shep answer web q1 yes` must refuse with exit 4 and name both ids, never answer the first one it happens to find. Pinned in Task 4.
4. **`shep daemon reload` with a question open.** The successor must still list it and still deliver an answer to it. Pinned in Task 5.
5. **An answer typed with shell quoting surprises:** `shep answer web q1 no "rebase first"` and `shep answer web q1 no rebase first` must deliver the same note, and `shep answer web q2 no thanks` on a `text` question delivers the text `no thanks`, not a `no` with a note. Pinned in Task 6.

---

### Task 0: Move the request wire snapshot out of `verbs.rs`

`crates/shep-core/src/protocol/request/verbs.rs` is exactly 1000 lines, so Task 2's new variant cannot land in it. `request_wire_snapshots` (from line 663) is the version-pinned fixture table for every request, the IR-35 contract rather than a test of code beside it. It moves to a module of its own. This is a pure move: no line of the test changes.

**Files:**
- Create: `crates/shep-core/src/protocol/request/request_wire.rs`
- Modify: `crates/shep-core/src/protocol/request/verbs.rs` (remove the test and the `envelope` helper if nothing else in the file uses it)
- Modify: `crates/shep-core/src/protocol/request/mod.rs` (add `#[cfg(test)] mod request_wire;`)
- Rename: `crates/shep-core/src/protocol/request/snapshots/shep_core__protocol__request__verbs__tests__request_wire_v11.snap` to the name insta derives from the new module path (`shep_core__protocol__request__request_wire__request_wire_v11.snap` if the test sits at the module's top level; run once and read the name insta asks for)

**Interfaces:** none.

- [ ] **Step 1:** Create `request_wire.rs` with a `//!` line ("The request wire, pinned: one row per variant, and a protocol bump when a row changes"), the imports the moved test needs, the `envelope` helper, and `request_wire_snapshots` moved byte for byte.
- [ ] **Step 2:** Remove them from `verbs.rs`. Keep `envelope` there too if another test in `verbs.rs` still calls it.
- [ ] **Step 3:** `git mv` the `.snap` file to the new name. Its contents do not change.
- [ ] **Step 4:** Run `cargo test -p shep-core --lib -- protocol::request`. Expected: PASS, and no `.snap.new` file anywhere (`git status --short` shows only the rename and the two edits).
- [ ] **Step 5:** `wc -l crates/shep-core/src/protocol/request/verbs.rs` is well under 1000.
- [ ] **Step 6:** Commit: `refactor(core): move the request wire snapshot into its own module`, body saying it is a pure move to make room in `verbs.rs` under IR-48.

---

### Task 1: The question grammar and the three channel messages (shep-channel)

**Files:**
- Create: `crates/shep-channel/src/question.rs` (grammar types, not behind the `client` feature)
- Create: `crates/shep-channel/src/ask.rs` (the `Shepherd` methods, behind `client`)
- Modify: `crates/shep-channel/src/wire.rs` (three variants)
- Modify: `crates/shep-channel/src/lib.rs` (modules, re-exports, crate doc bullet)
- Modify: `crates/shep-channel/src/dispatch.rs` (answer handler registry and resolution)
- Modify: `crates/shep-channel/src/serve.rs` (`reader_loop` outcomes only; the methods go in `ask.rs`, since `serve.rs` is already 874 lines)
- Modify: `crates/shep-channel/tests/wire_export.rs` and regenerate `crates/shep-channel/wire/channel.go`
- Modify: `crates/shep-channel/tests/fixtures.rs` (wire fixtures for the three kinds, following the `lamb-label` rows there)
- Modify, to keep the workspace compiling: `crates/shep-core/src/protocol/events.rs` (topic arms), `crates/shep-core/src/protocol/mod.rs` (re-exports), `crates/shep-daemon/src/supervisor/sheep.rs` (two match arms), and any other exhaustive match `cargo check --workspace` names (`crates/shep-daemon/src/fake/scripted_runner.rs` matches `ShepherdMessage`)

**Interfaces:**
- Produces, all re-exported from `shep_channel` and from `shep_core::protocol`:
  - `QuestionId` (newtype over `String`; `new(impl Into<String>) -> Result<Self, QuestionError>`, `as_str`, `MAX_CHARS = 32`; serde `try_from = "String", into = "String"`; `Display`)
  - `QuestionText` (same shape; `MAX_CHARS = 1000`)
  - `Takes` (`YesNo`, `Text`; serde `rename_all = "kebab-case"`; `#[non_exhaustive]` with a comment: a `choice` kind is anticipated)
  - `Takes::check(self, answer: &str, note: Option<&str>) -> Result<(), QuestionError>`
  - `check_via(&str) -> Result<(), QuestionError>` and `check_who(&str) -> Result<(), QuestionError>` (64 and 128 characters, no control character)
  - `Answer` struct, `#[non_exhaustive]`, pub fields `question: QuestionId`, `answer: String`, `note: Option<String>`, `via: Option<String>`, `who: Option<String>`, with `Answer::new(question, answer)` and `with_note`, `with_via`, `with_who` setters. The three `Option`s carry `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - `QuestionError`, `#[non_exhaustive]`, deriving `Debug, Clone, PartialEq, Eq`: `Empty { field: &'static str }`, `TooLong { field: &'static str, max: usize, chars: usize }`, `ControlCharacter { field: &'static str }`, `IdCharacter { found: char }`, `NotYesOrNo { found: String }`, `NoteOnText`. `field` is the wire key (`"question"`, `"text"`, `"answer"`, `"note"`, `"via"`, `"who"`).
  - `ChildMessage::Ask { question: QuestionId, text: QuestionText, takes: Takes }`, `ChildMessage::Withdraw { question: QuestionId }`, `ShepherdMessage::Answer(Answer)`
  - `Shepherd::ask(&self, question: QuestionId, text: QuestionText, takes: Takes) -> Result<(), ChannelError>`, `Shepherd::withdraw(&self, question: QuestionId) -> Result<(), ChannelError>`, `Shepherd::on_answer<H>(&self, handler: H) -> &Self where H: Fn(&Answer) + Send + Sync + 'static`

- [ ] **Step 1: Write the failing grammar tests** in `question.rs`'s `#[cfg(test)] mod tests`:

```rust
#[test]
fn a_question_id_takes_letters_digits_dot_underscore_and_dash() {
    assert!(QuestionId::new("koji-3.retry_2").is_ok());
    assert_eq!(QuestionId::new(""), Err(QuestionError::Empty { field: "question" }));
    assert_eq!(QuestionId::new("a b"), Err(QuestionError::IdCharacter { found: ' ' }));
    assert_eq!(QuestionId::new("ü"), Err(QuestionError::IdCharacter { found: 'ü' }));
    assert!(QuestionId::new("x".repeat(32)).is_ok());
    assert_eq!(
        QuestionId::new("x".repeat(33)),
        Err(QuestionError::TooLong { field: "question", max: 32, chars: 33 })
    );
}

#[test]
fn question_text_keeps_newlines_and_refuses_every_other_control_character() {
    assert!(QuestionText::new("Merge #12?\nCI is green.").is_ok());
    assert_eq!(QuestionText::new("a\rb"), Err(QuestionError::ControlCharacter { field: "text" }));
    assert_eq!(QuestionText::new("\u{1b}[2J"), Err(QuestionError::ControlCharacter { field: "text" }));
    assert_eq!(QuestionText::new(""), Err(QuestionError::Empty { field: "text" }));
    assert!(QuestionText::new("é".repeat(1000)).is_ok(), "counted in characters, not bytes");
    assert!(QuestionText::new("é".repeat(1001)).is_err());
}

#[test]
fn a_yes_no_question_takes_exactly_yes_or_no_and_an_optional_note() {
    assert_eq!(Takes::YesNo.check("yes", None), Ok(()));
    assert_eq!(Takes::YesNo.check("no", Some("rebase first")), Ok(()));
    assert_eq!(
        Takes::YesNo.check("Yes", None),
        Err(QuestionError::NotYesOrNo { found: "Yes".to_string() })
    );
    assert_eq!(
        Takes::YesNo.check("no", Some(&"n".repeat(501))),
        Err(QuestionError::TooLong { field: "note", max: 500, chars: 501 })
    );
    assert_eq!(
        Takes::YesNo.check("no", Some("a\nb")),
        Err(QuestionError::ControlCharacter { field: "note" })
    );
}

#[test]
fn a_text_question_takes_any_text_and_no_note() {
    assert_eq!(Takes::Text.check("call it kelpie-probe", None), Ok(()));
    assert_eq!(Takes::Text.check("two\nlines", None), Ok(()));
    assert_eq!(Takes::Text.check("", None), Err(QuestionError::Empty { field: "answer" }));
    assert_eq!(Takes::Text.check("x", Some("y")), Err(QuestionError::NoteOnText));
}

#[test]
fn via_and_who_are_short_and_carry_no_control_character() {
    assert!(check_via("discord").is_ok());
    assert!(check_via(&"v".repeat(65)).is_err());
    assert!(check_who(&"w".repeat(128)).is_ok());
    assert!(check_who("<@1>\n").is_err());
}

#[test]
fn a_question_off_the_wire_is_checked_like_one_built_in_rust() {
    let bad = r#"{"kind":"ask","question":"a b","text":"x","takes":"yes-no"}"#;
    assert!(serde_json::from_str::<crate::ChildMessage>(bad).is_err());
    let unknown = r#"{"kind":"ask","question":"q","text":"x","takes":"maybe"}"#;
    assert!(serde_json::from_str::<crate::ChildMessage>(unknown).is_err());
}

#[test]
fn every_error_names_its_field_in_plain_words() {
    assert_eq!(
        QuestionError::TooLong { field: "note", max: 500, chars: 501 }.to_string(),
        "a note holds at most 500 characters, and this one has 501"
    );
    assert_eq!(
        QuestionError::NotYesOrNo { found: "maybe".to_string() }.to_string(),
        "this question takes `yes` or `no`, not `maybe`"
    );
}
```

- [ ] **Step 2:** Run `cargo test -p shep-channel --lib -- question`. Expected: FAIL to compile (types do not exist).

- [ ] **Step 3: Implement `question.rs`.** One private checker does the character rules, so the six fields cannot drift apart:

```rust
fn check_text(field: &'static str, text: &str, max: usize, newline: bool) -> Result<(), QuestionError> {
    if text.is_empty() {
        return Err(QuestionError::Empty { field });
    }
    let chars = text.chars().count();
    if chars > max {
        return Err(QuestionError::TooLong { field, max, chars });
    }
    if text.chars().any(|c| c.is_control() && !(newline && c == '\n')) {
        return Err(QuestionError::ControlCharacter { field });
    }
    Ok(())
}
```

`QuestionId::new` checks emptiness and length through `check_text` (with `newline: false`), then refuses the first character outside `[A-Za-z0-9._-]` with `IdCharacter`. `Takes::check` for `YesNo` refuses anything but `yes`/`no`, then checks a note with `check_text("note", note, 500, false)`; for `Text` refuses any note with `NoteOnText`, then `check_text("answer", answer, 1000, true)`. `Display` for `QuestionError` uses `"a {field} ..."` wording as the test pins; `impl core::error::Error`. Model every newtype on `LambLabel` in `wire.rs` (`try_from`/`into` `String`, `as_str`, a `// wire format` comment). `Answer`'s `Debug` derives: nothing in it is a credential.

- [ ] **Step 4: Add the wire variants** to `wire.rs`, with fixture tests in its test module following the existing round-trip style:

```rust
#[test]
fn ask_withdraw_and_answer_wire_fixtures_round_trip() {
    let ask = r#"{"kind":"ask","question":"koji-3","text":"Merge #12?","takes":"yes-no"}"#;
    let withdraw = r#"{"kind":"withdraw","question":"koji-3"}"#;
    let answer = r#"{"kind":"answer","question":"koji-3","answer":"no","note":"rebase first","via":"discord","who":"<@81234>"}"#;
    let bare = r#"{"kind":"answer","question":"koji-3","answer":"yes"}"#;
    for line in [ask, withdraw] {
        let parsed: ChildMessage = serde_json::from_str(line).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), line);
    }
    for line in [answer, bare] {
        let parsed: ShepherdMessage = serde_json::from_str(line).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), line, "absent options stay absent");
    }
}
```

Run `cargo check --workspace --all-targets --all-features` and add every arm it asks for:
- `shep-core/src/protocol/events.rs`: `ChildMessage::Ask { .. } => "channel.ask"`, `ChildMessage::Withdraw { .. } => "channel.withdraw"`, plus rows in the topic test there.
- `shep-daemon/src/supervisor/sheep.rs`: two arms that do nothing yet but `tracing::debug!`. Task 4 replaces them; say so in neither code nor comment, just leave them minimal.
- Re-export the new types from `shep_core::protocol` beside `LambLabel` (`protocol/mod.rs:45`).

- [ ] **Step 5: Dispatch.** In `dispatch.rs`, add `answer: Option<Arc<AnswerFn>>` to `Dispatch` (`type AnswerFn = dyn Fn(&Answer) + Send + Sync`; extend the hand-written `Debug` with `.field("answer", &self.answer.is_some())` and update its exact-string test), `register_answer`, `Resolved::Answer { handler, answer }` and `Resolved::UnhandledAnswer(QuestionId)`, and `Outcome::UnhandledAnswer(QuestionId)` and `Outcome::AnswerFailed(String)`. `run` calls the handler under `catch_unwind` exactly as the shutdown arm does. Tests, beside the shutdown ones:

```rust
#[test]
fn an_answer_reaches_its_handler_and_sends_nothing_back() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let mut dispatch = Dispatch::default();
    dispatch.register_answer(Box::new(move |answer: &Answer| {
        sink.lock().unwrap().push((answer.question.to_string(), answer.answer.clone()));
    }));
    let answer = Answer::new(QuestionId::new("q1").unwrap(), "yes");
    assert!(matches!(dispatch.handle(ShepherdMessage::Answer(answer)), Outcome::Handled));
    assert_eq!(*seen.lock().unwrap(), [("q1".to_string(), "yes".to_string())]);
}

#[test]
fn an_answer_with_no_handler_is_reported_rather_than_dropped() {
    let answer = Answer::new(QuestionId::new("q1").unwrap(), "yes");
    match Dispatch::default().handle(ShepherdMessage::Answer(answer)) {
        Outcome::UnhandledAnswer(id) => assert_eq!(id.as_str(), "q1"),
        other => panic!("expected UnhandledAnswer, got {other:?}"),
    }
}
```

Add a panicking-handler test modelled on `a_panicking_shutdown_handler_is_reported_rather_than_taking_the_reader_down`.

- [ ] **Step 6: `reader_loop`** in `serve.rs` gains two arms: `Outcome::UnhandledAnswer(id)` warns `"answer to question {id} arrived with no on_answer handler; it is lost"`, and `Outcome::AnswerFailed(message)` warns `"answer handler panicked: {message}"`. Neither ends the loop. Add a reader-loop test feeding one `answer` line with no handler and asserting the injected `warn` saw that text once.

- [ ] **Step 7: `ask.rs`.** An `impl Shepherd` block with `ask`, `withdraw` (both `push_blocking`, as `label_lamb` does, and `Ok(())` with no channel) and `on_answer` (as `on_shutdown`). `Shepherd`'s `Inner` and `outbox` are private to `serve`, so either make the two fields `pub(crate)` or add a `pub(crate) fn push_blocking(&self, ChildMessage) -> Result<(), ChannelError>` on `Shepherd` in `serve.rs` and call that. Prefer the method. Each pub fn gets `# Errors` and one doctest in the `label_lamb` style:

```rust
/// ```
/// use shep_channel::{QuestionId, QuestionText, Takes};
///
/// let shepherd = shep_channel::serve();
/// shepherd.on_answer(|answer| println!("{} said {}", answer.question, answer.answer));
/// shepherd
///     .ask(
///         QuestionId::new("koji-3").unwrap(),
///         QuestionText::new("Merge #12 into main?").unwrap(),
///         Takes::YesNo,
///     )
///     .unwrap();
/// ```
```

Add one line to the crate doc's bullet list: questions are answered at most once, and an answer with no `on_answer` handler warns and is lost.

- [ ] **Step 8: Go export.** In `tests/wire_export.rs` add kinds `KindAsk` ("KindAsk is a question the app puts to the operator."), `KindWithdraw` ("KindWithdraw takes back a question the app asked."), `KindAnswer` ("KindAnswer is the operator's answer to one question."), samples carrying every optional field, and fields:

```rust
// CHILD_FIELDS gains:
Field { ident: "Question", ty: "*string", tag: "question,omitempty", kinds: &["ask", "withdraw"] },
Field { ident: "Text",     ty: "*string", tag: "text,omitempty",     kinds: &["ask"] },
Field { ident: "Takes",    ty: "*string", tag: "takes,omitempty",    kinds: &["ask"] },
// SHEPHERD_FIELDS gains:
Field { ident: "Question", ty: "*string", tag: "question,omitempty", kinds: &["answer"] },
Field { ident: "Answer",   ty: "*string", tag: "answer,omitempty",   kinds: &["answer"] },
Field { ident: "Note",     ty: "*string", tag: "note,omitempty",     kinds: &["answer"] },
Field { ident: "Via",      ty: "*string", tag: "via,omitempty",      kinds: &["answer"] },
Field { ident: "Who",      ty: "*string", tag: "who,omitempty",      kinds: &["answer"] },
```

Regenerate with `SHEP_CHANNEL_BLESS=1 cargo test -p shep-channel --test wire_export`, then run it again without the variable. Expected: PASS. Read the `channel.go` diff.

- [ ] **Step 9:** Run `cargo test -p shep-channel --all-features`, which includes doctests. Expected: PASS. Then `cargo check --workspace --all-targets --all-features`. Expected: clean.
- [ ] **Step 10:** Commit: `feat(channel)!: let a sheep ask the operator a question`. Body: the three kinds, the grammar, `on_answer`; breaking for Rust callers because `ChildMessage` and `ShepherdMessage` are exhaustive; an older shepherd logs `ask` as malformed and carries on; `CHANNEL_VERSION` does not move.

---

### Task 2: Questions on the client protocol (shep-core)

**Files:**
- Create: `crates/shep-core/src/protocol/request/question.rs` (`OpenQuestion`, `Settled`)
- Modify: `crates/shep-core/src/protocol/request/process.rs` (`ProcessInfo::questions`, builder)
- Modify: `crates/shep-core/src/protocol/request/verbs.rs` (`Request::Answer`)
- Modify: `crates/shep-core/src/protocol/request/response.rs` (`Response::Answered`)
- Modify: `crates/shep-core/src/protocol/request/request_wire.rs` (one row) and the reply snapshot in `response.rs` (one row)
- Modify: `crates/shep-core/src/protocol/events.rs` (`BusEvent::QuestionSettled`, topic, tests)
- Modify: `crates/shep-core/src/protocol/request/mod.rs`, `crates/shep-core/src/protocol/mod.rs` (re-exports)

**Interfaces:**
- Consumes: `QuestionId`, `QuestionText`, `Takes` from Task 1.
- Produces:
  - `OpenQuestion { pub question: QuestionId, pub text: QuestionText, pub takes: Takes, pub asked_at_ms: u64 }`, `#[non_exhaustive]`, with `OpenQuestion::new(question, text, takes, asked_at_ms)`. `asked_at_ms` is Unix milliseconds.
  - `Settled`, `#[serde(tag = "kind", rename_all = "snake_case")]`, `#[non_exhaustive]`: `Answered { via: Option<String>, who: Option<String> }` (both `skip_serializing_if = "Option::is_none"`, `default`), `Withdrawn`, `Gone`.
  - `ProcessInfo::questions: Option<Vec<OpenQuestion>>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`, `None` when nothing is open) and `ProcessInfoBuilder::questions(Option<Vec<OpenQuestion>>)`.
  - `Request::Answer { selector: SelectorSpec, question: String, answer: String, note: Option<String>, via: Option<String>, who: Option<String> }`. `question` is a `String`, not a `QuestionId`, so a malformed one reaches the shepherd and is refused there with `InvalidConfig` naming the rule, rather than failing the whole frame's decode.
  - `Response::Answered { id: u32, name: String, question: String }`.
  - `BusEvent::QuestionSettled { id: u32, name: String, question: QuestionId, settled: Settled, at_ms: u64 }`, topic `question.settled`.

- [ ] **Step 1: Failing tests.** In `question.rs`:

```rust
#[test]
fn settled_wire_shapes_are_pinned() {
    let answered = Settled::Answered { via: Some("discord".into()), who: None };
    assert_eq!(serde_json::to_string(&answered).unwrap(), r#"{"kind":"answered","via":"discord"}"#);
    assert_eq!(serde_json::to_string(&Settled::Withdrawn).unwrap(), r#"{"kind":"withdrawn"}"#);
    assert_eq!(serde_json::to_string(&Settled::Gone).unwrap(), r#"{"kind":"gone"}"#);
}
```

In `events.rs` tests: `QuestionSettled`'s topic is `question.settled`, and the event round-trips through JSON. In `process.rs` tests: a `ProcessInfo` with `questions: None` serializes with no `questions` key, and one with an open question round-trips. Add a `Request::Answer` row (with every option set) to `request_wire_snapshots` and a `Response::Answered` row to the reply snapshot table.

- [ ] **Step 2:** Run `cargo test -p shep-core --lib`. Expected: FAIL to compile.
- [ ] **Step 3:** Implement. Doc each new field and variant with the condition it describes, not its name. `Request::Answer`'s doc names the refusals (`NotFound`, `InvalidConfig`) as the spec lists them.
- [ ] **Step 4:** Run `cargo test -p shep-core --lib`. Expected: the two snapshot tests fail with `.snap.new` files. Read each pending diff with `cargo insta review` or by diffing the `.snap.new` against the `.snap`: the only change must be the one new row each. Accept only then. Run again. Expected: PASS.
- [ ] **Step 5:** `cargo check --workspace --all-targets --all-features`. Every new variant should hit an existing wildcard arm. Fix whatever it names.
- [ ] **Step 6:** Report the line counts of `process.rs`, `response.rs`, `events.rs` and `verbs.rs`, and for any past 500 that grew, say whether you split it and why.
- [ ] **Step 7:** Commit: `feat(core): carry open questions and answers on the client protocol`.

---

### Task 3: The shepherd's question store (shep-daemon, pure)

A store with no IO, so every rule in the spec is a unit test.

**Files:**
- Create: `crates/shep-daemon/src/supervisor/questions.rs`
- Modify: `crates/shep-daemon/src/supervisor/mod.rs` (`mod questions;`)

**Interfaces:**
- Consumes: `OpenQuestion`, `Settled` (Task 2); `QuestionId`, `QuestionText`, `Takes`, `QuestionError` (Task 1).
- Produces:

```rust
/// One process's open questions, and the last few it settled.
#[derive(Debug, Default, Clone)]
pub(super) struct Questions { /* open: Vec<OpenQuestion>, settled: VecDeque<(QuestionId, Settled)> */ }

pub(super) const MAX_OPEN: usize = 64;
pub(super) const MAX_SETTLED: usize = 64;

pub(super) enum Asked { Opened, Replaced, Full }

pub(super) enum Refusal {
    /// Not open; `Some` when it is among the remembered settled ones.
    NotOpen(Option<Settled>),
    /// The answer does not fit what the question takes.
    WrongForm(QuestionError),
}

impl Questions {
    pub(super) fn ask(&mut self, question: OpenQuestion) -> Asked;
    pub(super) fn withdraw(&mut self, id: &QuestionId) -> bool;
    /// Checks `answer` against the open question, and on success closes it
    /// as `Settled::Answered` and returns its id.
    pub(super) fn answer(&mut self, id: &str, answer: &str, note: Option<&str>, via: Option<&str>, who: Option<&str>) -> Result<QuestionId, Refusal>;
    pub(super) fn holds(&self, id: &str) -> bool;
    pub(super) fn open(&self) -> &[OpenQuestion];
    /// Empties the store as the process goes, returning the ids that were open.
    pub(super) fn close_all(&mut self) -> Vec<QuestionId>;
}
```

`answer` checks in this order: the id parses as a `QuestionId` (else `WrongForm` with that error); it is open (else `NotOpen` with the remembered settlement); `via` and `who` pass `check_via`/`check_who`; `takes.check(answer, note)`. A replaced question keeps its place and its `asked_at_ms` is the new one's. `withdraw` remembers `Settled::Withdrawn`; `close_all` remembers nothing, since nothing can ask about a process that is gone.

- [ ] **Step 1: Failing tests,** one scenario each (IR-34): opened then listed oldest first; asking an open id again replaces it in place (`Asked::Replaced`, same position, new text); the 65th distinct ask returns `Asked::Full` and the first 64 are untouched; withdraw of an open id returns `true` and a later answer gets `NotOpen(Some(Settled::Withdrawn))`; withdraw of an unknown id returns `false`; a right answer returns the id and closes it, and a second answer gets `NotOpen(Some(Settled::Answered { via, who }))` carrying the first one's `via`/`who`; `maybe` to a yes-no question is `WrongForm(NotYesOrNo)` and leaves it open; a note on a text question is `WrongForm(NoteOnText)` and leaves it open; an id with a space is `WrongForm(IdCharacter)`; a 65th settlement evicts the oldest remembered one, so it then answers `NotOpen(None)`; `close_all` returns the open ids in order and leaves `open()` empty.
- [ ] **Step 2:** Run `cargo test -p shep-daemon --lib -- supervisor::questions`. Expected: FAIL to compile.
- [ ] **Step 3:** Implement. `MAX_OPEN` and `MAX_SETTLED` carry a comment with their basis: the spec's numbers, no benchmark behind them.
- [ ] **Step 4:** Run the same command. Expected: PASS.
- [ ] **Step 5:** Commit: `feat(shep-daemon): a store for one process's open questions`.

---

### Task 4: Wire the store through the actor, the bus and `Request::Answer`

**Files:**
- Modify: `crates/shep-daemon/src/supervisor/sheep.rs` (the two arms from Task 1 forward to the actor)
- Modify: `crates/shep-daemon/src/supervisor/command.rs` (`Msg::Ask`, `Msg::Withdraw`, `Command::Answer`)
- Modify: `crates/shep-daemon/src/supervisor/slot.rs` (`questions: Questions`, in `SheepSlot::new`)
- Modify: `crates/shep-daemon/src/supervisor/actor_core.rs` (route the new messages and command)
- Create: `crates/shep-daemon/src/supervisor/actor_questions.rs` (the handlers, so `actor_actions.rs` at 493 lines does not grow)
- Modify: the three sites that clear `slot.to_child` (`actor_exit.rs:277`, `actor_lifecycle.rs:230` and `:290`): close the store and publish `gone` for each id
- Modify: `crates/shep-daemon/src/supervisor/actor_actions.rs` (`snapshot_all` fills `questions`)
- Modify: `crates/shep-daemon/src/supervisor/handle.rs` (`SupervisorHandle::answer`)
- Modify: `crates/shep-daemon/src/rpc/dispatch.rs` (`Request::Answer`)
- Test: `crates/shep-daemon/src/supervisor/tests/questions.rs` (new, registered in `tests/mod.rs`) and `crates/shep-daemon/src/rpc/tests/` beside `trigger_signal.rs`

**Interfaces:**
- Consumes: Task 3's `Questions`; Task 2's `Request::Answer`, `Response::Answered`, `BusEvent::QuestionSettled`, `ProcessInfoBuilder::questions`.
- Produces: `SupervisorHandle::answer(&self, selector: ProcessSelector, question: String, answer: String, note: Option<String>, via: Option<String>, who: Option<String>) -> Result<(u32, String), SupervisorError>`, returning the answered sheep's id and name. Add whatever `SupervisorError` variants the refusals need, each mapping in `rpc_error` to the code the spec names: `NotFound` for no sheep, no channel and not open (the message says how it closed when remembered); `InvalidConfig` for a wrong form, a selector that is not one sheep (`All`, `Regex`, `Fold`), and a name whose instances both hold the question.

Behaviour, all on the actor, none of it awaiting:

- `ChildMessage::Ask` in the pump becomes `Msg::Ask { id, root_pid: proc.pid(), question: OpenQuestion }`, stamped with `crate::now_ms()`. `Withdraw` becomes `Msg::Withdraw { id, root_pid, question }`. The actor drops either when the slot is gone or `slot.entry.pid != Some(root_pid)`, the way `LambLabel` is keyed.
- `Asked::Full` logs `tracing::warn!` with the sheep's name and the dropped id.
- A successful `withdraw` publishes `QuestionSettled { settled: Withdrawn }`.
- `Command::Answer`: resolve the selector over the slots; keep those holding the question (`Questions::holds`); none matched by the selector → `NotFound`; matched but no slot has an open channel (`slot.open_channel()`) → `NotFound` with "has no shepherd channel, so it has no questions"; matched slots exist but none holds it → `NotFound` from the first matched slot's `Refusal::NotOpen`; more than one holds it → `InvalidConfig` naming the ids; exactly one → `Questions::answer`, then `try_send` the `ShepherdMessage::Answer` on `open_channel()`. A `try_send` that fails means the process is going: answer `NotFound` and leave the question for the exit path to close as `gone`. On success publish `QuestionSettled { settled: Answered { via, who } }`.
- Each `to_child = None` site calls `slot.questions.close_all()` and publishes `gone` for every id, so a dog can edit what it sent.
- `snapshot_all` sets `questions` from each slot's `open()`, `None` when empty. `to_info` does not change: the store lives on the slot, not the entry.

- [ ] **Step 1: Failing actor tests** in `supervisor/tests/questions.rs`, using the scripted fake runner and a paused clock as `supervisor/tests/actions.rs` does for lamb labels and actions. One scenario each: an ask reaches the listing; an answer reaches the child's channel as `ShepherdMessage::Answer` with the `via`/`who` given and `question.settled` publishes `answered`; a second answer is `NotFound` naming the first's `via`; a withdraw empties the listing and publishes `withdrawn`; the process exiting empties the listing and publishes `gone` (Review Focus 1); an ask stamped with a stale `root_pid` after a respawn is dropped (Review Focus 2); two instances of one app holding `q1` refuse `shep answer web q1` with `InvalidConfig` naming both ids, and answering by id works (Review Focus 3); a sheep with no channel is `NotFound`; `maybe` to a yes-no question is `InvalidConfig` and the question stays open. Every await carries a forcing mechanism (IR-46).
- [ ] **Step 2: Failing RPC test** beside `rpc/tests/trigger_signal.rs`: `Request::Answer` round-trips to `Response::Answered`, and `SelectorSpec::All` is refused with `RpcErrorCode::InvalidConfig`.
- [ ] **Step 3:** Run `cargo test -p shep-daemon --lib --all-features -- --skip ::slow::`. Expected: FAIL.
- [ ] **Step 4:** Implement.
- [ ] **Step 5:** Run the same command. Expected: PASS.
- [ ] **Step 6:** Report line counts for every file touched that is past 500.
- [ ] **Step 7:** Commit: `feat(shep-daemon): hold a sheep's questions and deliver its answers`.

---

### Task 5: Carry open questions across a handover

**Files:**
- Modify: `crates/shep-daemon/src/handover/carried.rs` (`CarriedSheep::questions: Option<Vec<OpenQuestion>>`, `#[serde(default, skip_serializing_if = "Option::is_none")]`)
- Modify: the code that builds a `CarriedSheep` from a slot and the code that rebuilds a slot from one (search `CarriedSheep {` under `crates/shep-daemon/src/supervisor/handover.rs` and `crates/shep-daemon/src/handover/`)
- Test: `crates/shep-daemon/src/handover/blob/tests/compat.rs`, and a carry round-trip test wherever the existing carry tests sit

**Interfaces:**
- Consumes: Task 3's `Questions::open()` and `Questions::ask()`.

- [ ] **Step 1: Failing tests.** In `compat.rs`, following `a_blob_written_before_the_channel_was_carried_still_loads` exactly: a blob with the `questions` key removed loads with `questions == None`. Then a carry test: a slot with one open question, carried and adopted, lists it again and still answers it (Review Focus 4). The remembered settled questions are not carried, and the test asserts it: a question answered before the carry answers `NotFound` with no "how it closed" after it.
- [ ] **Step 2:** Run `cargo test -p shep-daemon --lib --all-features -- handover --skip ::slow::`. Expected: FAIL.
- [ ] **Step 3:** Implement. Write `None` rather than `Some(vec![])` for a sheep with nothing open, so a blob for a flock that never asked is byte-identical to today's.
- [ ] **Step 4:** Run the same command. Expected: PASS. Then `cargo check -p shep-daemon --all-targets --all-features --target x86_64-unknown-linux-gnu`, because the handover is unix-only code.
- [ ] **Step 5:** Commit: `feat(shep-daemon): carry open questions across a handover`.

---

### Task 6: `shep answer`, and questions in `describe`

**Files:**
- Modify: `crates/shep-cli/src/cli/sheep.rs` (`AnswerArgs`), `crates/shep-cli/src/cli/verbs.rs` (`Commands::Answer`), `crates/shep-cli/src/cli/mod.rs` (export)
- Modify: `crates/shep-cli/src/cli/help.rs` (`"answer"` in the "Talk to a sheep" group, line 39, and the rendered help text near line 75)
- Create: `crates/shep-cli/src/commands/answer.rs`, registered beside `commands/trigger.rs`
- Modify: `crates/shep-cli/src/dispatch.rs` (route the verb, as `Commands::Trigger` is at line 382)
- Create: `crates/shep-cli/src/output/rows/questions.rs` (the list table, and the `describe` section's rows), registered in `output/rows/mod.rs`
- Modify: `crates/shep-cli/src/output/described.rs` (a QUESTIONS section when any is open) and `crates/shep-cli/src/output/mod.rs` (render types)
- Modify: `web/scripts/generate-cli-reference.sh` (`answer` in `VERBS`, line 52)
- Test: unit tests in `commands/answer.rs` in `commands/trigger.rs`'s style, and an e2e test in `crates/shep-cli/tests/cli_e2e/dispatch.rs` beside `a_sheep_labels_one_of_its_lambs_over_its_channel`

**Interfaces:**
- Consumes: `Request::Answer`, `Response::Answered`, `ProcessInfo::questions`, `OpenQuestion`.
- Produces: `AnswerArgs { sheep: Option<String>, question: Option<String>, words: Vec<String> }`. clap: `question` requires `sheep`; `words` is `trailing_var_arg`, and is required when `question` is given. `shep answer web` alone is a usage error (exit 2) naming the missing question.

Behaviour:

- **No arguments:** send `ListFlock` and render every open question, flock order, then oldest first: columns `SHEEP` (name, plus `#id` when the app has more than one instance), `QUESTION`, `TAKES`, `ASKED` (age, in whatever format `flock`'s uptime column uses), `TEXT` (cut to 80 characters with `...` and `\n` escaped, the way `trigger`'s DETAIL is; find and reuse that helper in `output/rows/replies.rs` rather than copying it). An empty flock-wide list prints one line saying no sheep is waiting on an answer, and exits 0. `--format json` emits the envelope with `data` an array of `{ "id", "name", "question", "text", "takes", "asked_at_ms" }`.
- **With arguments:** parse `sheep` with `parse_selector_spec` as `trigger` does. Then send `ListFlock` and look for the question among the matched sheep's `questions`, to learn what it takes. Split `words` by that kind: for `yes-no` the answer is the first word and the note is the rest joined by single spaces (`None` when there is no rest); for `text`, or a kind this build does not know, or a question not found, the answer is every word joined by single spaces and the note is `None`. Then send `Request::Answer`. A question not found is still sent, so the shepherd's `NotFound` message (which says how it closed) is what the operator reads. `via` and `who` are `None` from the CLI. On success print one line, `answered <question> on <name>`; JSON carries `{ "id", "name", "question" }`.
- **describe:** a `Questions of <name>` section after the lambs section, only when `questions` is `Some`, with the same columns minus `SHEEP`. JSON already carries `questions` through `ProcessInfo`.

- [ ] **Step 1: Failing unit tests** in `commands/answer.rs`, with the split as a pure function of `(Option<Takes>, &[String])`: yes-no `["yes"]` gives no note; yes-no `["no", "rebase", "first"]` and `["no", "rebase first"]` give the same note; text `["no", "rebase", "first"]` gives the answer `no rebase first` and no note; text `["two words"]` gives `two words`; an unknown question (`None`) joins every word; the request body sent for `shep answer web q1 no rebase first` carries `SelectorSpec::Name("web")`, `question: "q1"`, `answer: "no"`, `note: Some("rebase first")`, `via: None`, `who: None`; a `NotFound` reply exits 3 and an `InvalidConfig` exits 4. Rendering tests in `rows/questions.rs`: the list, a cut long text, an escaped newline, the empty-flock line, and the JSON shape.
- [ ] **Step 2: Failing e2e test**, unix only, modelled on `a_sheep_labels_one_of_its_lambs_over_its_channel`: a shell sheep with `channel = true` writes `{"kind":"ask","question":"q1","text":"Ship it?","takes":"yes-no"}` to fd 3, then `read -r line <&3` and writes `$line` to a file in the temp dir. Poll `shep --format json answer` until `q1` is listed; run `shep answer asker q1 no rebase first`, assert success; poll the file until it holds the answer line and assert it parses to `answer` `no` with note `rebase first`; assert `shep answer asker q1 yes` now exits 3; `graceful_kill`.
- [ ] **Step 3:** Run `cargo test -p shep --lib --bins --all-features -- --skip ::slow::` for the unit tests. Expected: FAIL. (The e2e test runs in Step 5.)
- [ ] **Step 4:** Implement. `cli/help.rs` tests pin every visible verb to a help group and to the generator's `VERBS` list; both must pass.
- [ ] **Step 5:** Run `cargo test -p shep --all-features` (this is the integration tier, which is where the e2e test lives). Expected: PASS.
- [ ] **Step 6:** Report line counts for `described.rs`, `dispatch.rs`, `output/mod.rs` and anything else past 500 that grew, with the split decision.
- [ ] **Step 7:** Commit: `feat(cli): answer a sheep's question with shep answer`.

---

### Task 7: Docs and site

**Files:**
- Modify: `docs/shepherd-channel.md` (the two outbound and one inbound row in the wire tables, the Windows paragraph's list of shapes at line 116, a new section "Asking the operator" after "Naming your lambs", and the bus list in "Everything you write here is also public on the bus")
- Modify: `docs/specs/shep-v1.md` (§7's channel message tables and §9's verb list)
- Modify: `docs/decisions.md` (one entry, below the last, in the house format: heading, the decision, `**Why:**`, a `verified` line naming the files)
- Modify: `web/src/pages/docs/shepherd-channel.astro`, `web/src/pages/docs/talking-to-a-sheep.astro`, `web/src/pages/docs/json-output.astro`, and whichever other page `grep -rn "lamb-label\|trigger" web/src/pages/docs` shows lists the channel kinds or the verbs
- Regenerate: `web/src/pages/docs/cli.astro` (or whatever the generator writes) with `cargo build --release && ./web/scripts/generate-cli-reference.sh`

The new section in `docs/shepherd-channel.md` covers, in this order: the `ask` line and what each field takes (copy the limits from Global Constraints); answering with `shep answer`, and that a dog can answer too; the `answer` line the app receives, and that `via` and `who` are claims nobody checked; that a question closes when answered, withdrawn or when the process exits, so an app asks again at start; that a handover keeps them; the 64 limit and that the 65th is dropped; and that an answer with no `on_answer` handler is lost with a warning. In Rust: `Shepherd::ask`, `withdraw`, `on_answer`.

The decision log entry records the two calls the spec argues: the answer is its own message rather than a trigger, and ntfy is a dog of its own rather than a bark sink. Its `verified` line names `docs/brainstorming/specs/2026-10-05-ask-the-operator-design.md`, `crates/shep-channel/src/question.rs`, `crates/shep-daemon/src/supervisor/questions.rs` and `crates/shep-daemon/src/supervisor/actor_questions.rs`.

Prose rules: one idea per sentence, short sentences, no em dashes, plain words. Match the voice of the surrounding page. Run `humanizer` over each new section before committing.

- [ ] **Step 1:** Write the docs.
- [ ] **Step 2:** `cargo build --release` then `./web/scripts/generate-cli-reference.sh`. Read `git diff` of the generated page: `answer` is new and nothing else moved.
- [ ] **Step 3:** In order, from `web/`: `npm ci`, `npx astro check`, `npm run build`. Expected: all three exit 0. `npm run build` runs the prose word budgets, and a new section can trip one: shorten the prose, do not raise the budget, unless the budget file says a page's budget tracks its feature count (as `verify-prose-budget.ts` did for `lamb-label` in #638), in which case raise it by the size of the new section and say so in the commit.
- [ ] **Step 4:** Commit: `docs: a sheep asks the operator, and shep answer replies`.

---

## Task gate (after Task 7)

Run once, in this order, one cargo command at a time, capturing `$?` directly:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --document-private-items
cargo check -p shep-daemon --all-targets --all-features --target x86_64-unknown-linux-gnu
cargo check --workspace --all-targets --all-features --target x86_64-pc-windows-gnu
```

`cargo test` here is unfiltered, so the `::slow::` tests run too.
