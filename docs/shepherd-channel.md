# The shepherd channel — a contract for app authors

This is for anyone writing an app that runs under shep and wants to talk
back: send readiness, custom metrics, or answer a `shep trigger`. It is
language-agnostic — the channel is a plain file descriptor carrying JSON
text, nothing Rust-specific about it.

The wire shapes themselves are pinned in spec
[§7](specs/shep-v1.md#7-readiness--health) and
[§9](specs/shep-v1.md#9-cli-surface-sheep-native); this doc is the practical
walkthrough of the same contract, written for the app side rather than the
daemon side.

## Getting a channel

shep opens the channel — a socketpair, one end handed to your process as an
extra file descriptor — only when your app's Flockfile config asks for one.
Three fields open it, and any one of them is enough:

- `channel = true` — ask for it directly, with no other behavior implied.
- `wait_ready = true` — implies a channel, because it needs one to receive
  your `ready` message.
- `shutdown_with_message = true` — also implies a channel, for the same
  reason in the other direction.

Leave all three unset and your app gets no fd 3 at all. This is opt-in on
purpose: a channel is a socketpair plus two pump tasks running for the life
of the process, and shep would rather not pay that for an app that never
uses it.

**The file descriptor is fd 3**, and the daemon also exports its number as
the `SHEP_CHANNEL_FD` environment variable — read that instead of hardcoding
`3` if you want to be robust to it changing later.

**The descriptor is a normal blocking file descriptor.** A plain `read()`
on it parks your process until the daemon has something to say, exactly as
reading any other pipe would. You do not need an event loop, non-blocking
I/O, or polling to use it — a shell script doing `read -r line <&3` works.

The daemon also exports `SHEP_CHANNEL_VERSION`, currently `1`. It describes
the wire on fd 3 as this document defines it. shep cannot ask what your app
speaks, so this is not a negotiation — it is there so a defensive app can
notice a version it has never seen and say so, rather than failing to parse a
line with nothing to connect that failure to a protocol change.

**Rust and Go apps do not have to speak this by hand.** The `shep-channel`
crate and `github.com/shep-pm/shep-go/channel` each implement everything on
this page: discovering the descriptor, framing the JSON, and answering
messages the app does not handle itself. JavaScript and Python libraries
over the same contract are planned. Whatever language an app is written in,
this document is still the contract it has to hold to.

## Four apps that already do this

`examples/` has the same app four times, once per language, and every one
of them runs under a Flockfile in this repository:

| app | language | how it frames the wire |
|---|---|---|
| `examples/src/bin/chatty.rs` | Rust | the `shep-channel` crate |
| `examples/polyglot/go-chatty/` | Go | by hand, over shep's generated Go types |
| `examples/polyglot/node-chatty.js` | JavaScript | by hand |
| `examples/polyglot/python-chatty.py` | Python | by hand |

All four answer the same actions, so one trigger reads the same way against
any of them:

```text
$ shep trigger '*chatty' ping
ID  NAME           OUTCOME  DETAIL
20  chatty         replied  pong from rust pid=40076, up 7.1s, level info
10  go-chatty      replied  pong from go pid=40075, up 7.1s, level info
8   node-chatty    replied  pong from node pid=40077, up 7.1s, level info
9   python-chatty  replied  pong from python pid=40078, up 7.1s, level info
```

`ping` answers, `metric` sends a sample and says what it sent, and `level`
shows an app splitting its own `params`. Any other name gets the
unknown-action reply this document asks for. `shep stop` sends the shutdown
message instead of a signal, because all four set
`shutdown_with_message = true`.

The three hand-rolled ones carry both platform arms, and each takes one of
the two ways out of the deadlock above. go-chatty and python-chatty read
and write from a single loop, so nothing is ever parked while something
else wants to write. node-chatty is the overlapped case rather than the
single-threaded one: libuv drives a named pipe asynchronously, so its
reads and writes do not serialise against each other in the first place.
All four were run on macOS, and the three under `examples/polyglot/` were
run again on Windows against a real named pipe.

## On Windows: a named pipe, not fd 3

**Everything above this heading describes the unix tier.** Windows has no
fd-3 inheritance a child can rely on — `cmd.exe` has no `<&3` redirection,
and the mechanism shep uses on unix (`command-fds`, a `pre_exec` crate) does
not exist there. So the descriptor half of the contract is replaced, and
only that half:

- The daemon exports **`SHEP_CHANNEL_PIPE`**, whose value is a named-pipe
  path like `\\.\pipe\shep-channel-1234-0-9f3c1a2b4d5e6f708192a3b4c5d6e7f8`.
  **Your app opens it itself**, for reading and writing, exactly as it would
  open any file. That open is the whole difference. Read the name out of
  the environment and do not try to reconstruct it: the trailing hex is
  random per spawn. The channel is readable by other accounts on the same
  machine, so do not put anything on it you would not show them. Detail in
  [deferred.md](specs/deferred.md) if you run shep somewhere that matters.
- `SHEP_CHANNEL_VERSION` is exported as before.
- **`SHEP_CHANNEL_FD` is deliberately NOT set on Windows.** Branch on which
  variable is present rather than on the platform: an app that finds
  `SHEP_CHANNEL_PIPE` should open it, one that finds `SHEP_CHANNEL_FD`
  should use that descriptor, and one that finds neither was not given a
  channel.

**The wire format below is unchanged** — same newline-delimited JSON, same
`ready`/`metric`/`action-reply`/`lamb-label`/`ask`/`withdraw` outbound and
`shutdown`/`action`/`answer` inbound shapes, same correlation id. A named
pipe opened in byte mode is a blocking byte stream, so "read a line, parse
it, act on it" still describes reading it exactly.

### The one way that pipe is not a descriptor

**Read this before you write a reader thread. Getting it wrong deadlocks
your app, and nothing in the failure tells you that is what happened.** The shepherd creates a
single pipe instance and accepts once, so the handle you open is one kernel
file object however many times you duplicate it — and Windows serialises
every operation on a *synchronous* file object. A thread parked in
`ReadFile` waiting for a message holds that object for as long as it waits,
and a write from any other thread queues behind it.

Both sides then wait for each other. The shepherd sends nothing until it has
heard `ready`; your app cannot send `ready` while its own reader is parked.
`wait_ready` times out, and nothing anywhere says why. This is not
hypothetical — it is what `shep-channel` itself did on Windows until
2026-09-02, and it was invisible because the arm compiled and no test ran it.

Two ways out. Pick one before you write the reader:

- **Never park waiting for data.** `PeekNamedPipe` does not block for bytes
  to arrive; issue a `ReadFile` only for bytes it has just told you are
  there. This holds the file object for the length of a copy rather than the
  length of a wait, so the writer gets in between polls. It is what
  `shep-channel` does, polling every 20 ms. Note what it does not buy you:
  the handle is still synchronous, so a peek queues behind any operation
  already running on it, and Microsoft documents `PeekNamedPipe` as able to
  block in a multithreaded application for that reason. A writer parked on a
  full pipe buffer delays the next peek until the shepherd drains. That is a
  stall, not the deadlock above, because the shepherd reads and writes on
  independent tasks.
- **Open the pipe with `FILE_FLAG_OVERLAPPED`** and drive it
  asynchronously — Go's `go-winio`, .NET with `PipeOptions.Asynchronous`,
  or anything built on an IOCP loop. An overlapped handle is not serialised,
  so a read and a write overlap freely.

An app that only reads, or only writes, or does both from one thread, needs
neither. The hazard is specific to reading and writing at the same time,
which is also the shape almost every real app ends up in.

**This matters more on Windows than on unix**, and it is worth being blunt
about why. Windows has no way to deliver anything SIGTERM-shaped to another
process, so `shep stop` on an app that has NOT opted into this channel waits
out the app's whole `kill_timeout` and then terminates it. **The channel is
the only graceful shutdown available on that platform.** Set
`shutdown_with_message = true`, read the pipe, and act on `shutdown`.

## The wire format

Newline-delimited JSON, one complete message per line, in both directions.
Nothing else rides on this descriptor — no framing header, no length
prefix, just `{...}\n{...}\n...`. Read a line, parse it as JSON, act on it;
build a JSON object, append `\n`, write it.

### What you send (daemon reads this)

| Message | Meaning |
|---|---|
| `{"kind":"ready"}` | You are up and ready to serve. Only meaningful if `wait_ready = true`; the daemon is otherwise not waiting for it. |
| `{"kind":"metric","name":"<name>","value":<number>}` | A custom metric sample. Currently logged by the daemon at debug level and nothing more — no dog reads it yet. |
| `{"kind":"action-reply","action":"<name>","body":"<text>","id":<number>}` | Your answer to a triggered action. `action` names which one; `body` is free-form text and becomes what the operator sees. `id` is optional — echo the `id` from the `action` message you are answering and shep matches your reply to that exact request. |
| `{"kind":"lamb-label","pid":<number>,"label":"<text>"}` | Names one of your own child processes in `shep describe` and lookout. An empty `label` clears it. See [Naming your lambs](#naming-your-lambs). |
| `{"kind":"ask","question":"<id>","text":"<text>","takes":"yes-no"}` | Puts a question to the operator. `takes` is `yes-no` or `text`. See [Asking the operator](#asking-the-operator). |
| `{"kind":"withdraw","question":"<id>"}` | Takes a question back because you no longer need the answer. |

### What you receive (daemon writes this)

| Message | Meaning |
|---|---|
| `{"kind":"shutdown"}` | Sent instead of a stop signal when `shutdown_with_message = true`. Treat it as your cue to shut down gracefully; the daemon still escalates to `SIGKILL` after `kill_timeout` if you take too long. |
| `{"kind":"action","name":"<name>","id":<number>}`, optionally with `"params":"<text>"` | An operator ran `shep trigger <selector> <name> [params]` against you. `params` is present only when the operator supplied one; `id` is always present — echo it on your reply. |
| `{"kind":"answer","question":"<id>","answer":"<text>"}`, optionally with `note`, `via` and `who` | The operator answered a question you asked. See [Asking the operator](#asking-the-operator). |

The Go spelling of both shapes is generated from the Rust enums above and
committed at `crates/shep-channel/wire/channel.go`. It is the same file
`github.com/shep-pm/shep-go/channel` ships as `channel/wire.go`. Copy it
rather than retyping it: every optional field is a pointer, because Go's
`omitempty` on a plain value drops a metric of zero and an id of zero.

## Custom actions — the part most worth reading closely

`shep trigger <selector> <action> [params]` is how an operator reaches a
running app directly, for whatever your app defines an "action" to mean:
force a GC, dump internal state, flip a log level, whatever you want a
running process to be able to do without restarting it.

**The action name is entirely yours. shep never validates it, never keeps
a registry of known actions, and never inspects `params`.** Whatever the
operator typed after `shep trigger <selector>` is sent to you verbatim, for
you to recognize or refuse. There is no way to ask the daemon "what actions
does this app support" — that documentation lives with your app, not with
shep.

**Reply even to an action name you don't recognize.** This is the one rule
that actually matters for a good operator experience. From the daemon's
side, an app that is thinking hard about a slow action and an app that has
no idea what it was just asked are indistinguishable — both are silence.
The only thing that tells them apart is `action_timeout` (configurable per
app, default 3s, capped at 58s) running out. If you silently ignore
messages you don't understand, an operator who fat-fingers an action name
waits out the full timeout for nothing. Send back something like:

```json
{"kind":"action-reply","action":"reload-config","body":"unknown action: reload-config"}
```

and they find out immediately instead.

**The reply body is what the operator actually sees.** `shep trigger`
prints one row per matched sheep; a `Replied` row's `DETAIL` column is your
`body`, and `--format json` carries it whole and untouched — full length,
real newlines, byte-for-byte what you sent. The table view is the only
place it is ever altered, and only for display: capped at 80 characters
with a trailing `...` when cut, and embedded `\n`/`\r` shown as the two-
character escapes `\n`/`\r` so one long or multi-line reply cannot desync
the table's columns. Neither limit exists on the wire or in JSON output —
they are rendering choices in the CLI, not something shep asks you to
respect when you write `body`.

**Echo the `id`, and your reply is matched exactly.** Every `action` message
carries an `id` — an opaque number, unique for the life of the daemon. Put it
back on your `action-reply` as `id` and shep hands your answer to that exact
request. Do that and everything below stops applying to you.

**If you don't echo it, replies are matched by action name and by order.**
That is the fallback, and it is what every app written before `id` existed
gets. shep matches your `action-reply` to a waiting trigger by name, and if
you have two of the same action outstanding, by the order you wrote them. If
you reply to an action after its trigger has already timed out, that late
reply settles the debt for that one timeout rather than being handed to
whatever triggered the same action name next — but that protection covers
exactly one stray reply per timeout, and it has a sharp edge: while a debt is
outstanding, an unstamped reply to a *live* trigger of the same name is
consumed as the debt payment, and the live trigger reports `timed_out` even
though you answered it promptly. Echoing `id` is how you make that
impossible. Failing that, don't sit on a reply, and don't send more than one
per action you were asked.

**No shepherd channel is not a silent failure.** An app configured with
none of `channel`/`wait_ready`/`shutdown_with_message` still gets a row
back from `shep trigger` — `no_channel`, naming exactly which config field
would have opened one. A reload drainee (the old instance mid-swap-out)
gets `skipped` instead of a wait, because an answer from the process on its
way out would be worse than none. Neither of these costs the operator a
timeout; both are refused immediately.

**Multi-argument `params` has no defined quoting today.** `params` is one
opaque string — whatever text followed the action name on the `shep
trigger` command line, passed to you exactly as given. If your action needs
more than one value, you own the grammar: put your own delimiter or your
own JSON inside that one string. shep will never parse it for you, and
there is currently no shep-level convention for how a multi-value `params`
string should be split — that is a decision your app makes for its own
actions, documented wherever you document them.

## Naming your lambs

`shep describe` lists your app's child processes, its lambs, as a pid and
an executable name. It never shows their command lines, which carry
credentials. So a worker pool of four `python` children reads as four
identical rows. Tell shep which is which:

```json
{"kind":"lamb-label","pid":4312,"label":"worker 1"}
```

`describe` then shows a LABEL column beside NAME, `--format json` carries
the label as `lambs[].label`, and lookout's lamb line reads
`4312 python (worker 1)`. Send the message again to replace a label, or
send an empty `label` to clear it.

- **The label is at most 64 characters and holds no control character.**
  A line that breaks either rule is dropped as a malformed frame, and the
  shepherd logs a warning. There is no reply either way.
- **Only your own tree counts.** A label is shown only while its pid is a
  parent-pid descendant of your process. A pid outside it is never shown.
- **A label goes when its lamb does.** Every 15 seconds the shepherd drops
  labels whose pid has left your tree. A pid the OS hands to a new
  descendant inside that window inherits the old label until then.
- **Labels live in the shepherd's memory.** A restart of your app, of the
  shepherd, or a `shep daemon reload` forgets them. Send them again when
  you spawn the lamb. Each process holds at most 256; past that, the oldest
  goes.

A shepherd that predates this message logs it as malformed and carries on,
so sending one to an older shep costs nothing but the warning.

In Rust, `shep_channel::Shepherd::label_lamb(pid, LambLabel::new("worker 1")?)`
checks the grammar before anything is sent.

## Asking the operator

An app that needs a person to decide something can ask, and carry on while
it waits. The shepherd holds the question, shows it to the operator, and
hands the answer back on the same descriptor.

```json
{"kind":"ask","question":"koji-3","text":"Merge #12 into main?","takes":"yes-no"}
```

- **`question`** is your own id for it: 1 to 32 characters from
  `A-Z`, `a-z`, `0-9`, `.`, `_` and `-`, so an operator can type it. Ask
  again with an id that is still open and the text and kind are replaced.
- **`text`** is at most 1000 characters. Newlines are fine. Any other
  control character is not.
- **`takes`** is `yes-no` or `text`.

A line that breaks a rule is dropped as a malformed frame and the shepherd
logs a warning. There is no reply either way.

The question shows up in `shep describe` in a section titled "Questions
of <name> (id N)", in `--format json` as `questions[]` (only when the sheep
has one open), and in `shep answer` with no arguments, which lists every
open question in the flock. The operator answers with the verb:

```text
$ shep answer
SHEEP  QUESTION  TAKES   ASKED  TEXT
asker  q1        yes-no  2s     Ship it?\nline two

$ shep answer asker q1 no rebase first
answered q1 on asker
```

The words after the question are joined with spaces. For a `yes-no`
question the first word is the answer and the rest is the note. For a
`text` question every word is the answer. A dog can answer too: it sends the
same request over the client socket, and fills in `via` and `who`.

What you receive:

```json
{"kind":"answer","question":"koji-3","answer":"no","note":"rebase first"}
```

- **`answer`** for `yes-no` is exactly `yes` or `no`. It may carry a `note`
  of at most 500 characters. For `text` it is the answer itself, 1 to 1000
  characters with the same control-character rule as `text`, and it carries
  no note.
- **`via`** names the dog that delivered the answer. It is absent when the
  operator answered at the socket or with `shep answer`. At most 64
  characters.
- **`who`** is what that dog says about the person. At most 128 characters.

`via` and `who` are claims nobody checked. shep passes them through unread,
and any client on the socket can write them. Treat them as a label for a log
line, not as proof of who said yes. Proving that is the delivering dog's
job.

The first answer wins. A second one is refused, and the message says how the
question closed. A question closes when it is answered, when you withdraw
it, or when your process exits. A restart forgets your open questions, so
ask again at start. A `shep daemon reload` keeps them: your channel crosses
it, you are still waiting, and you have no reason to ask twice.

The shepherd holds 64 open questions for each process. The 65th is dropped
and logged. It does not push an older one out, because losing a question
somebody is about to answer is worse than refusing a new one.

An `answer` is delivered once and you owe no reply. If the answer reaches a
sheep that is not reading its channel, the question stays open and the
operator is told it was not delivered. If your app has no `on_answer`
handler, `shep-channel` warns on stderr and the answer is lost.

In Rust, `QuestionId::new` and `QuestionText::new` check the grammar before
anything is sent. Then `Shepherd::ask(id, text, Takes::YesNo)` and
`Shepherd::withdraw(id)` send, and `Shepherd::on_answer(|answer| ...)`
registers the handler. The handler runs on the reader thread, so a slow one
delays the next message.

An older shepherd logs `ask` and `withdraw` as malformed and carries on, so
asking one costs the warning and no answer will come.

## Finish writing before you exit

`shutdown` is the one message that asks you to end the process, which makes
it the one place a reply you have already written can still be lost. If a
library writes for you on another thread, or your app buffers its own
output, exiting from the shutdown path drops whatever had not reached the
descriptor yet. The operator sees a timeout instead of your answer.

An app that writes straight to the descriptor has nothing to do here: a
`write` that returned means the bytes are with the kernel, and shep gets
them whether or not your process is still alive. `shep-channel` queues
instead, so it has `Shepherd::flush`, which takes the time you are willing
to give it. Keep that well inside `kill_timeout`, which shep is counting
down while you wait.

## Everything you write here is also public on the bus

Every message you send on fd 3 (`ready`, `metric`, `action-reply`,
`lamb-label`, `ask`, `withdraw`) is republished on the daemon's event bus
under `channel.*` (`channel.ready`, `channel.metric`,
`channel.action_reply`, `channel.lamb_label`, `channel.ask`,
`channel.withdraw`), as its own topic alongside `process.*` and the log topics. Anyone subscribed to
`channel.*` sees it, not just the operator who happened to send the
`trigger` you were answering.

This is not a security warning — nothing on this wire is a credential,
which is exactly why the event carries your message whole rather than a
redacted stand-in for it. It is a reminder that a `body` you write for one
operator's terminal is also a `body` a dashboard, a log aggregator, or
another dog entirely might render. Put in it what you would be comfortable
with any subscriber seeing, not just the one who asked.

## Summary for the impatient

- Ask for a channel with `channel = true` (or get one for free from
  `wait_ready` / `shutdown_with_message`).
- Read `SHEP_CHANNEL_FD` on unix or `SHEP_CHANNEL_PIPE` on Windows (exactly
  one is ever set), open it, read/write newline-delimited JSON.
- A plain blocking read works — no event loop required. On Windows that
  holds only while nothing else writes; if you read and write from two
  threads, read the pipe section above first.
- Reply to every `action` message you receive, even ones you don't
  recognize, and reply exactly once, promptly.
- Echo the `id` from the action on your reply — one field, and it is what
  makes a slow action's answer land on the right trigger.
- What you put in `body` is what the operator reads back.
- To ask the operator something, send `ask` with your own id. The answer comes
  back as an `answer` message, and `shep answer` is how the operator sends it.
