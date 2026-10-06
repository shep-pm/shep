# Design: a sheep asks the operator a question

Status: designed 2026-10-05, not yet implemented. Resolves #689.

## The problem

A sheep can send readiness, metrics and replies to triggers, but it cannot
ask a person something and wait for the answer. shep-kelpie needs to for
every decision only the maintainer makes (merge this, answer a worker's
question). Today each kelpie runner carries its own ntfy and Discord
delivery, polls the ntfy topic for replies every 15 seconds, and checks an
authenticator code on each reply.

The fix is a question shep holds for the sheep: the operator sees it in
`shep describe`, lookout and a dog's sinks, answers it from any of them, and
shep hands the answer back over the channel.

## What already exists

Established from the code, not assumed.

- **Every child message is republished on the bus.** `sheep.rs`'s channel
  pump sends `BusEvent::Channel` before acting on the message, and
  `BusEvent::topic` is a total match over `ChildMessage`, so a new kind
  fails to compile until its topic is chosen. `channel.ask` and
  `channel.withdraw` come with no extra wiring.
- **`ChildMessage` and `ShepherdMessage` are exhaustive.** A new variant
  breaks Rust callers, as `lamb-label` did (#638, `feat(channel)!`). An
  older shepherd logs an unknown kind as a malformed frame and carries on.
- **`Request`, `Response` and `BusEvent` are `#[non_exhaustive]`.**
  `PROTOCOL_VERSION`'s own doc: new variants there keep the version. It
  stays 11 and `MIN_SUPPORTED` stays 8. An older shepherd answers an unknown
  request with `RpcErrorCode::Unsupported`.
- **The channel survives a handover.** `CarriedFds::channel` carries the
  daemon's end across `shep daemon reload`, so the sheep keeps talking to
  the successor. Anything the shepherd holds for that sheep in memory is
  lost unless the blob carries it. `CarriedSheep` takes optional keys, and
  `handover/blob/tests/compat.rs` pins each one's absence.
- **An action reaches a child through the slot's open channel.**
  `actor_actions.rs` takes `slot.open_channel()` and sends a
  `ShepherdMessage` on it. An answer goes the same way.
- **A dog's `Hello::dog_name` is a claim.** `server/conn_protocol.rs`
  records it and checks it against nothing, so any client on the socket
  can name itself a dog. The socket is the trust boundary: a peer that
  reaches it can already `shep stop` the flock.
- **Every listing is built in one place.** `Actor::snapshot_all` answers
  both `ListFlock` and `Describe`, so a field it fills reaches both.
- **bark is one-way.** Its sinks (`discord`, `slack`, `json`) each make one
  HTTP POST per firing. Nothing in bark reads anything back.
- **shep-discord has a gateway bot.** It already serves slash commands and
  buttons, so it can take a button click as an answer, and Discord tells it
  which user clicked.
- **Kelpie's ntfy path is stateful.** `src/runner/replies.rs` polls the
  topic, `src/totp.rs` checks an RFC 6238 code at the end of each reply,
  and `src/totp/answers.rs` records spent steps and failures on disk,
  locking replies off after five wrong codes. Its question kinds are an
  answer (free text) and a yes, or a no with a note.

## The wire

Two messages from the sheep:

```json
{"kind":"ask","question":"koji-3","text":"Merge #12 into main?","takes":"yes-no"}
{"kind":"withdraw","question":"koji-3"}
```

One message to the sheep:

```json
{"kind":"answer","question":"koji-3","answer":"no","note":"rebase first","via":"discord","who":"<@81234>"}
```

- **`question`** is the question's id, the app's own: 1 to 32 characters
  from `[A-Za-z0-9._-]`, so it can be typed on a command line and inside an
  ntfy reply. Asking again with an id that is still open replaces that
  question's text and kind. The key is `question` and not `id` on all three
  messages: shep-go's `ChildMessage` is one flat struct whose `ID` is
  `action-reply`'s number, and a string under the same key would not decode.
- **`text`** is at most 1000 characters. Newlines are allowed, any other
  control character is refused. 1000 leaves a Discord message (2000) room
  for the framing a dog adds.
- **`takes`** is `yes-no` or `text`.
- **`answer`** for `yes-no` is exactly `yes` or `no`, and may carry a
  `note` of at most 500 characters, kelpie's `no <note>`. For `text` it is
  the answer itself, 1 to 1000 characters with the same control-character
  rule as `text`, and carries no note.
- **`via`** names the dog that delivered the answer, and is absent when an
  operator answered at the socket directly (the CLI or lookout). At most 64
  characters.
- **`who`** is what the dog says about the person, at most 128 characters.
- **shep vouches for neither.** Both are what the answering client said,
  passed through unread. Proof of who answered is the delivering dog's
  job, which is why a dog writes `who` and not shep.

A line that breaks any of these rules is dropped as a malformed frame and
logged, as `lamb-label` is. The sheep gets no reply.

In Rust, `shep_channel::Question::new(id, text, Takes)` and
`QuestionId::new` check the grammar before anything is sent, the way
`LambLabel::new` does. `Shepherd::ask(Question)` and
`Shepherd::withdraw(&QuestionId)` send, and `on_answer` handles the answer.

## The shepherd

- **Open questions are kept per sheep process, in memory.** 64 at most. A
  65th is dropped and logged rather than evicting an older one: losing a
  question somebody is about to answer is worse than refusing a new one.
- **A question closes when it is answered, when the sheep withdraws it, or
  when its process exits.** A restart forgets them, as it forgets lamb
  labels, so the app asks again at start.
- **Open questions ride the handover blob.** The sheep's channel crosses
  `shep daemon reload`, so the sheep is still waiting and has no reason to
  ask again. `CarriedSheep` gains an optional `questions` key, absent
  loading as none.
- **The first answer wins.** Answers go through the sheep's actor one at a
  time. A second answer finds the question closed, and is told how: the
  shepherd also remembers the last 64 questions settled on that process,
  in memory, and never carries them across a handover.
- **`answered` means handed to the channel writer.** The sheep owes no reply.
- **One bus event of shep's own,** `question.settled`, carrying the sheep's
  id and name, the question id, and how it closed (`Settled`): `answered`
  with `via` and `who`, `withdrawn`, or `gone` (the process exited). A dog edits the message
  it already sent when it sees one. `channel.ask` and `channel.withdraw`
  come from the republish.

## The client protocol and the CLI

- **`Request::Answer { sheep, question, answer, note, via, who }`** names one
  sheep by id, name or instance, never `all`, a regex or a fold. Success is
  `Response::Answered { id, name, question }`. Every refusal is an existing
  `RpcErrorCode`, so `shep answer` needs no new exit code:
  - `NotFound` (exit 3): no sheep matches; it has no channel, so it cannot
    have asked; or the question is not open on its running process. The
    message says how it closed when it is among the remembered ones
    (answered, naming `via` and `who`, or withdrawn).
  - `InvalidConfig` (exit 4): `maybe` to a `yes-no` question, a note on a
    `text` one, a value outside the grammar, a selector that is not one
    sheep, or a name whose instances both hold that question open (the
    message names their ids, to answer by id).
- **`ProcessInfo` carries `questions`,** the open ones oldest first, `None`
  when there are none. `ListFlock` and `Describe` both fill it, so a dog
  reads every open question with the `ListFlock` it already makes on
  connect and after a handover, then follows the bus. No request of its own.
- **`shep answer <sheep> <question> <answer> [note...]`** is a new verb.
  The words after `<question>` are joined with spaces: for a `yes-no`
  question the first word is the answer and the rest the note, for a
  `text` question all of them are the answer.
- **`shep answer` with no arguments lists every open question in the
  flock**: SHEEP, QUESTION, TAKES, ASKED, TEXT, the text cut and escaped as
  `trigger`'s DETAIL is.
- **`describe` gets a QUESTIONS section** only when the sheep has one open,
  and `--format json` a `questions[]` array only when it is not empty.
  Additive, so `SCHEMA_VERSION` does not move.
- **Answering is a control action,** like `trigger` and `stop`: reaching the
  socket is enough for the CLI.

## Lookout

- The flock row marks a sheep with open questions.
- The detail pane lists them.
- `a` on the detail pane opens an answer prompt, behind `--allow-control`.

## Dogs

- **bark delivers and does not listen.** A new rule `on = "question"`
  posts the question with the `shep answer ...` line that answers it, once
  per sheep and question. It joins the built-in default rules, so an
  operator with no rules configured still hears questions.
- **Answers come from dogs with a way back in.** shep-discord gets Yes and
  No buttons and a modal for text, with an allowlist of Discord user ids as
  its proof of who answered. ntfy becomes a dog of its own, `shep-ntfy`,
  carrying kelpie's poll, TOTP and lockout code.
- **Why ntfy is not a bark sink:** kelpie's path keeps state on disk
  (spent steps, failure counts, a lock) and owns an authenticator secret.
  bark is a rule engine over stateless POSTs, and giving it a TOTP secret
  makes the alerting dog the one holding the credential that approves a
  merge.

## Not a trigger

The issue proposed delivering the answer as a trigger. It is a message of
its own instead:

- shep promises never to read or validate an action name. Recognising one
  as an answer breaks that promise.
- A trigger fans out over a selector and waits under a timeout for a reply.
  An answer goes to one question of one sheep, and the sheep owes nothing
  back.
- shep has to close the question in the same step that delivers the answer,
  which it cannot do for an action it does not read.

## Slices

Each is complete on its own:

1. **shep:** the wire, the shepherd's store and handover, the bus event,
   `Request::Answer`, `ProcessInfo::questions`, `shep answer`, `describe`
   and JSON, docs and site.
2. **shep:** lookout's marker, list and answer prompt.
3. **shep:** bark's `question` rule.
4. **Follow-up issues:** shep-discord's buttons and allowlist, a new
   `shep-ntfy` repository with kelpie's TOTP path, and shep-kelpie dropping
   its own delivery for the channel.

## Not decided here

- A `choice` kind (merge, close, wait) is a third `takes`, later.
- A question that expires inside shep. The app withdraws one it no longer
  wants.
